//! # 10 · External 分发策略：工具执行完全由你的系统接管
//!
//! 默认流程里，插件内置分发器执行 `ToolHandlers` 中注册的纯函数。当你需要
//! **访问 World** 的工具逻辑（spawn 实体、查组件、发自己的消息…）时，用
//! `ToolDispatchPolicy::External`：
//!
//! - 插件生成 `ToolInvocation`（Queued）后**不碰它**；
//! - 你的系统在 `ToolDispatchSystems` 集内（或之后）自行执行并写入终态；
//! - `resolve` 只认终态 —— 外部工具可以跨任意多帧完成（异步资产加载、
//!   网络请求……），回喂会等它。
//!
//! 本例对比两种策略，并演示"跨帧完成的外部工具"。
//!
//! **无需引擎**。
//!
//! ```bash
//! cargo run -p bevy_needle --example 10_external_dispatch
//! ```

use bevy_app::{App, Update};
use bevy_ecs::prelude::*;
use bevy_needle::prelude::*;
use serde_json::json;

// ── 外部工具的执行标记与载荷 ──

/// 外部工具完成时由你的系统插入（演示"跨帧"用的延迟计数）。
#[derive(Component)]
struct ExternalPending(u8);

/// 外部工具的执行结果（演示 game 侧自定义组件）。
#[derive(Component)]
struct SpawnedAt(u32);

fn main() {
    // 脚本：第 1 轮同时调 registry 工具与 external 工具（验证混合分发互不阻塞
    // —— registry 的先完成，resolve 等 external），第 2 轮收尾。
    let mock = MockBackend::new(vec![
        json!({ "type": "call", "success": true, "confidence": 0.9, "function_calls": [
            { "name": "registry_ping", "arguments": {} },
            { "name": "spawn_unit", "arguments": { "kind": "archer", "at": [3, 7] } },
        ]}),
        json!({ "type": "respond", "success": true, "reasoning": "both done", "confidence": 0.5 }),
    ]);

    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(mock));

    let world = app.world_mut();

    // 工具 1：普通 registry handler。
    let ping = world
        .spawn(ToolBundle::new(ToolSpec::new(
            "registry_ping",
            "Ping via the built-in registry handler.",
            ParametersBuilder::new().build(),
        )))
        .id();
    register_tool_handler(world, "registry_ping", |_call| {
        Ok(ToolOutput::text("pong"))
    });

    // 工具 2：External 策略 —— 不注册 handler。
    // 在 spawn 时就声明策略（也可以之后 insert 组件覆盖）。
    let spawn_unit = {
        let mut ec = world.spawn(ToolBundle::new(ToolSpec::new(
            "spawn_unit",
            "Spawn a game unit at a grid position.",
            ParametersBuilder::new()
                .str_enum("kind", &["archer", "knight", "mage"], "unit kind")
                .integer("at_x", "grid x")
                .integer("at_y", "grid y")
                .build(),
        )));
        ec.insert(ToolDispatchPolicy::External);
        ec.id()
    };

    let handles = spawn_agent(world, NeedleAgentSpec::new("mixed"));
    attach_tool(world, handles.agent, ping).unwrap();
    attach_tool(world, handles.agent, spawn_unit).unwrap();

    // ── 你的外部执行系统：挂在 ToolDispatchSystems 之后（或同集内）皆可。──
    // 职责：找到 Queued 且策略为 External 的调用 → 开始执行（这里模拟 3 帧工期）。
    app.insert_resource(FrameCounter::default());
    app.add_systems(Update, |mut f: ResMut<FrameCounter>| f.0 += 1);
    app.add_systems(
        Update,
        start_external_tools.after(bevy_needle::ToolDispatchSystems),
    );
    // 完成系统：倒计时归零 → 写终态。用 exclusive 系统（要 World）。
    app.add_systems(
        Update,
        finish_external_tools.after(start_external_tools),
    );

    app.world_mut()
        .write_message(RunAgent::new(handles.agent, "spawn an archer at 3,7 and ping"));

    let mut frames = 0;
    loop {
        app.update();
        frames += 1;
        let done = {
            let mut q = app
                .world_mut()
                .query::<(&RunStatus, &RunOwner, Option<&RunFinalized>)>();
            q.iter(app.world()).any(|(s, o, f)| {
                o.0 == handles.agent
                    && matches!(s, RunStatus::Completed | RunStatus::Failed)
                    && f.is_some()
            })
        };
        if done || frames > 400 {
            break;
        }
    }

    // ── 断言 ──
    // 1) registry 工具完成；
    // 2) external 工具在 3 帧后完成（期间 run 一直 awaiting）；
    // 3) 游戏组件被外部系统 spawn 出来（效果落地的证据）。
    let units: Vec<(String, u32)> = {
        let mut q = app.world_mut().query::<(&Name, &SpawnedAt)>();
        // Name 在这里复用作 kind 存储位（真实游戏用你自己的组件）。
        q.iter(app.world())
            .map(|(n, s)| (n.to_string(), s.0))
            .collect()
    };
    assert_eq!(units.len(), 1, "外部工具应 spawn 一个单位");
    assert_eq!(units[0].0, "archer");
    assert!(frames >= 3, "external 工具应跨帧完成（实际 {frames} 帧）");

    let executed: Vec<serde_json::Value> = {
        let mut q = app.world_mut().query::<&RunExecutedResults>();
        q.iter(app.world()).next().map(|r| r.0.clone()).unwrap_or_default()
    };
    assert_eq!(executed.len(), 2, "两个调用都回喂");
    println!("✓ 混合分发完成：registry + external 各 1 次，回喂 2 条结果");
    println!("  spawn 的单位: {units:?}（SpawnedAt 帧 {frames} 内）");
}

/// External 工具的"开始执行"：立即写 Running + 挂工期组件。
fn start_external_tools(
    mut commands: Commands,
    invocations: Query<
        (Entity, &ToolInvocationCall, &ToolInvocationStatus),
        (
            With<ToolInvocation>,
            Changed<ToolInvocationStatus>, // 只关心刚变成 Queued 的
        ),
    >,
    frames: Res<FrameCounter>,
) {
    for (entity, call, status) in &invocations {
        if *status != ToolInvocationStatus::Queued {
            continue;
        }
        if call.0.name != "spawn_unit" {
            continue; // 只处理自己认领的工具
        }
        // 标记执行中（真实逻辑：查地图、扣资源、入队动画…）。
        commands
            .entity(entity)
            .insert((ToolInvocationStatus::Running, ExternalPending(3)));
        // 顺手演示：工具系统也能有自己的副作用（spawn 游戏实体）。
        let kind = call.0.args.get("kind").and_then(|v| v.as_str()).unwrap_or("archer");
        commands.spawn((Name::new(kind.to_string()), SpawnedAt(frames.0)));
    }
}

/// External 工具的"完成"：工期归零 → 写终态（回喂数据自己定义）。
fn finish_external_tools(
    world: &mut World,
) {
    let mut to_finish: Vec<(Entity, serde_json::Value)> = Vec::new();
    {
        let mut q = world.query::<(
            Entity,
            &ToolInvocationCall,
            &mut ExternalPending,
        )>();
        for (entity, call, mut pending) in q.iter_mut(world) {
            pending.0 = pending.0.saturating_sub(1);
            if pending.0 == 0 {
                let x = call.0.args["at"][0].as_i64().unwrap_or(0);
                let y = call.0.args["at"][1].as_i64().unwrap_or(0);
                to_finish.push((entity, json!({ "spawned": true, "at": [x, y] })));
            }
        }
    }
    for (entity, output) in to_finish {
        // 插件导出的终态助手：写入 Completed + Output（resolve 会据此回喂）。
        complete_tool_invocation(world, entity, ToolOutput::json(output));
    }
}

#[derive(Resource, Default)]
struct FrameCounter(u32);
