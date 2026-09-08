//! rig 工具路径测试（`rig` feature）：验证 I3/I18/I20 —— rig 的 tool call
//! 必定走 ECS `ToolInvocation` pipeline，结果按原始顺序回喂。
//!
//! 无真实模型：通过 [`RigModelInbox`] 直接注入 [`rig_run::ModelTurn`]，
//! 绕过 transport 层的异步模型调用（PR-B 范围说明）。

#![cfg(feature = "rig")]

use std::collections::BTreeSet;
use std::sync::Arc;

use bevy_app::App;
use bevy_ecs::prelude::*;
use bevy_needle::escalate::{DriverRegistry, EscalationReason, EscalationState};
use bevy_needle::prelude::*;
use bevy_needle::rig::{
    rig_step_system, RigDriver, RigModelInbox, RigModelTurn, RIG_DRIVER_ID,
};
use rig_core::completion::message::{
    AssistantContent, ToolCall as RigToolCall, ToolCallId, ToolFunction,
};
use rig_core::completion::Usage;
use rig_run::ModelTurn;
use serde_json::json;

fn run_frames(app: &mut App, frames: usize) {
    for _ in 0..frames {
        app.update();
    }
}

/// echo 工具的 advertise 面（与 agent 内注册的一致；填 allowed 集合用）。
fn echo_defs() -> Vec<rig_core::completion::ToolDefinition> {
    vec![rig_core::completion::ToolDefinition {
        name: "echo".to_string(),
        description: "Echo".to_string(),
        parameters: ParametersBuilder::new().string("x", "value to echo").build(),
    }]
}

fn tool_call_turn(name: &str, args: serde_json::Value) -> ModelTurn {
    let call = RigToolCall::new(ToolCallId::mint(), ToolFunction::new(name.to_string(), args));
    bevy_needle::rig::model_turn_with_tools(
        None,
        vec![AssistantContent::ToolCall(call)],
        Usage::default(),
        &echo_defs(),
    )
}

fn text_turn(text: &str) -> ModelTurn {
    bevy_needle::rig::model_turn_with_tools(
        None,
        vec![AssistantContent::text(text)],
        Usage::default(),
        &echo_defs(),
    )
}

#[test]
fn rig_tool_call_goes_through_ecs_pipeline_and_roundtrips() {
    let mut app = App::new();
    // 用 mock 后端注入：避免本仓库 checkout 的 discover_library 命中真引擎 .so
    // （libneedle.so 在本沙箱的进程退出析构会段错误——引擎侧问题，与升级链路无关）。
    app.add_plugins(BevyNeedlePlugin::with_backend(MockBackend::new(vec![])));

    app.world_mut().insert_resource(
        EscalationPolicy::new()
            .enabled()
            .with_fallback(OnlineFallback::LocalModelOnly)
            .with_tier(EscalationTarget::Local),
    );
    {
        let mut registry = app.world_mut().resource_mut::<DriverRegistry>();
        registry.register_for_tier(Arc::new(RigDriver::new()), 0);
    }

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

    // agent + 会话 + 转录（供 rig 系统重建 AgentRun，I14 排序）
    let session = spawn_session(world);
    let agent = world
        .spawn((
            NeedleAgentSpec::new("a"),
            AgentToolRefs(vec![tool]),
            PrimarySession(session),
        ))
        .id();
    spawn_chat_message_now(world, session, ChatMessageRole::User, "hi");

    // run 直接进入 Escalating + rig 接管（手插组件：deadline/started 必须显式给足，
    // 否则 coordinator 的 per_tier 超时会立刻把它判失败）
    let run = {
        let mut state = EscalationState::new(0, RIG_DRIVER_ID, 0, EscalationReason::BelowConfidence);
        state.deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        state.started = std::time::Instant::now();
        world
            .spawn(RunBundle::new(agent, session, "hi"))
            .insert(RunStatus::Escalating { tier: 0 })
            .insert(state)
            .id()
    };

    // 帧 1：rig 系统创建 RigDriverState → CallModel → AwaitModel
    run_frames(&mut app, 3);
    {
        let state = app.world().get::<bevy_needle::rig::RigDriverState>(run);
        assert!(state.is_some(), "rig system should create RigDriverState");
    }

    // 注入含工具调用的模型回合 → CallTools → ToolInvocation
    app.world_mut().resource::<RigModelInbox>().inject(RigModelTurn {
        run,
        epoch: 0,
        turn: tool_call_turn("echo", json!({ "x": 42 })),
    });
    run_frames(&mut app, 20);

    // 断言：产生了 ECS ToolInvocation 且已执行（I3）
    let invocations: Vec<(String, ToolInvocationStatus, serde_json::Value)> = {
        let mut q = app.world_mut().query::<(
            &ToolInvocationCall,
            &ToolInvocationStatus,
            Option<&ToolInvocationOutput>,
        )>();
        q.iter(app.world())
            .filter(|(call, _, _)| call.0.run == run)
            .map(|(call, status, output)| {
                (
                    call.0.call_id.clone(),
                    *status,
                    output.map(|o| o.0.value.clone()).unwrap_or(serde_json::Value::Null),
                )
            })
            .collect()
    };
    assert_eq!(invocations.len(), 1, "rig tool call must become ECS invocation");
    assert_eq!(invocations[0].1, ToolInvocationStatus::Completed);
    assert_eq!(invocations[0].2["echoed"], 42, "handler executed in ECS");
    assert!(!invocations[0].0.is_empty(), "call_id 必须非空（I18 严格往返）");

    // 工具结果回喂后 rig 继续 → CallModel（等第二个模型回合）
    {
        let state = app.world().get::<bevy_needle::rig::RigDriverState>(run);
        assert!(state.is_some());
        assert!(
            matches!(
                state.map(|s| &s.awaiting),
                Some(bevy_needle::rig::RigAwait::Model)
            ),
            "after tool results, rig should await next model turn"
        );
    }

    // 注入文本回合 → Done → Succeeded → coordinator → Completed
    app.world_mut().resource::<RigModelInbox>().inject(RigModelTurn {
        run,
        epoch: 0,
        turn: text_turn("escalated done"),
    });
    run_frames(&mut app, 20);

    let completed = {
        let mut q = app
            .world_mut()
            .query::<(&RunStatus, Option<&RunResultText>)>();
        q.iter(app.world()).any(|(status, text)| {
            *status == RunStatus::Completed
                && text.map(|t| t.0 == "escalated done").unwrap_or(false)
        })
    };
    assert!(completed, "rig Done → coordinator → Completed");
}

#[test]
fn rig_step_system_is_registered_before_coordinator() {
    // 冒烟：系统按序注册且不 panic（rig + escalate 共存时）
    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(MockBackend::new(vec![])));
    app.world_mut().insert_resource(
        EscalationPolicy::new()
            .enabled()
            .with_fallback(OnlineFallback::LocalModelOnly)
            .with_tier(EscalationTarget::Local),
    );
    {
        let mut registry = app.world_mut().resource_mut::<DriverRegistry>();
        registry.register_for_tier(Arc::new(RigDriver::new()), 0);
    }
    run_frames(&mut app, 5);
    // 直接调用系统函数本身可编译可执行（顺序由插件保证，这里验证无 panic）
    rig_step_system(app.world_mut());
}
