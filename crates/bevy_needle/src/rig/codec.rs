//! Needle3 ↔ Rig 0.44 的纯数据转换（升级计划 §19.1 `model.rs` 的职责）。
//!
//! 只做三件事：
//! 1. `CompletionRequest` → [`NeedlePayload`]（§10：不重放整段 history，
//!    末条消息判定本轮输入）；
//! 2. `NeedleResponse` → `Vec<AssistantContent>`（§6：function_calls →
//!    ToolCall 块；respond → 文本块）；
//! 3. Rig 工具结果 → Needle 官方 `json.dumps(results)` 数组语义（§11）。
//!
//! 无 World、无 runtime、无 escalation——纯函数，worker 与 ECS 均可调用。

use serde_json::json;

use crate::engine::NeedleResponse;

use rig_core::completion::CompletionRequest;
use rig_core::completion::message::{Message, ToolResultContent, UserContent};
use rig_core::error::EncodeError;

/// `max_tokens` 缺省时的生成长度（对齐现有 `RunAgent` 默认值）。
pub(crate) const DEFAULT_MAX_NEW_TOKENS: u32 = 256;

/// 会话输入槽：Needle 单会话的复用判定（§12/§13：一个模型一个会话）。
#[derive(Debug, Default)]
pub struct SessionSlot {
    /// 会话代号（诊断面；Needle 引擎本身进程级单会话）。
    pub label: Option<String>,
}

/// 本轮输入（§10 判定）：新用户输入或工具结果数组。
#[derive(Debug, Clone)]
pub struct NeedlePayload {
    /// 本轮输入（用户文本或工具结果 JSON）。
    pub input: String,
    /// 本轮最大生成 token。
    pub max_new_tokens: u32,
    /// 会话标签（worker 路由；`None` = worker 默认会话）。
    pub session: Option<String>,
}

/// 从规范请求抽取本轮输入（§10：不重放整段 history）。
///
/// 判定规则：
/// - 末条消息是 `User` 的 `ToolResult` 集 → 工具结果数组（§11 官方语义）；
/// - 末条消息是 `User` 的纯文本 → 新用户输入（原文直传）；
/// - 末条是 `System`（如无用户轮的极端形态）→ 其文本；
/// - 末条是 `Assistant` → 请求非法（`EncodeError`，协议要求用户轮收尾）。
///
/// `max_tokens` 缺省取 [`DEFAULT_MAX_NEW_TOKENS`]。
pub fn encode_payload(request: &CompletionRequest) -> Result<NeedlePayload, EncodeError> {
    let input = terminal_input(&request.chat_history).ok_or_else(|| {
        EncodeError::request(
            "completion request ends without a user turn (needle needs a user input or a tool result batch)",
        )
    })?;
    let max_new_tokens = request
        .max_tokens
        .and_then(|tokens| u32::try_from(tokens).ok())
        .unwrap_or(DEFAULT_MAX_NEW_TOKENS);
    Ok(NeedlePayload {
        input,
        max_new_tokens,
        session: None,
    })
}

/// 历史尾部 → 本轮输入（纯函数；§10）。
pub fn terminal_input(history: &[Message]) -> Option<String> {
    let last = history.last()?;
    match last {
        Message::User { content } => {
            // §11 官方 run() 语义：工具结果按数组回喂，元素是**每个工具
            // 返回的 JSON 值本身**（`{"ok": true}`），不是 Rig 的带标签
            // 形态——Rig 的 `type`/`call`/`name` 元数据不进 Needle 输入。
            let results: Vec<serde_json::Value> = content
                .iter()
                .filter_map(|part| match part {
                    UserContent::ToolResult(result) => Some(
                        result
                            .content
                            .iter()
                            .map(|item| match item {
                                ToolResultContent::Json { value } => value.clone(),
                                ToolResultContent::Text(text) => {
                                    serde_json::json!({ "text": text.text })
                                }
                                ToolResultContent::Image(_) => {
                                    serde_json::json!({ "image": "unrenderable-image" })
                                }
                            })
                            .collect::<Vec<_>>(),
                    ),
                    _ => None,
                })
                .flatten()
                .collect();
            if !results.is_empty() {
                return serde_json::to_string(&results).ok();
            }
            content.iter().find_map(|part| match part {
                UserContent::Text(text) => Some(text.text.clone()),
                _ => None,
            })
        }
        Message::System { content } => Some(content.clone()),
        Message::Assistant(_) => None,
    }
}

/// Rig 工具结果内容 → Needle 官方数组语义的 JSON 文本（§11）。
///
/// `Json` 项保持原值；`Text` 项包成 `{"text": ...}`（引擎约定所有结果可序列化）；
/// `Image` 项包成 `{"image_kind": ...}`（Needle 不读图，占位说明）。
pub fn tool_results_input(results: &[ToolResultContent]) -> Result<String, serde_json::Error> {
    let items: Vec<serde_json::Value> = results
        .iter()
        .map(|content| match content {
            ToolResultContent::Json { value } => value.clone(),
            ToolResultContent::Text(text) => json!({ "text": text.text }),
            ToolResultContent::Image(_) => json!({ "image": "unrenderable-image" }),
        })
        .collect();
    serde_json::to_string(&items)
}

/// `NeedleResponse` → 规范 assistant 内容（§6；I18 v27）。
///
/// `call` 信封 → 每个 function_call 一个 ToolCall 块。call id 的**唯一
/// mint 点**在本函数：Needle 从不发 call id，adapter 解码层按 model
/// 实例级单调序生成 `needle-call-<seq>`——生成一次、两侧同值、执行前
/// 不得重铸（I18 v27）。`seq` 由调用方传入（wire 解码器持有 model 级
/// `AtomicU64`，保证跨轮次/跨 run 不碰撞）。
///
/// # Panics
///
/// 占位名 mint（`ToolName::new`）对非空字面串失败——不变量，正常不会发生。
pub fn choice_from_envelope(
    response: &NeedleResponse,
    seq: &std::sync::atomic::AtomicU64,
) -> Vec<rig_core::message::AssistantContent> {
    use rig_core::message::AssistantContent;
    let mut choice = Vec::new();
    if let Some(reasoning) = &response.reasoning {
        choice.push(AssistantContent::reasoning(reasoning.clone()));
    }
    if response.is_call() {
        for (index, call) in response.function_calls.iter().enumerate() {
            let name = rig_core::message::ToolName::new(call.name.as_str()).unwrap_or_else(|_| {
                rig_core::message::ToolName::new(format!("needle-invalid-{index}"))
                    .expect("non-empty placeholder")
            });
            choice.push(AssistantContent::tool_call(
                call_id(seq),
                name,
                call.arguments.clone(),
            ));
        }
        return choice;
    }
    let text = response.summary();
    if !text.is_empty() {
        choice.push(AssistantContent::text(text));
    }
    choice
}

/// 规范 call id 的唯一命名点（I18 v27）：`needle-call-<seq>`，model 实例级
/// 单调序——run 级唯一的实现形态（同一 model 的多个 run 也互不碰撞）。
pub fn call_id(seq: &std::sync::atomic::AtomicU64) -> String {
    let seq = seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("needle-call-{seq}")
}

#[cfg(test)]
mod tests;
