//! # 03 · 多 Agent 与动态工具集：引擎单会话下的正确姿势
//!
//! 引擎是进程级单例（事实 F1）：同一时刻只服务一个 agent。插件的处理方式是
//! **自动排队 + 轮次边界重绑**。本例演示：
//!
//! 1. 两个 agent 各带不同工具集，交错提问 —— 插件自动串行；
//! 2. 运行中给 agent 换工具集（attach/detach）→ 签名变化 → 下轮自动重绑；
//! 3. agent 的其他可调参数（max_new_tokens / max_steps / system facts）。
//!
//! **无需引擎**（MockBackend）。
//!
//! ```bash
//! cargo run -p bevy_needle --example 03_agents
//! ```

use bevy_app::App;
use bevy_needle::prelude::*;
use serde_json::json;

fn main() {
    // 每轮都回 respond（最简脚本），重点不在工具而在 agent 调度。
    let mock = MockBackend::dynamic(|input| {
        // dynamic 模式：把输入回显进 reasoning，便于观察"哪次提问进了哪轮"。
        json!({
            "type": "respond",
            "success": true,
            "reasoning": format!("saw: {input}"),
            "confidence": 0.5,
        })
    });

    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(mock));

    // 作用域技巧：所有"借用 world 的装配代码"放进一个块，
    // 块结束即归还借用 —— 之后的循环/发送消息不再冲突。
    let (weather_tool, time_tool, weather_agent, time_agent) = {
        let world = app.world_mut();

    // ── 两个工具：分别将要绑定到两个 agent ──
    // 工具实体是全局的：同一个工具可以 attach 给多个 agent（绑定只是实体 id 列表）。
    let weather_tool = world
        .spawn(ToolBundle::new(ToolSpec::new(
            "get_weather",
            "Get the weather for a city.",
            ParametersBuilder::new().string("city", "city name").build(),
        )))
        .id();
    let time_tool = world
        .spawn(ToolBundle::new(ToolSpec::new(
            "get_time",
            "Get the current in-game time.",
            ParametersBuilder::new().build(),
        )))
        .id();

    // ── agent A：天气员。演示 system facts 与 max_new_tokens。──
    let weather_agent = spawn_agent(
        world,
        NeedleAgentSpec::new("weather-agent")
            // system facts = 环境事实，冒号分隔的 k/v；模型用它解析相对表达
            // （"tomorrow" 需要日期事实才合法）。再次强调：不放指令。
            .with_system_facts("device: game-console; region: eu-west; date: 2026-09-01")
            // 单轮生成的 token 上限。工具调用的 JSON 很短，默认 256 绰绰有余；
            // 只有大参数列表时才需要调大。
            .with_max_new_tokens(128),
    )
    .agent;
    attach_tool(world, weather_agent, weather_tool).unwrap();

    // ── agent B：报时员。演示 max_steps（工具回喂轮数上限）。──
    let time_agent = spawn_agent(
        world,
        // max_steps 限制的是"工具执行→回喂"的轮数（不是消息数）。
        // 到达上限时 run 以 Completed 收尾并带 RunNote 说明。
        NeedleAgentSpec::new("time-agent").with_max_steps(3),
    )
    .agent;
    attach_tool(world, time_agent, time_tool).unwrap();
    (weather_tool, time_tool, weather_agent, time_agent)
    }; // ← world 借用在此归还（let 解构语句结束）

    // ──────────────────────────────────────────────────────────────────────
    // 1) 交错提问：两个 agent 的 run 会**串行**执行（F1），结果互不串线。
    //    顺序 = 发消息顺序；先到先得，其余在插件内排队。
    // ──────────────────────────────────────────────────────────────────────
    app.world_mut()
        .write_message(RunAgent::new(weather_agent, "what's the weather in Paris?"));
    app.world_mut()
        .write_message(RunAgent::new(time_agent, "what time is it?"));

    // 驱动到两个 run 都终结。
    drive_until_all_done(&mut app, 2, 800);

    // ──────────────────────────────────────────────────────────────────────
    // 2) 动态换工具集：detach 天气工具、attach 时间工具 → 签名变化。
    //    引擎语义（F3）：重绑 = 重新 needle_init = KV 会话重置。
    //    插件保证重绑发生在**轮次边界**，不会打断任何在途轮次。
    // ──────────────────────────────────────────────────────────────────────
    {
        let world = app.world_mut();
        detach_tool(world, weather_agent, weather_tool).unwrap();
        attach_tool(world, weather_agent, time_tool).unwrap();
    }

    // 换绑后的第一条提问：内部会先重绑再解码（对 mock 无感，对真引擎即 init）。
    app.world_mut()
        .write_message(RunAgent::new(weather_agent, "what time is it?"));
    drive_until_all_done(&mut app, 3, 800);

    // ── 检视：每个 agent 的转录应当互不串线 ──
    for (label, agent) in [("weather-agent", weather_agent), ("time-agent", time_agent)] {
        // PrimarySession 组件存的是 agent 的会话实体。
        // 每轮循环内借用：读 session id 归还，再借 world 跑转录读取。
        let session = {
            let world = app.world_mut();
            world
                .get::<PrimarySession>(agent)
                .expect("agent 应有会话")
                .0
        };
        println!("╭── {label} 转录");
        for (role, text) in collect_transcript(app.world_mut(), session) {
            println!("│  {role:?}: {text}");
        }
        println!("╰──");
    }

    // 断言：3 个 run 全部完成（交错 2 + 换绑后 1）。
    let mut q = app.world_mut().query::<&RunStatus>();
    let completed = q
        .iter(app.world())
        .filter(|s| matches!(s, RunStatus::Completed))
        .count();
    assert_eq!(completed, 3, "三个 run 都应完成");
}

/// 驱动 app 直到「终结 run 数量达到 target」或帧数耗尽。
fn drive_until_all_done(app: &mut App, target_done: usize, max_frames: usize) {
    for _ in 0..max_frames {
        app.update();
        let mut q = app.world_mut().query::<(&RunStatus, Option<&RunFinalized>)>();
        let done = q
            .iter(app.world())
            .filter(|(s, f)| {
                matches!(s, RunStatus::Completed | RunStatus::Failed) && f.is_some()
            })
            .count();
        if done >= target_done {
            return;
        }
    }
}
