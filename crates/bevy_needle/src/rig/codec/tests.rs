//! codec 的单测：纯数据转换（§6/§10/§11/§15 映射语义）。

use super::*;
use rig_core::completion::message::{AssistantContent, Message, UserContent};
use serde_json::json;

fn request(history: Vec<Message>) -> CompletionRequest {
    CompletionRequest::from(history)
}

#[test]
fn terminal_user_text_is_input() {
    let req = request(vec![Message::user("把人声变低沉")]);
    let payload = encode_payload(&req).expect("encode");
    assert_eq!(payload.input, "把人声变低沉");
    assert_eq!(payload.max_new_tokens, DEFAULT_MAX_NEW_TOKENS);
}

#[test]
fn tool_results_batch_becomes_array() {
    let id = rig_core::message::CallId::from_wire("needle-local-0");
    let name = rig_core::message::ToolName::new("APP.change_pitch").expect("name");
    let content = vec![ToolResultContent::Json { value: json!({"ok": true}) }];
    let result = rig_core::completion::message::ToolResult {
        call: id,
        name: name.clone(),
        content,
        is_error: false,
    };
    let req = request(vec![Message::User {
        content: vec![UserContent::ToolResult(result)],
    }]);
    let payload = encode_payload(&req).expect("encode");
    let parsed: serde_json::Value = serde_json::from_str(&payload.input).expect("array");
    assert!(parsed.is_array());
    assert_eq!(parsed.as_array().expect("array").len(), 1);
    // ToolResultContent 带 `type: json` 标签序列化——Needle 收到的数组元素
    // 是 Rig 规范形态；worker 侧拆出内层 value（见 tool_results_input 语义）。
        // §11 官方语义：数组元素是**每个工具返回的 JSON 值本身**——Rig 的
    // `type`/`call`/`name` 元数据不进 Needle 输入。
    assert_eq!(parsed[0], json!({"ok": true}));
}

#[test]
fn max_tokens_is_capped_to_u32() {
    let mut req = request(vec![Message::user("hi")]);
    req.max_tokens = Some(u64::MAX);
    let payload = encode_payload(&req).expect("encode");
    assert_eq!(payload.max_new_tokens, DEFAULT_MAX_NEW_TOKENS);
    req.max_tokens = Some(64);
    let payload = encode_payload(&req).expect("encode");
    assert_eq!(payload.max_new_tokens, 64);
}

#[test]
fn assistant_terminated_history_is_rejected() {
    let req = request(vec![Message::Assistant(rig_core::completion::message::AssistantMessage::new(
        vec![AssistantContent::text("done")],
    ))]);
    assert!(encode_payload(&req).is_err());
}

#[test]
fn empty_history_is_rejected() {
    assert!(encode_payload(&request(Vec::new())).is_err());
}

#[test]
fn envelope_respond_becomes_text() {
    let response = NeedleResponse {
        kind: "respond".into(),
        success: true,
        error: None,
        error_code: None,
        function_calls: Vec::new(),
        reasoning: Some("turning down".into()),
        confidence: Some(0.94),
        prefill_tps: None,
        decode_tps: None,
        peak_ram_mb: None,
        validation: None,
    };
    let seq = std::sync::atomic::AtomicU64::new(0);
    let choice = choice_from_envelope(&response, &seq);
    // reasoning 先落 Reasoning 块，summary()（= reasoning 文本）再落 Text 块。
    assert!(matches!(&choice[0], AssistantContent::Reasoning(_)));
    assert!(matches!(&choice[1], AssistantContent::Text(_)));
}

#[test]
fn envelope_call_becomes_tool_calls() {
    let response = NeedleResponse {
        kind: "call".into(),
        success: true,
        error: None,
        error_code: None,
        function_calls: vec![crate::engine::NeedleFunctionCall {
            name: "APP.change_pitch".into(),
            arguments: json!({ "semitones": -2 }),
        }],
        reasoning: None,
        confidence: Some(0.94),
        prefill_tps: None,
        decode_tps: None,
        peak_ram_mb: None,
        validation: None,
    };
    let seq = std::sync::atomic::AtomicU64::new(0);
    let choice = choice_from_envelope(&response, &seq);
    match &choice[0] {
        AssistantContent::ToolCall(call) => {
            assert_eq!(call.function.name.as_str(), "APP.change_pitch");
            assert_eq!(call.function.arguments["semitones"], -2);
            // I18 v27：规范 call id 形态（`needle-call-<seq>`），两侧同值。
            assert_eq!(call.id.to_string(), "needle-call-0");
        }
        other => panic!("expected tool call, got {other:?}"),
    }
}

#[test]
fn tool_results_input_roundtrip() {
    let content = vec![ToolResultContent::Json { value: json!({"volume_db": -3}) }];
    let text = tool_results_input(&content).expect("serialize");
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("array");
    assert_eq!(parsed, json!([{"volume_db": -3}]));
}
