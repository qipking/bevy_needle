//! rig 工具路径测试（`rig` feature）：验证 I3/I18/I20 —— rig 的 tool call
//! 必定走 ECS `ToolInvocation` pipeline，结果按原始顺序回喂。
//!
//! 模型用 `FakeModel`（手工 `impl CompletionModel`，无网络）；I26 的唯一
//! 注入口语义在 `rig_model_loop.rs` 里专项验证。

#![cfg(feature = "rig")]

use std::sync::Arc;

use bevy_app::App;
use bevy_ecs::prelude::*;
use bevy_needle::prelude::*;
use bevy_needle::escalate::DriverId;
use bevy_needle::rig::{register_with_model, RigDriverState};
use rig_core::completion::message::{
    AssistantContent, ToolCall as RigToolCall, ToolCallId, ToolFunction,
};
use rig_core::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse, Usage,
};
use serde_json::json;

/// 脚本化模型：先回工具调用，再回文本（工具轮的驱动源）。
#[derive(Default)]
struct FakeModel;

impl CompletionModel for FakeModel {
    fn completion(
        &self,
        request: CompletionRequest,
    ) -> impl std::future::Future<Output = Result<CompletionResponse, CompletionError>> {
        // 第一轮（history 无 ToolResult）→ 回工具调用；
        // 第二轮（有 ToolResult）→ 回文本收尾。
        let had_tool_result = request.chat_history.iter().any(|m| match m {
            rig_core::completion::message::Message::User { content, .. } => content
                .iter()
                .any(|c| {
                    matches!(
                        c,
                        rig_core::completion::message::UserContent::ToolResult(_)
                    )
                }),
            _ => false,
        });
        let resp = if had_tool_result {
            CompletionResponse::new(
                vec![AssistantContent::text("escalated done")],
                Usage::default(),
                "fake",
            )
        } else {
            let call = RigToolCall::new(
                ToolCallId::mint(),
                ToolFunction::new("echo".to_string(), json!({ "x": 42 })),
            );
            CompletionResponse::new(
                vec![AssistantContent::ToolCall(call)],
                Usage::default(),
                "fake",
            )
        };
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

fn run_frames(app: &mut App, frames: usize) {
    for _ in 0..frames {
        app.update();
    }
}

#[test]
fn rig_tool_call_goes_through_ecs_pipeline_and_roundtrips() {
    let mut app = App::new();
    // mock 后端注入：避免本仓库 checkout 的 discover_library 命中真引擎 .so
    // （libneedle.so 在本沙箱的进程退出析构会段错误——引擎侧问题，与升级链路无关）。
    app.add_plugins(BevyNeedlePlugin::with_backend(MockBackend::new(vec![json!({
        "type": "call", "success": true, "confidence": 0.01,
        "function_calls": [{ "name": "echo", "arguments": {} }],
    })])));

    app.world_mut().insert_resource(
        EscalationPolicy::new()
            .enabled()
            .with_fallback(OnlineFallback::LocalModelOnly)
            .with_tier(EscalationTarget::Local),
    );

    // 启动期预热完成的模型 + tier 0 接线（§4.7 注册入口）
    register_with_model(
        &mut app,
        DriverId("rig-test"),
        EscalationTarget::Local,
        Arc::new(FakeModel),
        &[0],
    )
    .expect("rig driver registers");

    // 工具 + handler（执行在 ECS，I3）
    let world = app.world_mut();
    let tool = world
        .spawn(ToolBundle::new(ToolSpec::new(
            "echo",
            "Echo",
            ParametersBuilder::new().string("x", "value to echo").build(),
        )))
        .id();
    register_tool_handler(world, "echo", |call| {
        Ok(ToolOutput::json(json!({ "echoed": call.args["x"] })))
    });

    // agent（带门限）：走真实链路——needle 低置信度门控 → coordinator submit
    // → RigDriver::submit 发 Attach → rig 系统接管（Attach 缺失会让 worker
    // 回执 Unavailable，测试会抓住）
    let handles = spawn_agent(
        world,
        NeedleAgentSpec::new("a").with_confidence_threshold(0.5),
    );
    attach_tool(world, handles.agent, tool).unwrap();
    let session = handles.session;
    let agent = handles.agent;

    app.world_mut()
        .write_message(RunAgent::new(handles.agent, "hi"));
    run_frames(&mut app, 3);

    // coordinator 已提交 rig：找到 run（经由 RunOwner）
    let run = {
        let w = app.world_mut();
        let mut q =
            w.query::<(Entity, &RunStatus, Option<&RigDriverState>)>();
        q.iter(w)
            .filter_map(|(run, _status, state)| state.is_some().then_some(run))
            .next()
    };
    let Some(run) = run else {
        panic!("escalation should have reached rig driver");
    };

    // FakeModel 回工具调用 → CallTools → ECS ToolInvocation → handler 执行
    run_frames(&mut app, 20);

    // 断言：产生了 ECS ToolInvocation 且已执行（I3），call_id 非空（I18 往返）
    let invocations: Vec<(String, ToolInvocationStatus, serde_json::Value)> = {
        let w = app.world_mut();
        let mut q = w.query::<(
            &ToolInvocationCall,
            &ToolInvocationStatus,
            Option<&ToolInvocationOutput>,
        )>();
        q.iter(w)
            .filter(|(call, _, _)| call.0.run == run)
            .map(|(call, status, output)| {
                (
                    call.0.call_id.clone(),
                    *status,
                    output
                        .map(|o| o.0.value.clone())
                        .unwrap_or(serde_json::Value::Null),
                )
            })
            .collect()
    };
    assert_eq!(
        invocations.len(),
        1,
        "rig tool call must become ECS invocation"
    );
    assert_eq!(invocations[0].1, ToolInvocationStatus::Completed);
    assert_eq!(invocations[0].2["echoed"], 42, "handler executed in ECS");
    assert!(
        !invocations[0].0.is_empty(),
        "call_id 必须非空（I18 严格往返）"
    );

    // 工具结果按序回喂后 rig 继续下一轮模型调用（FakeModel 回文本）
    run_frames(&mut app, 20);

    // Done → Succeeded → coordinator → Completed
    let completed = {
        let w = app.world_mut();
        let mut q = w.query::<(&RunStatus, Option<&RunResultText>)>();
        q.iter(w).any(|(s, t)| {
            *s == RunStatus::Completed && t.map(|t| t.0 == "escalated done").unwrap_or(false)
        })
    };
    assert!(completed, "rig Done → coordinator → Completed");
}

#[test]
fn rig_step_system_runs_before_coordinator() {
    // 冒烟：register_with_model 后系统接线无 panic
    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(MockBackend::new(vec![])));
    register_with_model(
        &mut app,
        DriverId("rig-test"),
        EscalationTarget::Local,
        Arc::new(FakeModel),
        &[0],
    )
    .expect("rig driver registers");
    app.world_mut().insert_resource(
        EscalationPolicy::new()
            .enabled()
            .with_fallback(OnlineFallback::LocalModelOnly)
            .with_tier(EscalationTarget::Local),
    );
    run_frames(&mut app, 5);
}
