//! 置信度门控（升级计划 §12 / §27.4；`rig` feature）。
//!
//! Rig 0.44 的官方 policy hook 是 [`rig_core::serve::Intercept`]：
//! `after` 阶段拿到 handler 的答案（`Outcome::Completion`），在**工具
//! materialise 之前**给出 `Verdict`——低置信度的回复被 `Replace(Err(Denied))`
//! 拒绝，调用不执行（§12.3「禁止伪 gate」：拒绝点在 tool materialise 之前，
//! 保证 `low confidence → zero tool execution`——e2e 验收看 effect 计数，
//! §27.4）。
//!
//! 门限来源（§26.2 落地）：置信度阈值属于 **Rig 执行策略**，不属
//! `NeedleSecurityPolicy`；本 gate 在 handler 注册时拿到具体阈值
//! （per-model，宿主决定全局/per-agent），**不依赖 legacy 的
//! `EscalationPolicy`**（⑦ legacy 冻结纪律：新代码不依赖 legacy）。
//! bevy_needle 的 legacy 管线保留它自己的 gate（temporary migration
//! fallback，§26.4），两边不是同一个 gate。
//!
//! 门控只约束「有 function_calls 的轮」——respond 轮不参与（§27.4：
//! `final response 不得被 confidence gate 阻塞`）。
//!
//! 线程面：`Intercept` 是 `WasmCompatSend + Sync`；阈值是构造期字段。

use rig_core::completion::CompletionResponse;
use rig_core::effect::{EffectId, EffectKind, Outcome};
use rig_core::error::ErrorReport;
use rig_core::serve::{Decision, Intercept, Verdict};

/// 门控层名（记录/回放语义）。
pub const GATE_LAYER: &str = "needle-confidence";

/// 置信度门控 Intercept。
///
/// `after`：`Outcome::Completion` 且 `raw.confidence` 存在且低于门限
/// → `Verdict::Replace(Err(Denied))`；否则 `Keep`。
pub struct ConfidenceGate {
    threshold: f64,
}

impl ConfidenceGate {
    /// 构造（阈值来自宿主；门控覆盖范围 = 挂了本 gate 的 handler）。
    pub fn new(threshold: f64) -> Self {
        Self { threshold }
    }

    /// 门限（诊断面）。
    pub fn threshold(&self) -> f64 {
        self.threshold
    }
}

impl Intercept for ConfidenceGate {
    fn name(&self) -> String {
        GATE_LAYER.to_string()
    }

    async fn before(&self, _id: EffectId, _kind: &EffectKind) -> Decision {
        Decision::Proceed
    }

    async fn after(
        &self,
        _id: EffectId,
        _kind: &EffectKind,
        outcome: &Result<Outcome, ErrorReport>,
    ) -> Verdict {
        let Ok(Outcome::Completion(response)) = outcome else {
            return Verdict::Keep;
        };
        let Some(confidence) = confidence_of(response) else {
            return Verdict::Keep; // 无 confidence（或 respond 轮）→ 不门控
        };
        if response.tool_calls().next().is_none() {
            return Verdict::Keep; // 无调用 → 门控只约束工具轮（§27.4 第三行）
        }
        if confidence < self.threshold {
            Verdict::Replace(Err(ErrorReport::new(
                rig_core::error::ErrorKind::Denied,
                format!(
                    "置信度 {confidence:.2} 低于门限 {:.2}，调用未执行（升级契约）",
                    self.threshold
                ),
            )))
        } else {
            Verdict::Keep
        }
    }
}

/// 信封 `raw` 里的 confidence（§15 的读取面）。
fn confidence_of(response: &CompletionResponse) -> Option<f64> {
    response
        .raw
        .get("confidence")
        .and_then(serde_json::Value::as_f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::message::{AssistantContent, ToolName};
    use serde_json::json;

    fn gate(threshold: f64) -> ConfidenceGate {
        ConfidenceGate::new(threshold)
    }

    fn completion(choice: Vec<AssistantContent>, confidence: Option<f64>) -> Outcome {
        let origin = rig_core::message::Origin::new("needle.complete", "needle", "needle3");
        let response = CompletionResponse::new(
            choice,
            rig_core::completion::Usage::default(),
            origin,
            json!({ "confidence": confidence }),
        );
        Outcome::Completion(response)
    }

    fn call_part(name: &str) -> AssistantContent {
        AssistantContent::tool_call("call-1", ToolName::new(name).expect("name"), json!({}))
    }

    #[test]
    fn low_confidence_call_is_denied_before_materialisation() {
        let gate = gate(0.85);
        let kind = EffectKind::Completion {
            request: CompletionRequest::from(vec![Message::user("hi")]),
            stream: false,
        };
        let outcome = Ok(completion(vec![call_part("set_volume")], Some(0.4)));
        let fut = gate.after(EffectId::from_raw(1), &kind, &outcome);
        assert!(matches!(futures_now(fut), Verdict::Replace(Err(_))));
    }

    #[test]
    fn high_confidence_call_is_kept() {
        let gate = gate(0.85);
        let kind = EffectKind::Completion {
            request: CompletionRequest::from(vec![Message::user("hi")]),
            stream: false,
        };
        let outcome = Ok(completion(vec![call_part("set_volume")], Some(0.94)));
        let fut = gate.after(EffectId::from_raw(1), &kind, &outcome);
        assert!(matches!(futures_now(fut), Verdict::Keep));
    }

    #[test]
    fn respond_turn_is_never_gated() {
        let gate = gate(0.85);
        let kind = EffectKind::Completion {
            request: CompletionRequest::from(vec![Message::user("hi")]),
            stream: false,
        };
        let outcome = Ok(completion(
            vec![AssistantContent::text("all done")],
            Some(0.01),
        ));
        let fut = gate.after(EffectId::from_raw(1), &kind, &outcome);
        assert!(matches!(futures_now(fut), Verdict::Keep));
    }

    #[test]
    fn missing_confidence_is_never_gated() {
        let gate = gate(0.85);
        let kind = EffectKind::Completion {
            request: CompletionRequest::from(vec![Message::user("hi")]),
            stream: false,
        };
        let outcome = Ok(completion(vec![call_part("set_volume")], None));
        let fut = gate.after(EffectId::from_raw(1), &kind, &outcome);
        assert!(matches!(futures_now(fut), Verdict::Keep));
    }

    /// 同步推进 async 测试体（无执行器依赖；after 是 async fn）。
    fn futures_now<F: std::future::Future>(fut: F) -> F::Output {
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        let mut fut = std::pin::pin!(fut);
        for _ in 0..1000 {
            if let std::task::Poll::Ready(value) = fut.as_mut().poll(&mut cx) {
                return value;
            }
        }
        panic!("intercept did not resolve");
    }

    use rig_core::completion::CompletionRequest;
    use rig_core::message::Message;
}
