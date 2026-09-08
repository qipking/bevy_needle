//! rig 工具桥（规格 §5.2，整个集成最值得坚持的一点——I3）。
//!
//! ```text
//! rig AgentRun::CallTools
//!      │
//!      ▼
//! bevy_needle ToolCall / ToolInvocation    ← 复用现有 ECS pipeline
//!      │
//!      ▼
//! ECS dispatch_registered_tool_calls（绝不走 rig async 回调）
//!      │
//!      ▼
//! rig AgentRun::tool_results(..)
//! ```
//!
//! 三个必须处理的细节（规格 §5.2 + I18/I19/I20）：
//! 1. `PortableDynamicTool` 而非 `DynamicTool`——context-free，rig 侧只持定义，
//!    ECS 侧按名解析，I3 从「靠纪律」变成「类型上无法违反」；
//! 2. `PendingToolCall::preresolved_result` 非空时**不进 ECS**，直接回灌；
//! 3. `tool_call.id` 必须**原样往返**到 `ToolCall.call_id`（缺失 → 错误，**禁止 mint**）。

use bevy_ecs::prelude::Entity;
use rig_core::completion::message::{ToolCallId, ToolResult, ToolResultContent, UserContent};
use rig_core::tool::{PortableDynamicTool, ToolErrorKind, ToolExecutionError, ToolOutput};
use rig_core::wasm_compat::WasmBoxedFuture;
use rig_run::PendingToolCall;
use serde_json::Value;

use crate::escalate::driver::DriverProtocolError;
use crate::tool::{ToolCall, ToolSpec};

/// 绊线回调的 future 构造器（独立成函数以便无 `ToolContext` 直接测试：
/// `PortableDynamicTool` 不暴露回调，测它等于测整条 rig 侧分发——那是
/// 上游测试的事，我们只测自己的绊线语义）。
fn tripwire_future(
    name: String,
) -> WasmBoxedFuture<'static, Result<ToolOutput, ToolExecutionError>> {
    Box::pin(async move {
        Err(ToolExecutionError::new(
            ToolErrorKind::Other,
            format!(
                "工具 {name} 的执行必须在 ECS 侧（bevy_needle 不变量 I3）：rig 侧回调是绊线，不应被触发"
            ),
        ))
    })
}

/// 把一个 needle `ToolSpec` 注册为 rig 侧的"绊线"动态工具。
///
/// 回调永不成功：任何 rig 侧执行尝试都以 [`ToolErrorKind::Other`] 失败，
/// 错误文本指明 I3 与本模块。定义（name/description/parameters）来自 spec
/// 原文，保证 `advertise_tools` 上报给模型的工具面与 needle 一致。
pub fn escalation_tripwire(spec: &ToolSpec) -> PortableDynamicTool {
    let name = spec.name.clone();
    PortableDynamicTool::new(
        spec.name.clone(),
        spec.description.clone(),
        spec.parameters.clone(),
        move |_args: Value| tripwire_future(name.clone()),
    )
}

/// 批量绊线注册（保持入参顺序；调用方按需去重）。
pub fn tripwires(specs: &[ToolSpec]) -> Vec<PortableDynamicTool> {
    specs.iter().map(escalation_tripwire).collect()
}

/// 从绊线工具提取 `ToolDefinition`（走上游 `definition()`，避免手写字段）。
pub fn definitions(tools: &[PortableDynamicTool]) -> Vec<rig_core::completion::ToolDefinition> {
    tools.iter().map(|tool| tool.definition()).collect()
}

/// `preresolved_result` 纪律（I19）：非空时返回其 JSON，调用方**直接回灌**，
/// 不执行工具；空表示该调用真实待执行。
pub fn preresolved_content(pending: &PendingToolCall) -> Option<Value> {
    pending
        .preresolved_result
        .as_ref()
        .and_then(|content| serde_json::to_value(content).ok())
}

/// rig 调用 → needle `ToolCall`（I18：call_id 严格往返）。
///
/// `tool` 实体由调用方经 `ToolRegistry::get_by_name` 解析后传入（未知工具
/// 传 `None`，本函数落 `Entity::PLACEHOLDER`，与 needle 主管线行为一致）。
///
/// # Errors
/// [`DriverProtocolError::MissingToolCallId`]——`tool_call.id` 为空时返回，
/// **禁止 mint 新 ID**（I18）。
pub fn pending_to_tool_call(
    run: Entity,
    pending: &PendingToolCall,
    tool: Option<Entity>,
) -> Result<ToolCall, DriverProtocolError> {
    let call = &pending.tool_call;
    let call_id = call.id.as_str();
    if call_id.is_empty() {
        return Err(DriverProtocolError::MissingToolCallId);
    }
    Ok(ToolCall {
        run,
        tool: tool.unwrap_or(Entity::PLACEHOLDER),
        name: call.function.name.clone(),
        call_id: call_id.to_string(),
        args: call.function.arguments.clone(),
    })
}

/// 回喂结果构造：`(call_id, name, payload)` → `UserContent::ToolResult`。
///
/// `call_id` 必须原样返回 rig 铸造的 ID（I18）：用 [`ToolCallId::new`]
/// 重建（严格往返），**不是** [`ToolCallId::mint`] / `new_or_mint`。
///
/// # Errors
/// [`DriverProtocolError::MissingToolCallId`]——空 call_id。
pub fn tool_result(
    call_id: &str,
    name: &str,
    payload: Value,
) -> Result<UserContent, DriverProtocolError> {
    let id = ToolCallId::new(call_id).ok_or(DriverProtocolError::MissingToolCallId)?;
    Ok(UserContent::ToolResult(ToolResult {
        call: id,
        provider: None,
        name: name.to_string(),
        content: vec![ToolResultContent::Json { value: payload }],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::task::{Context, Poll, Waker};

    fn spec() -> ToolSpec {
        ToolSpec::new(
            "set_volume",
            "Set the playback volume.",
            json!({ "type": "object", "properties": { "percent": { "type": "integer" } } }),
        )
    }

    #[test]
    fn tripwire_always_errors_with_i3_message() {
        let mut fut = tripwire_future("set_volume".into());
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        match std::pin::Pin::new(&mut fut).poll(&mut cx) {
            Poll::Ready(Err(err)) => assert!(err.to_string().contains("I3"), "应指明 I3: {err}"),
            other => panic!("绊线必须立即以错误终结，得到 {other:?}"),
        }
    }

    #[test]
    fn definitions_carry_spec_shape() {
        let tools = tripwires(&[spec()]);
        let defs = definitions(&tools);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "set_volume");
        assert_eq!(defs[0].parameters["properties"]["percent"]["type"], "integer");
    }

    #[test]
    fn tool_result_roundtrips_call_id_without_minting() {
        let content = tool_result("rig-call-1", "echo", json!({ "ok": true })).unwrap();
        match content {
            UserContent::ToolResult(result) => {
                assert_eq!(result.wire_call_id(), "rig-call-1");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn missing_call_id_is_rejected_not_minted() {
        assert!(tool_result("", "echo", json!({})).is_err());
    }
}
