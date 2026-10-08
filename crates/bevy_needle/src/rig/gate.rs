//! 置信度门控（升级计划 §12；`rig` feature）。
//!
//! Rig 0.44 的官方 policy hook 是 [`rig_core::serve::Intercept`]：
//! `after` 阶段拿到 handler 的答案（`Outcome::Completion`），在**工具
//! materialise 之前**给出 `Verdict`——低置信度的回复被 `Replace(Err(Denied))`
//! 拒绝，调用不执行（§12.3「禁止伪 gate」：拒绝点在 tool materialise 之前，
//! 保证 `low confidence → zero tool execution`）。
//!
//! 与 bevy_needle 原生 gate 的关系：原生 gate（`app.rs` 的
//! `EscalationPolicy::below_threshold`）在 Needle 主管线上执行；本 gate 在
//! rig-ecs 路径上执行（同一 `EscalationPolicy` 数据源，语义一致）。门控
//! 只约束「有 function_calls 的轮」——respond 轮不参与（与原生约定一致）。
//!
//! 线程面：`Intercept` 是 `WasmCompatSend + Sync`；`EscalationPolicy` 经
//! `Arc<Mutex<>>` 共享（宿主在运行期可改字段，读侧拿快照）。

use std::sync::{Arc, Mutex};

use rig_core::completion::CompletionResponse;
use rig_core::effect::{EffectId, EffectKind, Outcome};
use rig_core::error::ErrorReport;
use rig_core::serve::{Decision, Intercept, Verdict};

use crate::policy::EscalationPolicy;

/// 门控层名（记录/回放语义）。
pub const GATE_LAYER: &str = "needle-confidence";

/// 共享策略槽（宿主可运行期改写）。
pub type SharedPolicy = Arc<Mutex<EscalationPolicy>>;

/// 置信度门控 Intercept。
///
/// `after`：`Outcome::Completion` 且 `raw.confidence` 存在且低于门限
/// → `Verdict::Replace(Err(Denied))`；否则 `Keep`。
pub struct ConfidenceGate {
    policy: SharedPolicy,
}

impl ConfidenceGate {
    /// 构造（`policy` 共享快照；宿主负责初始化与更新）。
    pub fn new(policy: SharedPolicy) -> Self {
        Self { policy }
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
            return Verdict::Keep; // 无调用 → 门控只约束工具轮（原生约定）
        }
        let threshold = self
            .policy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .confidence_threshold
            .unwrap_or(0.0) as f64;
        if confidence < threshold {
            Verdict::Replace(Err(ErrorReport::new(
                rig_core::error::ErrorKind::Denied,
                format!(
                    "置信度 {confidence:.2} 低于门限 {threshold:.2}，调用未执行（升级契约）"
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

    fn gate(threshold: f32) -> ConfidenceGate {
        let mut policy = EscalationPolicy::default();
        policy.confidence_threshold = Some(threshold);
        ConfidenceGate::new(Arc::new(Mutex::new(policy)))
    }

    fn completion(choice: Vec<AssistantContent>, confidence: Option<f64>) -> Outcome {
        let mut origin = rig_core::message::Origin::new("needle.complete", "needle", "needle3");
        origin.response_id = None;
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
        assert!(matches!(
            futures_now(fut),
            Verdict::Replace(Err(_))
        ));
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

    /// `Waker::noop` 自旋推进（毫秒级 after 体）。
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
