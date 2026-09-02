//! 用 [`MockBackend`] 驱动完整轮次循环的集成测试 —— 无需引擎动态库。
//!
//! 覆盖：单轮完成、多轮工具回喂、置信度门控（升级不执行）、max_steps 耗尽、
//! 未知工具错误回喂、会话重置、签名重绑。

use std::time::Duration;

use bevy_app::{App, Update};
use bevy_ecs::prelude::*;
use bevy_needle::prelude::*;
use serde_json::json;

fn envelope(kind: &str, calls: serde_json::Value) -> serde_json::Value {
    json!({ "type": kind, "success": true, "function_calls": calls, "confidence": 0.9 })
}

fn collect_replies(app: &mut App) -> Vec<String> {
    let mut replies = Vec::new();
    let mut q = app
        .world_mut()
        .query::<(&RunStatus, Option<&RunResultText>, Option<&RunFailure>)>();
    for (status, text, failure) in q.iter(app.world()) {
        if matches!(status, RunStatus::Completed) {
            replies.push(text.map(|t| t.0.clone()).unwrap_or_default());
        }
        if matches!(status, RunStatus::Failed) {
            replies.push(format!("[failed] {}", failure.map(|f| f.0.clone()).unwrap_or_default()));
        }
    }
    replies.sort();
    replies
}

fn run_until_idle(app: &mut App, max_frames: usize) {
    for _ in 0..max_frames {
        app.update();
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn single_turn_completes_without_calls() {
    let mock = MockBackend::new(vec![json!({
        "type": "respond",
        "success": true,
        "reasoning": "nothing to do",
        "confidence": 0.5,
    })]);
    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(mock));

    let handles = spawn_agent(app.world_mut(), NeedleAgentSpec::new("a"));
    app.world_mut().write_message(RunAgent::new(handles.agent, "hello"));
    run_until_idle(&mut app, 200);

    assert_eq!(collect_replies(&mut app), vec!["nothing to do".to_string()]);
}

#[test]
fn multi_turn_tool_feedback_loop() {
    // 第 1 轮：模型要调用 echo；第 2 轮：收尾
    let mock = MockBackend::new(vec![
        envelope("call", json!([{ "name": "echo", "arguments": { "text": "hi" } }])),
        json!({ "type": "respond", "success": true, "reasoning": "done", "confidence": 0.8 }),
    ]);
    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(mock));

    let world = app.world_mut();
    let tool = world
        .spawn(ToolBundle::new(ToolSpec::new(
            "echo",
            "Echo text",
            ParametersBuilder::new().string("text", "input").build(),
        )))
        .id();
    register_tool_handler(world, "echo", |call| {
        Ok(ToolOutput::json(json!({ "echoed": call.args["text"] })))
    });
    let handles = spawn_agent(world, NeedleAgentSpec::new("a"));
    attach_tool(world, handles.agent, tool).unwrap();

    app.world_mut().write_message(RunAgent::new(handles.agent, "say hi"));
    run_until_idle(&mut app, 400);

    let replies = collect_replies(&mut app);
    assert_eq!(replies, vec!["done".to_string()]);

    // 已执行结果应包含 handler 产物
    let executed: Vec<serde_json::Value> = {
        let mut q = app
            .world_mut()
            .query::<&RunExecutedResults>();
        q.iter(app.world())
            .next()
            .map(|r| r.0.clone())
            .unwrap_or_default()
    };
    assert_eq!(executed.len(), 1);
    assert_eq!(executed[0]["echoed"], "hi");
}

#[test]
fn confidence_gate_blocks_execution() {
    // 低置信度调用：应被拒绝执行（invocation 不产生）
    let mock = MockBackend::new(vec![json!({
        "type": "call",
        "success": true,
        "confidence": 0.01,
        "function_calls": [{ "name": "echo", "arguments": {} }],
    })]);
    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(mock));

    let world = app.world_mut();
    let tool = world
        .spawn(ToolBundle::new(ToolSpec::new(
            "echo", "Echo", ParametersBuilder::new().build(),
        )))
        .id();
    register_tool_handler(world, "echo", |_call| Ok(ToolOutput::ok()));
    let handles = spawn_agent(world, NeedleAgentSpec::new("a").with_confidence_threshold(0.5));
    attach_tool(world, handles.agent, tool).unwrap();

    app.world_mut().write_message(RunAgent::new(handles.agent, "hi"));
    run_until_idle(&mut app, 200);

    let replies = collect_replies(&mut app);
    assert_eq!(replies.len(), 1);
    assert!(replies[0].starts_with("[failed]"), "run should fail: {replies:?}");
    assert!(replies[0].contains("置信度"), "failure should mention confidence");
}

#[test]
fn unknown_tool_error_is_fed_back_and_run_finishes() {
    // 第 1 轮：调用不存在的工具（错误回喂）；第 2 轮：收尾
    let mock = MockBackend::new(vec![
        envelope("call", json!([{ "name": "ghost", "arguments": {} }])),
        json!({ "type": "respond", "success": true, "reasoning": "gave up", "confidence": 0.7 }),
    ]);
    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(mock));

    let handles = spawn_agent(app.world_mut(), NeedleAgentSpec::new("a"));
    app.world_mut().write_message(RunAgent::new(handles.agent, "do it"));
    run_until_idle(&mut app, 300);

    assert_eq!(collect_replies(&mut app), vec!["gave up".to_string()]);
}

#[test]
fn max_steps_bounds_the_loop() {
    // 引擎永远要求调用 echo —— max_steps=2 时应在两轮后强制收尾
    let mock = MockBackend::dynamic(|_input| {
        envelope("call", json!([{ "name": "echo", "arguments": { "text": "x" } }]))
    });
    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(mock));

    let world = app.world_mut();
    let tool = world
        .spawn(ToolBundle::new(ToolSpec::new(
            "echo", "Echo", ParametersBuilder::new().string("text", "input").build(),
        )))
        .id();
    register_tool_handler(world, "echo", |call| {
        Ok(ToolOutput::json(json!({ "echoed": call.args["text"] })))
    });
    let handles = spawn_agent(world, NeedleAgentSpec::new("a").with_max_steps(2));
    attach_tool(world, handles.agent, tool).unwrap();

    app.world_mut().write_message(RunAgent::new(handles.agent, "loop"));
    run_until_idle(&mut app, 600);

    let has_note = app
        .world_mut()
        .query::<&RunNote>()
        .iter(app.world())
        .any(|note| note.0.contains("max_steps"));
    assert!(has_note, "run should be capped with a max_steps note");
}

#[test]
fn engine_unavailable_fails_runs_with_actionable_error() {
    // 不注入后端、也保证引擎路径不存在：run 应以清晰原因失败而不是悬挂
    let mut app = App::new();
    app.insert_resource(NeedleEngineConfig(EngineConfig {
        library_path: Some(std::path::PathBuf::from("/nonexistent/libneedle.so")),
        ..EngineConfig::default()
    }));
    app.add_plugins(BevyNeedlePlugin::default());

    let handles = spawn_agent(app.world_mut(), NeedleAgentSpec::new("a"));
    app.world_mut().write_message(RunAgent::new(handles.agent, "hi"));
    run_until_idle(&mut app, 200);

    let replies = collect_replies(&mut app);
    assert_eq!(replies.len(), 1, "run should terminate");
    assert!(replies[0].starts_with("[failed]"), "should fail: {replies:?}");
}

#[test]
fn toolset_change_rebinds_engine() {
    // 修改 agent 的工具集 → 签名变化 → 第二次 run 仍能正常完成
    let mock = MockBackend::new(vec![json!({ "type": "respond", "success": true })]);
    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(mock));

    let (tool_a, tool_b, handles) = {
        let world = app.world_mut();
        let tool_a = world
            .spawn(ToolBundle::new(ToolSpec::new("a", "Tool A", ParametersBuilder::new().build())))
            .id();
        let tool_b = world
            .spawn(ToolBundle::new(ToolSpec::new("b", "Tool B", ParametersBuilder::new().build())))
            .id();
        let handles = spawn_agent(world, NeedleAgentSpec::new("a"));
        attach_tool(world, handles.agent, tool_a).unwrap();
        (tool_a, tool_b, handles.agent)
    };

    app.world_mut().write_message(RunAgent::new(handles, "first"));
    run_until_idle(&mut app, 200);
    let first = collect_replies(&mut app);
    assert_eq!(first.len(), 1);

    // 解绑 A、绑 B：签名变化
    {
        let world = app.world_mut();
        detach_tool(world, handles, tool_a).unwrap();
        attach_tool(world, handles, tool_b).unwrap();
    }

    app.world_mut().write_message(RunAgent::new(handles, "second"));
    run_until_idle(&mut app, 200);
    let second = collect_replies(&mut app);
    assert_eq!(second.len(), 2, "second run should also complete");
}

#[test]
fn reset_agent_sends_engine_reset() {
    // ResetAgent 消息不 panic、后续 run 仍能完成
    let mock = MockBackend::new(vec![
        json!({ "type": "respond", "success": true, "reasoning": "after reset", "confidence": 0.5 }),
    ]);
    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(mock));

    let handles = spawn_agent(app.world_mut(), NeedleAgentSpec::new("a"));
    app.world_mut().write_message(ResetAgent { agent: handles.agent });
    app.world_mut().write_message(RunAgent::new(handles.agent, "hi"));
    run_until_idle(&mut app, 300);

    assert_eq!(collect_replies(&mut app), vec!["after reset".to_string()]);
}

#[test]
fn schema_error_marks_agent_as_errored_not_panic() {
    // 工具 schema 非法（required 引用缺失的属性）：agent 快照记录错误，run 失败
    let mock = MockBackend::new(vec![json!({ "type": "respond", "success": true })]);
    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(mock));

    let world = app.world_mut();
    let bad_tool = world
        .spawn(ToolBundle::new(ToolSpec::new(
            "bad",
            "Bad schema",
            json!({ "type": "object", "required": ["ghost"] }),
        )))
        .id();
    let handles = spawn_agent(world, NeedleAgentSpec::new("a"));
    attach_tool(world, handles.agent, bad_tool).unwrap();

    app.world_mut().write_message(RunAgent::new(handles.agent, "hi"));
    run_until_idle(&mut app, 200);

    let index = app.world().resource::<AgentToolIndex>();
    assert!(index.error(handles.agent).is_some(), "snapshot error should be recorded");
}

#[test]
fn mock_counts_track_turns() {
    let mock = MockBackend::new(vec![
        envelope("call", json!([{ "name": "echo", "arguments": {} }])),
        json!({ "type": "respond", "success": true }),
    ]);
    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(mock));

    let world = app.world_mut();
    let tool = world
        .spawn(ToolBundle::new(ToolSpec::new("echo", "Echo", ParametersBuilder::new().build())))
        .id();
    register_tool_handler(world, "echo", |_call| Ok(ToolOutput::ok()));
    let handles = spawn_agent(world, NeedleAgentSpec::new("a"));
    attach_tool(world, handles.agent, tool).unwrap();

    app.world_mut().write_message(RunAgent::new(handles.agent, "hi"));
    run_until_idle(&mut app, 400);

    assert_eq!(collect_replies(&mut app), vec![String::new()]);

}
