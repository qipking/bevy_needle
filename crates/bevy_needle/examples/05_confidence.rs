//! # 05 · 置信度门控与升级路径（RunEscalation）
//!
//! Needle 的契约：**门控的是"是否执行调用"** —— 置信度低于门限时调用不被执行，
//! 宿主应"升级"（重问、换大模型、问用户），而不是硬着头皮执行。本插件把这条
//! 契约实现为：低于门限 → run 以 Failed 收尾 + 发出 [`RunEscalation`] 消息。
//!
//! ⚠️ **实测警告**（务必阅读）：本引擎置信度数值波动极大 —— 一次完全正确的
//! 调用可能只有 confidence=0.0002，也可能 0.9+，跨会话摆动覆盖两个数量级。
//! 因此示例默认**不设门限**；要启用门控，必须先用你自己的产品数据实测校准。
//!
//! 本例用 MockBackend 人为构造低置信度调用，验证门控的三要素：
//! 1. 调用**没有**被执行（无 ToolCallCompleted 消息、无 handler 调用）；
//! 2. `RunEscalation` 消息发出（附 confidence 与 threshold）；
//! 3. run 以 Failed 收尾，失败原因写明"低于门限"。
//!
//! ```bash
//! cargo run -p bevy_needle --example 05_confidence
//! ```

use bevy_app::{App, Update};
use bevy_ecs::prelude::*;
use bevy_needle::prelude::*;
use serde_json::json;

// 升级观察者：真实产品里这里接"重问/换大模型/弹窗问用户"。
#[derive(Resource, Default)]
struct EscalationLog {
    entries: Vec<(f64, f32)>, // (confidence, threshold)
}

// 执行观察者：验证低置信度调用确实"没被执行"。
#[derive(Resource, Default)]
struct ExecutedTools(Vec<String>);

fn watch_escalations(
    mut escalations: MessageReader<RunEscalation>,
    mut log: ResMut<EscalationLog>,
) {
    for e in escalations.read() {
        // run 实体也在消息里（e.run），可用于找到原始提问、决定重试策略。
        log.entries.push((e.confidence, e.threshold));
    }
}

fn watch_executions(mut completed: MessageReader<ToolCallCompleted>, mut log: ResMut<ExecutedTools>) {
    for m in completed.read() {
        log.0.push(m.call.name.clone());
    }
}

fn main() {
    let mock = MockBackend::new(vec![
        // 第 1 轮：低置信度调用 —— 应被门控拦截。
        json!({
            "type": "call", "success": true,
            "confidence": 0.05,              // 低于 0.5 门限
            "function_calls": [
                { "name": "fire_missiles", "arguments": { "count": 10 } }
            ],
        }),
        // 第 2 轮（模拟升级后重问）：高置信度 —— 正常执行。
        json!({
            "type": "call", "success": true,
            "confidence": 0.95,
            "function_calls": [
                { "name": "say_hello", "arguments": {} }
            ],
        }),
        json!({ "type": "respond", "success": true, "reasoning": "done", "confidence": 0.6 }),
    ]);

    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(mock));

    // 观察系统挂在 Update（读上一帧插件写入的消息）。
    app.insert_resource(EscalationLog::default());
    app.insert_resource(ExecutedTools::default());
    app.add_systems(Update, (watch_escalations, watch_executions));

    let world = app.world_mut();
    // "危险"工具：如果门控失效它会被执行 —— 断言会抓住。
    let missiles = world
        .spawn(ToolBundle::new(ToolSpec::new(
            "fire_missiles",
            "Fire missiles (dangerous!).",
            ParametersBuilder::new().integer("count", "how many").build(),
        )))
        .id();
    let hello = world
        .spawn(ToolBundle::new(ToolSpec::new(
            "say_hello",
            "Say hello.",
            ParametersBuilder::new().build(),
        )))
        .id();
    register_tool_handler(world, "fire_missiles", |call| {
        let count = call.args.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
        Ok(ToolOutput::json(json!({ "fired": count })))
    });
    register_tool_handler(world, "say_hello", |_call| Ok(ToolOutput::ok()));

    let handles = spawn_agent(
        world,
        // 门限 0.5：阈值选取是**产品决策**。原则：
        //   · 高风险动作 → 高门限（宁可多升级）
        //   · 低风险只读动作 → 低门限或无门限
        // 实测校准方法：收集真实查询的 confidence 分布，按"误拦率 vs 误执行率"
        // 的产品代价权衡取点。永远不要照抄别人的数值。
        NeedleAgentSpec::new("guarded-agent").with_confidence_threshold(0.5),
    );
    attach_tool(world, handles.agent, missiles).unwrap();
    attach_tool(world, handles.agent, hello).unwrap();

    // 第一条指令：触发低置信度调用 → 门控拦截 → 升级。
    app.world_mut()
        .write_message(RunAgent::new(handles.agent, "fire the missiles"));
    drive_until(&mut app, handles.agent, 400);

    // 升级路径示例：把原问题换个说法重问（真实产品可换更强模型/人工确认）。
    app.world_mut()
        .write_message(RunAgent::new(handles.agent, "just say hello instead"));
    drive_until(&mut app, handles.agent, 400);

    // ── 断言三要素 ──
    let escalations = app.world().resource::<EscalationLog>();
    assert_eq!(escalations.entries.len(), 1, "应恰好一次升级");
    let (confidence, threshold) = escalations.entries[0];
    assert_eq!(confidence, 0.05);
    assert_eq!(threshold, 0.5);

    let executed = app.world().resource::<ExecutedTools>();
    assert!(
        !executed.0.iter().any(|n| n == "fire_missiles"),
        "低置信度调用绝不能被执行！"
    );
    assert_eq!(executed.0, vec!["say_hello".to_string()], "只有高置信度调用被执行");

    // run 结果核对：第一个 Failed（注明门限），第二个 Completed。
    println!("✓ 低置信度调用被拦截（confidence={confidence} < threshold={threshold}），未执行");
    println!("✓ 升级后重问正常执行: {:?}", executed.0);
    println!();
    println!("提示：RunEscalation {:#?} 已写入 EscalationLog ——", escalations.entries);
    println!("      真实产品在这里接重试/更强模型/人工确认。");

    // 查看第一个 run 的失败原因文本。
    let mut q = app.world_mut().query::<(&RunStatus, Option<&RunFailure>)>();
    for (status, failure) in q.iter(app.world()) {
        if matches!(status, RunStatus::Failed)
            && let Some(f) = failure
        {
            println!("失败原因示例: {}", f.0);
        }
    }
}

fn drive_until(app: &mut App, agent: Entity, max_frames: usize) {
    let baseline = finalized_count(app, agent);
    for _ in 0..max_frames {
        app.update();
        if finalized_count(app, agent) > baseline {
            return;
        }
    }
}

fn finalized_count(app: &mut App, agent: Entity) -> usize {
    let mut q = app.world_mut().query::<(&RunOwner, &RunStatus, Option<&RunFinalized>)>();
    q.iter(app.world())
        .filter(|(o, s, f)| {
            o.0 == agent && matches!(s, RunStatus::Completed | RunStatus::Failed) && f.is_some()
        })
        .count()
}
