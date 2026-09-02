//! # 09 · 诊断与调度插入点：观测与扩展插件管线
//!
//! 两个主题：
//!
//! 1. **RuntimeDiagnostics**：引擎状态、run/turn/工具调用计数、最近一次
//!    置信度与吞吐 —— 全部由插件系统增量维护，你只需读。
//! 2. **五段调度的插入点**：游戏系统可以 `before/after/in_set` 挂进插件
//!    管线的任何位置。本例演示全部合法挂点，并打印每段被调用的证据。
//!
//! **无需引擎**。
//!
//! ```bash
//! cargo run -p bevy_needle --example 09_diagnostics
//! ```

use bevy_app::App;
use bevy_ecs::{
    prelude::*,
    // 自定义调度标签需要这些。
    schedule::{IntoScheduleConfigs, ScheduleLabel},
};
use bevy_needle::prelude::*;
use serde_json::json;

// ── 自定义调度段示例：想在 RunCommit 之后做存档 ──
#[derive(ScheduleLabel, Clone, Debug, PartialEq, Eq, Hash)]
struct MyArchive;

/// "只运行一次"条件（Local 状态 per-system 持久）。
fn run_once_cond(mut has_run: Local<bool>) -> bool {
    let first = !*has_run;
    *has_run = true;
    first
}

/// 插入点序列记录。只在首轮记录（前 8 个字符）—— 足够验证挂点顺序，
/// 又不会随帧数无限增长（真实项目用固定容量 ring 或指标聚合）。
#[derive(Resource, Default)]
struct Probe(String);

// 每个挂点一个迷你系统 —— ResMut 的字段访问直接内联，无需辅助 trait。
fn probe_pre_run(mut p: ResMut<Probe>) {
    p.0.push('r');
}
fn probe_pre_dispatch(mut p: ResMut<Probe>) {
    if p.0.chars().count() < 8 {
        p.0.push('d'); // 只记首轮
    }
}
fn probe_post_resolve(mut p: ResMut<Probe>) {
    if p.0.chars().count() < 8 {
        p.0.push('R'); // 只记首轮
    }
}
fn probe_commit(mut p: ResMut<Probe>) {
    if p.0.chars().count() < 8 {
        p.0.push('C'); // 只记首轮
    }
}

fn main() {
    let mock = MockBackend::new(vec![
        json!({ "type": "call", "success": true, "confidence": 0.9,
                "function_calls": [{ "name": "ping", "arguments": {} }] }),
        json!({ "type": "respond", "success": true, "reasoning": "pong", "confidence": 0.4 }),
    ]);

    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(mock));

    // ──────────────────────────────────────────────────────────────────────
    // 插入点全景（时序从早到晚）：
    //
    //   Update（游戏常规系统；读上一帧插件写的消息）
    //   EngineSync          ← .before(EngineSyncSystems) 在重建前动手
    //   RunPreparation      ← .in_set(RunPreparationSystems) 参与准备段
    //   RunExecution:
    //     RunExecutionSystems      （引擎推进，独占系统所在）
    //     ToolDispatchSystems      （工具分发） ← .before/.after 精确插队
    //     RunResolutionSystems     （结果回喂）
    //   RunCommit           ← .in_set(RunCommitSystems) 参与落盘段
    //   Telemetry           ← add_systems(Telemetry, …) 自定义遥测
    // ──────────────────────────────────────────────────────────────────────
    app.insert_resource(Probe::default());
    // EngineSync 之前的挂点需要系统签名，用一个本地标记系统演示。
    fn probe_pre_sync(mut p: ResMut<Probe>) {
    p.0.push('P');
}
    // RunOnce 条件：每个探针系统只运行一次（首帧），序列自然封顶。
    // 这是 bevy 的惯用方案（bevy_ecs::schedule::RunOnce）。
    app.add_systems(
        EngineSync,
        probe_pre_sync.before(EngineSyncSystems).run_if(run_once_cond),
    );
    app.add_systems(
        RunPreparation,
        probe_pre_run.before(RunPreparationSystems).run_if(run_once_cond),
    );
    app.add_systems(
        RunExecution,
        probe_pre_dispatch.before(ToolDispatchSystems).run_if(run_once_cond),
    );
    app.add_systems(
        RunExecution,
        probe_post_resolve.after(RunResolutionSystems).run_if(run_once_cond),
    );
    app.add_systems(
        RunCommit,
        probe_commit.after(RunCommitSystems).run_if(|mut has_run: Local<bool>| { let r = !*has_run; *has_run = true; r }),
    );

    // 自定义调度段：注册 + 排进主循环（Telemetry 之后）。
    // 主循环顺序是插件 build 时写入 MainScheduleOrder 的；
    // 追加用 insert_after。
    app.add_schedule(bevy_ecs::schedule::Schedule::new(MyArchive));
    {
        let mut order = app.world_mut().resource_mut::<bevy_app::MainScheduleOrder>();
        order.insert_after(Telemetry, MyArchive);
    }
    app.add_systems(
        MyArchive,
        (
            // 系统体写成语句块，避免闭包链解析歧义。
            |mut p: ResMut<Probe>| {
                p.0.push('A');
            },
        )
            .run_if(run_once_cond),
    );

    // ── 标准装配 ──
    let world = app.world_mut();
    let tool = world
        .spawn(ToolBundle::new(ToolSpec::new(
            "ping",
            "Ping.",
            ParametersBuilder::new().build(),
        )))
        .id();
    register_tool_handler(world, "ping", |_call| Ok(ToolOutput::text("pong")));
    let handles = spawn_agent(world, NeedleAgentSpec::new("observed"));
    attach_tool(world, handles.agent, tool).unwrap();

    // 引擎缺失时的对照（注释掉 with_backend 即可体验）：
    // run 会以「needle 引擎不可用（…）。请设置 NEEDLE_LIB_PATH …」失败。

    app.world_mut().write_message(RunAgent::new(handles.agent, "ping"));
    for _ in 0..200 {
        app.update();
    }

    // ── 诊断面 ──
    let d = app.world().resource::<RuntimeDiagnostics>();
    println!("╭── RuntimeDiagnostics");
    println!("│  engine_path      = {:?}", d.engine_path);
    println!("│  engine_ready     = {}", d.engine_ready);
    println!("│  runs             = started {} / done {} / failed {} / cancelled {}",
        d.runs_started, d.runs_completed, d.runs_failed, d.runs_cancelled);
    println!("│  turns            = {}", d.turns_completed);
    println!("│  tool_calls       = total {} / ok {} / failed {}",
        d.tool_calls_total, d.tool_calls_completed, d.tool_calls_failed);
    println!("│  last_confidence  = {:?}", d.last_confidence);
    println!("│  last_decode_tps  = {:?}", d.last_decode_tps);
    println!("│  peak_ram_mb      = {:?}", d.peak_ram_mb);
    println!("│  channel_broken   = {}", d.channel_broken);
    println!("╰── summary: {}", d.summary());

    // probe 序列应为 P r d R C A（一次完整 run 穿过全部挂点）。
    let probe_seq = app.world().resource::<Probe>().0.clone();
    println!("插入点序列: {probe_seq}");
    assert!(probe_seq.contains('P'), "EngineSync 前挂点");
    assert!(probe_seq.contains('r'), "RunPreparation 挂点");
    assert!(probe_seq.contains('d'), "ToolDispatch 前挂点");
    assert!(probe_seq.contains('R'), "RunResolution 后挂点");
    assert!(probe_seq.contains('C'), "RunCommit 后挂点");
    assert!(probe_seq.contains('A'), "自定义调度段");

    // 单轮诊断断言。
    assert_eq!(d.runs_completed, 1);
    assert_eq!(d.turns_completed, 2);
    assert_eq!(d.tool_calls_total, 1);
    assert_eq!(d.tool_calls_completed, 1);
}
