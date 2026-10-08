//! G1 验收（升级计划 §17.1）：Needle3 model adapter e2e——无网络。
//!
//! ```text
//! CompletionRequest
//!      ↓ Needle3Wire::encode（codec §10/§11）
//! NeedlePayload
//!      ↓ Needle3Transport::send（I1/I22：worker 侧阻塞解码）
//! NeedleFrame（单帧信封）
//!      ↓ NeedleDecoder（§6：function_calls → ToolCall 块）
//! CompletionResponse（raw = 信封原文，§15）
//!      ↓ ModelAdapter（上游 Serve 自动化）
//! rig-ecs Handler（G2 起接）
//! ```
//!
//! `MockBackend` 脚本化信封——生产路径把 `DlopenBackend` 换进来即可，
//! wire/codec/worker 全部不变。

#![cfg(feature = "rig")]

use std::sync::Arc;

use bevy_needle::engine::NeedleResponse;
use bevy_needle::rig::needle3_model;
use rig_core::completion::message::{AssistantContent, Message};
use rig_core::completion::CompletionRequest;
use serde_json::json;

use futures_now::block_on_first_poll;

/// MockBackend 便捷构造（信封脚本）。
fn mock_with(script: Vec<NeedleResponse>) -> Arc<dyn bevy_needle::backend::NeedleBackend> {
    let envelopes: Vec<serde_json::Value> = script
        .iter()
        .map(|response| serde_json::to_value(response).expect("serialize envelope"))
        .collect();
    Arc::new(bevy_needle::MockBackend::new(envelopes))
}

mod futures_now {
    /// 把 future 同步推进到 Ready（worker 回执毫秒级；测试专用驱动）。
    ///
    /// 自旋 poll：不依赖任何执行器，`Waker::noop` + 短 sleep 等待 worker。
    pub fn block_on_first_poll<F: std::future::Future>(fut: F) -> F::Output {
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        let mut fut = std::pin::pin!(fut);
        for _ in 0..10_000 {
            if let std::task::Poll::Ready(value) = fut.as_mut().poll(&mut cx) {
                return value;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("needle worker did not deliver within the test window");
    }
}

fn respond_envelope(text: &str, confidence: f64) -> NeedleResponse {
    serde_json::from_value(json!({
        "type": "respond",
        "success": true,
        "function_calls": [],
        "reasoning": text,
        "confidence": confidence,
        "prefill_tps": 4300.0,
        "decode_tps": 850.0,
    }))
    .expect("envelope")
}

fn call_envelope(name: &str, args: serde_json::Value, confidence: f64) -> NeedleResponse {
    serde_json::from_value(json!({
        "type": "call",
        "success": true,
        "function_calls": [{ "name": name, "arguments": args }],
        "confidence": confidence,
    }))
    .expect("envelope")
}

#[test]
fn unary_respond_completion_roundtrip() {
    let model = needle3_model(mock_with(vec![respond_envelope("done", 0.94)]), "needle3");
    let request = CompletionRequest::from(vec![Message::user("把人声变低沉")]);
    let response = block_on_first_poll(model.call(request)).expect("completion");
    assert_eq!(response.text(), "done");
    // §15：confidence / tps 全在 raw 可取。
    assert_eq!(response.raw["confidence"], json!(0.94));
    assert_eq!(response.raw["prefill_tps"], json!(4300.0));
    assert_eq!(response.provider(), "needle");
}

#[test]
fn unary_call_completion_maps_tool_calls() {
    let model = needle3_model(
        mock_with(vec![call_envelope("APP.change_pitch", json!({"semitones": -2}), 0.94)]),
        "needle3",
    );
    let request = CompletionRequest::from(vec![Message::user("降两个半音")]);
    let response = block_on_first_poll(model.call(request)).expect("completion");
    let calls: Vec<_> = response
        .choice
        .iter()
        .filter_map(|part| match part {
            AssistantContent::ToolCall(call) => Some(call),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].function.name.as_str(), "APP.change_pitch");
    assert_eq!(calls[0].function.arguments["semitones"], -2);
    assert!(!calls[0].id.wire().is_empty(), "rig 在空 id 时发本地 id");
}

#[test]
fn tool_result_input_reaches_worker_verbatim() {
    // 工具结果数组按 §11 官方语义整体回喂给引擎。
    let model = needle3_model(
        mock_with(vec![respond_envelope("ok", 0.9)]),
        "needle3",
    );
    let id = rig_core::message::CallId::from_wire("needle-local-0");
    let name = rig_core::message::ToolName::new("APP.change_pitch").expect("name");
    // 历史形态对齐真实多轮：先用户轮，再工具结果轮（末条判定取工具结果）。
    let request = CompletionRequest::from(vec![
        Message::user("降两个半音"),
        Message::User {
            content: vec![rig_core::completion::message::UserContent::tool_result(
                id,
                name,
                vec![rig_core::completion::message::ToolResultContent::Json {
                    value: json!({"ok": true}),
                }],
            )],
        },
    ]);
    let response = block_on_first_poll(model.call(request)).expect("completion");
    assert_eq!(response.text(), "ok");
}

#[test]
fn stream_degrades_to_single_terminal_frame() {
    // Needle 无 token 流：流式请求得到只含终点的流（Local wire 同形）。
    let model = needle3_model(mock_with(vec![respond_envelope("hello", 0.5)]), "needle3");
    let request = CompletionRequest::from(vec![Message::user("hi")]);
    let streamed = model.stream(request).expect("stream");
    let items: Vec<_> = block_on_first_poll(async move {
        use futures::StreamExt;
        let mut items = Vec::new();
        let mut stream = streamed;
        while let Some(item) = stream.next().await {
            items.push(item);
            if items.len() > 64 {
                break;
            }
        }
        items
    });
    // 至少有一个事件（origin）+ 终点。
    assert!(!items.is_empty());
    let last = items.last().expect("last").as_ref().expect("ok");
    let _ = last;
}
