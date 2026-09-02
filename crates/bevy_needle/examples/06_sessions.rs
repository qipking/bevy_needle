//! # 06 · 会话与转录：把对话历史留在 ECS 里
//!
//! 引擎自己维护 256-token 滑窗（工具集钉在 KV sink 上），所以**推理不依赖**
//! 我们的转录 —— 转录是插件做的**镜像持久化**，用于 UI 显示、存档、调试。
//!
//! 本例覆盖：
//! 1. `spawn_session` / 自动创建的主会话；
//! 2. run 生命周期自动写入的 User / Assistant 消息；
//! 3. 游戏代码手动追加自定义消息（`spawn_chat_message_now`）；
//! 4. `collect_transcript` 按序读取；
//! 5. `ResetAgent`：引擎会话回退 + 转录续写（不清空 —— 它是历史记录）。
//!
//! **无需引擎**。
//!
//! ```bash
//! cargo run -p bevy_needle --example 06_sessions
//! ```

use bevy_app::App;
use bevy_ecs::prelude::*;
use bevy_needle::prelude::*;
use serde_json::json;

fn main() {
    let mock = MockBackend::new(vec![
        // 两轮问答的应答（reasoning 会被转录为 Assistant 文本）。
        json!({ "type": "respond", "success": true, "reasoning": " answered: 42", "confidence": 0.5 }),
        json!({ "type": "respond", "success": true, "reasoning": " answered: reset done", "confidence": 0.5 }),
    ]);

    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(mock));

    // ── 独立会话：手动 spawn（用于不依赖 agent 的自定义对话面）──
    // 大多数场景用 agent 的 PrimarySession 即可；独立会话适合"全局事件日志"。
    let notebook = spawn_session(app.world_mut());

    // 手动写消息：`_now` 变体在独占上下文立即生效（不经命令队列）。
    spawn_chat_message_now(
        app.world_mut(),
        notebook,
        ChatMessageRole::System,
        "自定义系统日志：会话创建于示例启动时",
    );

    // ── agent 走标准问答 ──
    let handles = spawn_agent(
        app.world_mut(),
        NeedleAgentSpec::new("chat-agent")
            // system facts 不会进转录（引擎侧概念），转录只记录实际往来的消息。
            .with_system_facts("device: demo"),
    );

    app.world_mut()
        .write_message(RunAgent::new(handles.agent, "what is the answer?"));
    drive_until(&mut app, handles.agent, 300);

    // ── 游戏手动给 agent 会话追加消息（例如把玩家后续评论记入历史）──
    // 注意用 commands 变体（在系统里）或 _now 变体（在独占上下文）。
    spawn_chat_message_now(
        app.world_mut(),
        handles.session,
        ChatMessageRole::User,
        "（玩家插话：干得漂亮）",
    );

    // ── ResetAgent：引擎侧回退（KV 清零），工具集保留 ──
    // 转录是历史镜像，所以"清空引擎记忆"并不会删除已落盘的消息。
    // 重置后的下一轮：引擎从"全新会话"开始（mock 看不出区别，真引擎有效）。
    app.world_mut().write_message(ResetAgent { agent: handles.agent });
    app.world_mut()
        .write_message(RunAgent::new(handles.agent, "say something after reset"));
    drive_until(&mut app, handles.agent, 300);

    // ── 读取转录 ──
    println!("╭── agent 会话转录（含手动插入的玩家插话）");
    for (role, text) in collect_transcript(app.world_mut(), handles.session) {
        // 按入队序号排序 —— 这就是为什么消息实体带 ChatMessageSeq
        // （CommandQueue 的实体物化顺序不可靠，实体 id 排序会乱）。
        println!("│  {role:?}: {text}");
    }
    println!("╰──");

    println!("╭── 独立会话（自定义日志面）");
    for (role, text) in collect_transcript(app.world_mut(), notebook) {
        println!("│  {role:?}: {text}");
    }
    println!("╰──");

    // 断言：转录顺序 = 真实时序。
    let texts: Vec<String> = collect_transcript(app.world_mut(), handles.session)
        .into_iter()
        .map(|(_, t)| t)
        .collect();
    assert_eq!(texts[0], "what is the answer?");
    assert_eq!(texts[1], " answered: 42");
    assert_eq!(texts[2], "（玩家插话：干得漂亮）");
    assert_eq!(texts[3], "say something after reset");
    assert_eq!(texts[4], " answered: reset done");
}

fn drive_until(app: &mut App, agent: Entity, max_frames: usize) {
    let baseline = finalized(app, agent);
    for _ in 0..max_frames {
        app.update();
        if finalized(app, agent) > baseline {
            return;
        }
    }
}

fn finalized(app: &mut App, agent: Entity) -> usize {
    let mut q = app.world_mut().query::<(&RunOwner, &RunStatus, Option<&RunFinalized>)>();
    q.iter(app.world())
        .filter(|(o, s, f)| {
            o.0 == agent && matches!(s, RunStatus::Completed | RunStatus::Failed) && f.is_some()
        })
        .count()
}
