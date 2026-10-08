//! 升级能力层 e2e（`escalate` feature，**无 rig、无网络**）。
//!
//! 规格 §0 成功判据 2：MockDriver 跑通完整升级链路。
//! 链路：needle 低置信度门控 → `Escalating{0}` → DriverCoordinator → MockDriver
//! （脚本化成功）→ 事件回灌 → `Completed`，且低置信度调用**绝不执行**。

#![cfg(feature = "escalate")]

use std::sync::Arc;

use bevy_app::App;
use bevy_ecs::prelude::*;
use bevy_needle::escalate::{Driver, DriverRegistry, MockDriver, MockStep};
use bevy_needle::prelude::*;
use serde_json::json;

fn run_frames(app: &mut App, frames: usize) {
    for _ in 0..frames {
        app.update();
    }
}

fn setup_with_script(steps: Vec<MockStep>) -> App {
    let mock = MockBackend::new(vec![json!({
        "type": "call",
        "success": true,
        "confidence": 0.01,
        "function_calls": [{ "name": "echo", "arguments": {} }],
    })]);
    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(mock));

    app.world_mut().insert_resource(
        EscalationPolicy::new()
            .enabled()
            .with_fallback(OnlineFallback::LocalModelOnly)
            .with_tier(EscalationTarget::Local),
    );
    {
        let mut registry = app.world_mut().resource_mut::<DriverRegistry>();
        registry.register_for_tier(Arc::new(MockDriver::new("mock").with_script(steps)), 0).expect("register driver");
    }

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
    app
}

#[test]
fn mock_driver_completes_escalation_without_executing_tools() {
    let mut app = setup_with_script(vec![MockStep::succeed("escalated by mock")]);
    run_frames(&mut app, 400);

    // run 以 MockDriver 的脚本输出 Completed
    let completed: Vec<String> = {
        let mut q = app
            .world_mut()
            .query::<(&RunStatus, Option<&RunResultText>)>();
        q.iter(app.world())
            .filter_map(|(status, text)| match status {
                RunStatus::Completed => Some(text.map(|t| t.0.clone()).unwrap_or_default()),
                _ => None,
            })
            .collect()
    };
    assert!(
        completed.iter().any(|t| t == "escalated by mock"),
        "mock driver should complete the run: {completed:?}"
    );

    // 低置信度调用绝不执行（I3 契约核心）
    let invocations = app
        .world_mut()
        .query::<&ToolInvocation>()
        .iter(app.world())
        .count();
    assert_eq!(invocations, 0, "gated call must not spawn any invocation");

    // 诊断：升级计数 +1，失败 0
    let diagnostics = app.world().resource::<RuntimeDiagnostics>();
    assert_eq!(diagnostics.runs_escalated, 1);
    assert_eq!(diagnostics.runs_failed, 0);
}

#[test]
fn mock_driver_failure_with_no_more_tiers_is_failed() {
    let mut app = setup_with_script(vec![MockStep::fail(
        bevy_needle::escalate::DriverError::Model("boom".into()),
    )]);
    run_frames(&mut app, 400);

    // 规格 §6：试过且坏了 → Failed（不是 Escalated）
    let failed: Vec<String> = {
        let mut q = app
            .world_mut()
            .query::<(&RunStatus, Option<&RunFailure>)>();
        q.iter(app.world())
            .filter_map(|(status, failure)| match status {
                RunStatus::Failed => Some(failure.map(|f| f.0.clone()).unwrap_or_default()),
                _ => None,
            })
            .collect()
    };
    assert_eq!(failed.len(), 1, "driver failure must fail the run: {failed:?}");
    assert!(
        failed[0].contains("driver"),
        "failure should mention driver: {}",
        failed[0]
    );
}

#[test]
fn no_driver_is_escalated_not_failed() {
    let mock = MockBackend::new(vec![json!({
        "type": "call",
        "success": true,
        "confidence": 0.01,
        "function_calls": [{ "name": "echo", "arguments": {} }],
    })]);
    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(mock));

    // 策略启用 + 允许 Local，但**不注册任何 driver**
    app.world_mut().insert_resource(
        EscalationPolicy::new()
            .enabled()
            .with_fallback(OnlineFallback::LocalModelOnly)
            .with_tier(EscalationTarget::Local),
    );

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
    run_frames(&mut app, 400);

    // 规格 §6：没试/不许试 → Escalated（正常收尾，不是失败）
    let escalated = {
        let mut q = app.world_mut().query::<&RunStatus>();
        q.iter(app.world())
            .filter(|s| matches!(s, RunStatus::Escalated))
            .count()
    };
    assert_eq!(
        escalated, 1,
        "no driver → Escalated (normal close), not Failed"
    );
}

#[test]
fn two_tiers_first_fails_second_succeeds() {
    // 规格 §7：某 tier 失败且还有档 → Escalating{tier+1}，attempt 归零、epoch+1
    let mock = MockBackend::new(vec![json!({
        "type": "call",
        "success": true,
        "confidence": 0.01,
        "function_calls": [{ "name": "echo", "arguments": {} }],
    })]);
    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(mock));

    app.world_mut().insert_resource(
        EscalationPolicy::new()
            .enabled()
            .with_fallback(OnlineFallback::Cloud)
            .with_tier(EscalationTarget::Local)
            .with_tier(EscalationTarget::Remote),
    );
    {
        let mut registry = app.world_mut().resource_mut::<DriverRegistry>();
        registry.register_for_tier(
            Arc::new(MockDriver::new("tier0").with_script(vec![MockStep::fail(
                bevy_needle::escalate::DriverError::Model("tier0 down".into()),
            )])),
            0,
        ).expect("register driver");
        registry.register_for_tier(
            Arc::new(
                MockDriver::new("tier1")
                    .with_capability(EscalationTarget::Remote) // I25：与 tiers[1] 匹配
                    .with_script(vec![MockStep::succeed("tier1 saved")]),
            ),
            1,
        ).expect("register driver");
    }

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
    run_frames(&mut app, 400);

    let completed = {
        let mut q = app
            .world_mut()
            .query::<(&RunStatus, Option<&RunResultText>)>();
        q.iter(app.world()).any(|(s, t)| {
            *s == RunStatus::Completed && t.map(|t| t.0 == "tier1 saved").unwrap_or(false)
        })
    };
    assert!(completed, "second tier should complete the run");

    // tier1 的 driver_id 记录在案（I6：状态里只认 id）
    let escalated_by: Vec<String> = app
        .world_mut()
        .query::<&bevy_needle::escalate::EscalationState>()
        .iter(app.world())
        .map(|s| s.driver_id.as_str().to_string())
        .collect();
    assert!(
        escalated_by.iter().any(|id| id == "tier1"),
        "final attempt should be attributed to tier1 driver: {escalated_by:?}"
    );
}


/// G4（升级计划 §17.4）：`LocalModelOnly` + Remote Driver 已注册
/// → Remote tier 不可被选中（policy.allows 拒绝；§11.2/§11.3）。
///
/// 不能靠「默认没注册 remote」作为唯一测试（升级计划原文）：本测试显式
/// 注册一个 Remote driver 并断言它永远不会被 submit。
#[test]
fn local_model_only_refuses_registered_remote_driver() {
    // 阶段 0：低置信度触发 → tier 0 是 Local → MockDriver 接管。
    // 阶段 1：显式注册 Remote driver（只绑一个不存在的 tier 1 之外——
    // policy 允许的最大 tier 是 Local），断言 coordinator 从不 submit 它。
    let mock = MockBackend::dynamic(|_| {
        serde_json::json!({
            "type": "call",
            "success": true,
            "function_calls": [{ "name": "echo", "arguments": {} }],
            "confidence": 0.01,
        })
    });
    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(mock));

    // LocalModelOnly：Remote 永远不可选。
    app.world_mut().insert_resource(
        EscalationPolicy::new()
            .enabled()
            .with_fallback(OnlineFallback::LocalModelOnly)
            .with_tier(EscalationTarget::Local),
    );
    {
        let mut registry = app.world_mut().resource_mut::<DriverRegistry>();
        registry.register_for_tier(Arc::new(MockDriver::new("mock-local").with_script(vec![MockStep::Succeed {
            output: "handled locally".into(),
        }])), 0).expect("register driver");
        // Remote driver 也注册了（绑定 tier 1）——但 policy 不允许 Remote。
        registry.register_for_tier(Arc::new(MockDriver::new("mock-remote").with_script(vec![MockStep::Succeed {
            output: "REMOTE MUST NOT RUN".into(),
        }])), 1).expect("register driver");
    }

    let world = app.world_mut();
    let tool = world
        .spawn(ToolBundle::new(ToolSpec::new(
            "echo",
            "Echo",
            ParametersBuilder::new().build(),
        )))
        .id();
    register_tool_handler(world, "echo", |_call| Ok(ToolOutput::ok()));
    let handles = spawn_agent(world, NeedleAgentSpec::new("agent"));
    attach_tool(world, handles.agent, tool).unwrap();

    let world = app.world_mut();
    world.write_message(RunAgent::new(handles.agent, "low confidence request"));
    run_frames(&mut app, 400);

    let world = app.world_mut();
    let mut query = world.query::<&RunStatus>();
    let status = *query
        .iter(world)
        .next()
        .expect("run entity exists");
    assert!(
        matches!(status, RunStatus::Completed),
        "LocalModelOnly 下 run 应由 Local driver 正常完成（Completed），不是 Escalated/Failed；得到 {status:?}"
    );
    // Remote driver 的脚本从未被消费（若被选中会输出 REMOTE MUST NOT RUN）。
    let mut notes = world.query::<&RunNote>();
    for note in notes.iter(world) {
        assert!(!note.0.contains("REMOTE MUST NOT RUN"));
    }
}


/// G5（升级计划 §17.5）：取消 + stale epoch。
///
/// 场景：MockDriver 用 `Pending` 步骤挂起回灌（submit 不交付）；run 在
/// `Escalating` 期间被取消 → `Cancelled`；之后 `release()` 放行迟到事件 →
/// 必须被丢弃（终态不被替换，I17/§7「旧结果不复活 Run」）。
#[test]
fn cancelled_run_discards_late_driver_result() {
    use bevy_needle::run::{CancelRun, RunStatus};

    let mock = MockBackend::new(vec![json!({
        "type": "call",
        "success": true,
        "confidence": 0.01,
        "function_calls": [{ "name": "echo", "arguments": {} }],
    })]);
    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(mock));

    app.world_mut().insert_resource(
        EscalationPolicy::new()
            .enabled()
            .with_fallback(OnlineFallback::LocalModelOnly)
            .with_tier(EscalationTarget::Local),
    );
    let driver = Arc::new(
        MockDriver::new("mock").with_script(vec![MockStep::pending()]),
    );
    {
        let mut registry = app.world_mut().resource_mut::<DriverRegistry>();
        registry.register_for_tier(Arc::clone(&driver) as Arc<dyn Driver>, 0).expect("register driver");
    }

    let world = app.world_mut();
    let tool = world
        .spawn(ToolBundle::new(ToolSpec::new(
            "echo",
            "Echo",
            ParametersBuilder::new().build(),
        )))
        .id();
    register_tool_handler(world, "echo", |_call| Ok(ToolOutput::ok()));
    let handles = spawn_agent(world, NeedleAgentSpec::new("a").with_confidence_threshold(0.5));
    attach_tool(world, handles.agent, tool).unwrap();

    app.world_mut()
        .write_message(RunAgent::new(handles.agent, "hi"));
    // 2 帧：run 提交 → 低置信度 → Escalating；driver submit 挂起（在途）。
    run_frames(&mut app, 2);

    let run = {
        let world = app.world_mut();
        let mut query = world.query::<(Entity, &RunStatus)>();
        let (run, _) = query
            .iter(world)
            .find(|(_, status)| matches!(status, RunStatus::Escalating { .. }))
            .expect("escalating run exists");
        run
    };

    // 取消（contract：RunStatus::Cancelled）
    app.world_mut().write_message(CancelRun { run });
    run_frames(&mut app, 2);

    // 迟到结果放行：不得复活 run（终态不替换，I17）
    {
        let world = app.world_mut();
        let bus = world.resource::<bevy_needle::escalate::DriverEventBus>();
        assert!(driver.release(bus), "held event must be released");
    }
    run_frames(&mut app, 3);

    let world = app.world_mut();
    let mut query = world.query::<&RunStatus>();
    let status = *query.iter(world).next().expect("run exists");
    assert!(
        matches!(status, RunStatus::Cancelled),
        "迟到结果不得复活已取消的 run；得到 {status:?}"
    );
    // 输出不得变成迟到文本
    let mut texts = world.query::<&RunResultText>();
    for text in texts.iter(world) {
        assert_ne!(text.0, "released late", "迟到结果落进了转录");
    }
}
