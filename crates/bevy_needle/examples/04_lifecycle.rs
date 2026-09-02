//! # 04 · 轮次生命周期：一帧一帧地看 run 如何推进
//!
//! 01 讲了"一条指令的全链路"，本例放大中间状态：逐帧打印 run 的状态机
//! 字段（status/turn/awaiting/in-flight），配合 mock 脚本制造一次
//! 「调用 → 回喂 → 再调用 → 收尾」的三轮旅程，让你看清：
//!
//! - `RunStatus` 各状态的切换时机；
//! - `RunEngineInFlight`（在途引擎调用）何时出现/消失；
//! - `RunAwaitingTools`（等工具结果）何时出现/消失；
//! - `RunTurn`（轮次号）何时自增；
//! - `CancelRun` 的逻辑取消语义（不能打断引擎，但结果会被丢弃）。
//!
//! **无需引擎**。
//!
//! ```bash
//! cargo run -p bevy_needle --example 04_lifecycle
//! ```

use bevy_app::{App, Update};
use bevy_ecs::prelude::*;
use bevy_needle::prelude::*;
use serde_json::json;

// 观察帧计数器：给每行日志标注帧号。
#[derive(Resource, Default)]
struct Frame(usize);

// ── 逐帧状态观察系统：本例的核心 ──
fn observe_frames(
    mut frame: ResMut<Frame>,
    // 一次查询拿到 run 的全部"状态面"。
    runs: Query<(
        Entity,
        &RunStatus,
        &RunTurn,
        Option<&RunEngineInFlight>,
        Option<&RunAwaitingTools>,
        Option<&RunResultText>,
        Option<&RunFailure>,
        Option<&RunNote>,
    )>,
    // 关键：只在组件变化时运行/读取 —— 完成后的 run 不会每帧刷屏。
    // （Changed 过滤放在查询里，把"要不要打印"的决定权交给 change-detection。）
) {
    frame.0 += 1;
    for (entity, status, turn, in_flight, awaiting, result, failure, note) in &runs {
        // 终结态且已打印过会重复出现 —— 用状态变化判断：只在非 Completed/Failed
        // 或第一帧看到时打印。简化：跳过已完成且无 in-flight 的稳定帧。
        if matches!(status, RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled) {
            continue; // 终结态由最后的总结打印，不逐帧刷
        }
        // 组装一行紧凑的状态快照。
        let mut parts = vec![format!("{status:?}")];
        parts.push(format!("turn={}", turn.0));
        if in_flight.is_some() {
            parts.push("引擎在途".into());
        }
        if let Some(a) = awaiting {
            parts.push(format!("等工具 {}/{}", 0, a.expected));
        }
        if let Some(t) = result {
            if !t.0.is_empty() {
                parts.push(format!("结果={:?}", t.0));
            }
        }
        if let Some(f) = failure {
            parts.push(format!("失败={:?}", f.0));
        }
        if let Some(n) = note {
            parts.push(format!("注={:?}", n.0));
        }
        println!("  帧{:>3} run#{} ─ {}", frame.0, entity.index(), parts.join(" "));
    }
}

fn main() {
    // 三轮脚本：
    //   轮1 → 调用 echo ×2（两个调用同一轮：演示 expected=2 的等待）
    //   轮2 → 再调用 echo ×1（回喂后又调用）
    //   轮3 → 收尾
    let call2 = json!({
        "type": "call", "success": true, "confidence": 0.9,
        "function_calls": [
            { "name": "echo", "arguments": { "text": "first" } },
            { "name": "echo", "arguments": { "text": "second" } },
        ]
    });
    let mock = MockBackend::new(vec![
        call2,
        json!({ "type": "call", "success": true, "confidence": 0.9,
                "function_calls": [{ "name": "echo", "arguments": { "text": "third" } }] }),
        json!({ "type": "respond", "success": true, "reasoning": "all done", "confidence": 0.8 }),
    ]);

    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(mock));
    app.insert_resource(Frame::default());
    app.add_systems(Update, observe_frames);

    let world = app.world_mut();
    let tool = world
        .spawn(ToolBundle::new(ToolSpec::new(
            "echo",
            "Echo text.",
            ParametersBuilder::new().string("text", "input").build(),
        )))
        .id();
    // handler 记录收到的文本，顺便演示结果可以是任何 JSON。
    register_tool_handler(world, "echo", |call| {
        let text = call.args.get("text").and_then(|v| v.as_str()).unwrap_or("");
        Ok(ToolOutput::json(json!({ "length": text.chars().count() })))
    });
    let handles = spawn_agent(world, NeedleAgentSpec::new("observer"));
    attach_tool(world, handles.agent, tool).unwrap();

    // 发起 run。
    app.world_mut()
        .write_message(RunAgent::new(handles.agent, "count three echoes"));

    // 第一段：跑到第一个「等工具」状态出现。
    // 演示：消息 → Queued → Running[in-flight] → 调用实体生成 → awaiting。
    let mut frames = 0;
    loop {
        app.update();
        frames += 1;
        // 注意：QueryState::query 需要 &mut World —— 用 world_mut()。
        let awaiting = app
            .world_mut()
            .query::<Option<&RunAwaitingTools>>()
            .iter(app.world())
            .any(|a| a.is_some());
        if awaiting || frames > 50 {
            break;
        }
    }
    println!("── 轮1 已生成 2 个工具调用，处于 awaiting 状态 ──");

    // ── 逻辑取消演示前的说明：此刻 run 在等工具；引擎并不在跑。
    //    真正的"在途取消"场景是：submit 后引擎还在解码时发 CancelRun ——
    //    结果回来时 run 已是 Cancelled，事件被丢弃。
    //    本例等全部完成后单独演示"取消一个新 run"。

    // 第二段：跑到全部终结。
    let mut guard = 0;
    loop {
        app.update();
        guard += 1;
        let pending = app
            .world_mut()
            .query::<(&RunStatus, Option<&RunFinalized>)>()
            .iter(app.world())
            .any(|(s, f)| {
                matches!(s, RunStatus::Queued | RunStatus::Running)
                    || (matches!(s, RunStatus::Completed | RunStatus::Failed) && f.is_none())
            });
        if !pending || guard > 200 {
            break;
        }
    }

    // ── 收尾后检查累积结果 ──
    let executed: Vec<serde_json::Value> = {
        let mut q = app.world_mut().query::<&RunExecutedResults>();
        q.iter(app.world()).next().map(|r| r.0.clone()).unwrap_or_default()
    };
    println!("── 累积执行结果（对应 Python run() 的 results）──");
    for (i, value) in executed.iter().enumerate() {
        println!("  {i}: {value}");
    }
    assert_eq!(executed.len(), 3, "三轮共执行 3 次 echo");

    // ── 逻辑取消：新发一个 run，立即取消 ──
    // 语义：CancelRun 置 Cancelled 状态；引擎结果回来时被丢弃（不落转录）。
    // 引擎调用本身不可中断 —— 这是引擎事实决定的，插件只能保证"结果不生效"。
    println!("── 演示逻辑取消 ──");
    app.world_mut()
        .write_message(RunAgent::new(handles.agent, "will be cancelled"));
    // 同帧发取消（此时 run 还没提交到引擎，效果=直接终结；引擎在途时发亦可，结果会被丢弃）。
    // 先算出 run 实体再发消息 —— 避免同一表达式的双重可变借用。
    let newest_run = find_newest_run(&mut app);
    app.world_mut().write_message(CancelRun { run: newest_run });

    let mut g = 0;
    loop {
        app.update();
        g += 1;
        let cancelled_done = {
            let mut q = app
                .world_mut()
                .query::<(&RunStatus, &RunOwner, Option<&RunFinalized>)>();
            q.iter(app.world()).any(|(s, o, f)| {
                o.0 == handles.agent && matches!(s, RunStatus::Cancelled) && f.is_some()
            })
        };
        if cancelled_done || g > 100 {
            break;
        }
    }
    println!("── 取消完成 ──");
}

/// 找到最新的 run 实体（示例辅助：真实代码请自己保存 run 实体或用组件标记）。
fn find_newest_run(app: &mut App) -> Entity {
    let mut q = app.world_mut().query::<(Entity, &RunRequest)>();
    q.iter(app.world())
        .map(|(e, _)| e)
        .max_by_key(|e| e.to_bits())
        .expect("at least one run")
}
