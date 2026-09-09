//! §16 DriverId 覆盖 bug 红测（v14 第一优先级）。
//!
//! 两个**不同 Rust 类型**的模型（16.5：不能用同类型两实例——归因不清）：
//! tier 0 → FakeCandleModel（capability=Local，注定失败），tier 1 →
//! FakeOpenAiModel（capability=Remote，成功）。断言**执行者身份**：
//! - FakeCandleModel 恰被调用 1 次（tier 0 执行者）
//! - FakeOpenAiModel 恰被调用 1 次（tier 1 执行者）
//! - 最终 EscalationState.driver_id == "rig-openai"
//!
//! 修复前预期：`RigDriver::id()` 固定 "rig" → 第二次注册覆盖第一次 →
//! 两档都被后者执行（或模型/收件箱错配）→ 上述断言失败（红）。

#![cfg(feature = "rig")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use bevy_app::App;
use bevy_needle::prelude::*;
use bevy_needle::rig::register_with_model;
use rig_core::completion::message::AssistantContent;
use rig_core::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse, Usage,
};
use serde_json::json;

/// 调用计数 + 脚本化响应的基类零件。
#[derive(Default)]
struct CallCounter(AtomicUsize);

impl CallCounter {
    fn bump(&self) -> usize {
        self.0.fetch_add(1, Ordering::SeqCst) + 1
    }
    fn calls(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

/// tier 0 模型：Local，第一次调用返回错误（触发升档）。
struct FakeCandleModel {
    calls: CallCounter,
}

impl CompletionModel for FakeCandleModel {
    fn completion(
        &self,
        _request: CompletionRequest,
    ) -> impl std::future::Future<Output = Result<CompletionResponse, CompletionError>> {
        let call = self.calls.bump();
        let resp: Result<CompletionResponse, CompletionError> = Err(CompletionError::ResponseError(
            format!("candle attempt {call} exploded (scripted)"),
        ));
        std::future::ready(resp)
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> impl std::future::Future<Output = Result<rig_core::streaming::StreamingCompletionResponse, CompletionError>>
    {
        std::future::ready(Err(CompletionError::ResponseError("no stream".into())))
    }
}

/// tier 1 模型：Remote，返回成功文本。
struct FakeOpenAiModel {
    calls: CallCounter,
}

impl CompletionModel for FakeOpenAiModel {
    fn completion(
        &self,
        _request: CompletionRequest,
    ) -> impl std::future::Future<Output = Result<CompletionResponse, CompletionError>> {
        let call = self.calls.bump();
        let resp = CompletionResponse::new(
            vec![AssistantContent::text(format!("openai answered (call {call})"))],
            Usage::default(),
            "fake-openai",
        );
        std::future::ready(Ok(resp))
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> impl std::future::Future<Output = Result<rig_core::streaming::StreamingCompletionResponse, CompletionError>>
    {
        std::future::ready(Err(CompletionError::ResponseError("no stream".into())))
    }
}

#[test]
fn two_models_two_tiers_execute_with_distinct_identity() {
    let candle = Arc::new(FakeCandleModel { calls: CallCounter::default() });
    let openai = Arc::new(FakeOpenAiModel { calls: CallCounter::default() });

    let mut app = App::new();
    // mock 后端注入：避免本仓库 checkout 的 discover_library 命中真引擎 .so
    app.add_plugins(BevyNeedlePlugin::with_backend(MockBackend::new(vec![json!({
        "type": "call", "success": true, "confidence": 0.01,
        "function_calls": [{ "name": "echo", "arguments": {} }],
    })])));

    app.world_mut().insert_resource(
        EscalationPolicy::new()
            .enabled()
            .with_fallback(OnlineFallback::Cloud)
            .with_tier(EscalationTarget::Local)
            .with_tier(EscalationTarget::Remote),
    );

    // 显式身份（I30）：不同模型不同 id；capability 声明（I25/I30——
    // 硬编码 Local 会让 Remote 档永远注册不上，§16.3）
    register_with_model(
        &mut app,
        bevy_needle::escalate::DriverId("rig-candle"),
        EscalationTarget::Local,
        Arc::clone(&candle),
        &[0],
    )
    .expect("candle driver registers");
    register_with_model(
        &mut app,
        bevy_needle::escalate::DriverId("rig-openai"),
        EscalationTarget::Remote,
        Arc::clone(&openai),
        &[1],
    )
    .expect("openai driver registers");

    let world = app.world_mut();
    let tool = world
        .spawn(ToolBundle::new(ToolSpec::new(
            "echo",
            "Echo",
            ParametersBuilder::new().build(),
        )))
        .id();
    register_tool_handler(world, "echo", |_call| Ok(ToolOutput::ok()));
    let handles = spawn_agent(
        world,
        NeedleAgentSpec::new("a").with_confidence_threshold(0.5),
    );
    attach_tool(world, handles.agent, tool).unwrap();

    app.world_mut()
        .write_message(RunAgent::new(handles.agent, "hi"));
    for _ in 0..400 {
        app.update();
    }

    // ── 执行者身份断言（16.5：不是只比输出文本）──
    assert_eq!(
        candle.calls.calls(),
        1,
        "tier 0 must be executed by the candle model exactly once"
    );
    assert_eq!(
        openai.calls.calls(),
        1,
        "tier 1 must be executed by the openai model exactly once"
    );

    // 最终执行者身份：EscalationState.driver_id（I6：状态里只认 id）
    let (status, driver_id) = {
        let w = app.world_mut();
        let mut q = w.query::<(&RunStatus, Option<&bevy_needle::escalate::EscalationState>)>();
        q.iter(w)
            .map(|(s, esc)| (*s, esc.map(|e| e.driver_id.as_str().to_string())))
            .next()
            .expect("run exists")
    };
    assert_eq!(
        driver_id.as_deref(),
        Some("rig-openai"),
        "final executor identity must be rig-openai"
    );
    assert!(
        matches!(status, RunStatus::Completed),
        "openai tier should complete the run, got {status:?}"
    );
}
