//! # 02 · 工具声明全解：schema 参数类型 × handler 三种写法 × 失败路径
//!
//! 工具是插件与模型之间的全部接口。schema 描述「能调什么」，handler 描述
//! 「调了之后做什么」。本例覆盖：
//!
//! 1. `ParametersBuilder` 的每一种参数类型（含会编译进解码语法的约束）；
//! 2. 手写 JSON Schema 的等价形式与校验时机；
//! 3. handler 的三种合法写法（闭包/函数/带状态闭包）；
//! 4. 失败路径：业务 Err、handler panic、未知工具 —— 三者都会被转成
//!    `{"error": …}` 回喂引擎，模型在下一轮自行纠正。
//!
//! **无需引擎**（MockBackend 脚本驱动）。
//!
//! ```bash
//! cargo run -p bevy_needle --example 02_tools
//! ```

use bevy_app::App;
use bevy_ecs::prelude::*;
use bevy_needle::prelude::*;
use serde_json::{json, Value};

// ───────────────────────────────────────────────────────────────────────────
// 2.3 handler 写法之二：普通函数。只要签名匹配就能注册，逻辑复杂时比闭包清晰。
// ───────────────────────────────────────────────────────────────────────────
/// `div` 工具的实现：演示"业务性失败"的标准返回方式。
fn div_handler(call: &ToolCall) -> ToolExecutionResult {
    // 参数读取是纯 serde_json 操作。
    let a = call.args.get("a").and_then(Value::as_f64).unwrap_or(0.0);
    let b = call.args.get("b").and_then(Value::as_f64).unwrap_or(1.0);

    if b == 0.0 {
        // 业务失败 → Err。信息会以 {"error": "..."} 回喂，模型能读懂并重试。
        // 注意与 panic 的区别：panic 是"程序 bug"，会被 catch_unwind 兜住，
        // 但正确做法是把可预期的失败写成 Err。
        return Err(ToolExecutionError::new(format!("cannot divide {a} by zero")));
    }
    Ok(ToolOutput::json(json!({ "quotient": a / b })))
}

// ───────────────────────────────────────────────────────────────────────────
// 2.3 handler 写法之三：带状态的闭包。状态用 Arc<Mutex<_>> 共享 ——
// handler 必须是 Send+Sync（插件可能在任意线程上下文调用它）。
// 场景：计数器、缓存、到外部系统的连接池句柄等。
// ───────────────────────────────────────────────────────────────────────────
// 带状态闭包的状态载体示例（本例正文未用到 —— 保留此注释说明模式：
// struct HitCounter(Mutex<u32>) + #[derive(Resource)]，闭包 move Arc 进去）。
#[allow(dead_code)]
#[derive(Resource, Default)]
struct HitCounter(std::sync::Mutex<u32>);

fn main() {
    // 引擎脚本：依次驱动四种调用，覆盖每个演示点。
    let mock = MockBackend::new(vec![
        // ① 调用"全参数类型"工具 —— 验证各类约束字段都能被模型给出。
        json!({ "type": "call", "success": true, "confidence": 0.9, "function_calls": [{
            "name": "player_action",
            "arguments": {
                "action": "jump",            // 枚举：只能取列出的值
                "power": 80,                 // 有界整数：0..=100
                "direction": [1.0, 0.0],     // 普通数字（无界，靠描述引导）
                "shout": true,               // 布尔
                "note": "make it flashy",    // 必选字符串
            }
        }]}),
        // ② 调用 div（业务失败路径：除零）。
        json!({ "type": "call", "success": true, "confidence": 0.9, "function_calls": [{
            "name": "div", "arguments": { "a": 1.0, "b": 0.0 }
        }]}),
        // ③ 引擎看到 div 的 error 后，自我纠正成 9/3。
        json!({ "type": "call", "success": true, "confidence": 0.9, "function_calls": [{
            "name": "div", "arguments": { "a": 9.0, "b": 3.0 }
        }]}),
        // ④ 调用不存在的工具（未知工具路径：插件立即生成 Failed invocation）。
        json!({ "type": "call", "success": true, "confidence": 0.9, "function_calls": [{
            "name": "teleport_to_moon", "arguments": {}
        }]}),
        // ⑤ handler panic 的演示工具；之后收尾。
        json!({ "type": "call", "success": true, "confidence": 0.9, "function_calls": [{
            "name": "risky", "arguments": {}
        }]}),
        json!({ "type": "respond", "success": true, "reasoning": "done", "confidence": 0.5 }),
    ]);

    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(mock));
    app.insert_resource(HitCounter::default());

    let world = app.world_mut();

    // ──────────────────────────────────────────────────────────────────────
    // 2.1 全参数类型一览。
    // 红线：写进 builder 的约束（枚举/区间/required）会编译成解码语法，
    //       模型在字节层面就被禁止越界；其余类型靠描述引导，语法不强制。
    // ──────────────────────────────────────────────────────────────────────
    let action_tool = world
        .spawn(ToolBundle::new(ToolSpec::new(
            "player_action",
            "Make the player perform an action in the game world.",
            // builder 是纯链式 API，最后 build() 产出 JSON Schema。
            ParametersBuilder::new()
                // 枚举（必选）：解码语法强制只能三选一。**45M 模型最可靠的参数形式**。
                .str_enum("action", &["jump", "crouch", "wave"], "action to perform")
                // 有界整数（必选）：minimum/maximum 进入语法。0-100 百分比类首选。
                .int_range("power", 0, 100, "action power percentage")
                // 数字（必选）：无语法界，适合坐标/倍率；靠 description 引导量纲。
                .number("direction_x", "x component of the direction vector")
                .number("direction_y", "y component of the direction vector")
                // 布尔（必选）。注意：模型对 bool 的输出偶有不稳定，
                // 关键开关建议拆成 xxx_on / xxx_off 两个零参工具（见 README 经验）。
                .boolean("shout", "whether to also shout")
                // 必选字符串：内容不受语法约束（自由文本是 45M 模型的能力边界）。
                .string("note", "a short note to display")
                // 可选字符串：模型在没有证据时会**省略**该字段（不是给空串）。
                .optional_string("emoji", "optional emoji reaction")
                // 可选布尔：同理，证据不足时省略。
                .optional_boolean("repeat", "whether to repeat the action")
                // 手动追加 required（一般不需要 —— 带 required 语义的 builder
                // 方法已自动登记；这个 API 用于手写 property 后补 required）。
                .required(&["action"])
                .build(),
        )))
        .id();

    // 手写 JSON Schema 完全等价 —— ToolSpec 的第三参数就是原始 schema。
    // 什么时候手写：需要 oneOf/anyOf、数组 item 约束、嵌套对象等 builder 未覆盖的能力。
    // 注意：**必须手写 required**。没有 required 时模型可以合法地产出空参数调用
    //（实测如此），这与上游 build_schema 从"无默认值参数"推断 required 的语义一致。
    let div_tool = world
        .spawn(ToolBundle::new(ToolSpec::new(
            "div",
            "Divide a by b.",
            json!({
                "type": "object",
                "properties": {
                    "a": { "type": "number", "description": "numerator" },
                    "b": { "type": "number", "description": "denominator" },
                },
                // 手写 schema 时不要漏掉 required！
                "required": ["a", "b"],
            }),
        )))
        .id();

    // 零参工具：ParametersBuilder::new().build() 产出 {"type":"object","required":[]}
    // —— 零参工具是 45M 模型命中率最高的形式，能用则用。
    let risky_tool = world
        .spawn(ToolBundle::new(ToolSpec::new(
            "risky",
            "A tool that panics (demo of panic safety).",
            ParametersBuilder::new().build(),
        )))
        .id();

    // 校验：normalized_parameters 会跑与 EngineSync 相同的校验 ——
    // 提前在代码里发现手写 schema 的错误（required 引用不存在的属性等）。
    if let Err(err) = world.get::<ToolSpec>(div_tool).unwrap().normalized_parameters() {
        panic!("手写 schema 校验失败: {err}");
    }

    // ── 注册 handler 三种写法 ──
    // ① 闭包（最常用）。
    register_tool_handler(world, "player_action", |call| {
        // 枚举值一定合法（语法保证），可以直接 expect 吗？不要 ——
        // mock/引擎版本差异可能给你意外值，防御性读取永远没错。
        let action = call.args.get("action").and_then(Value::as_str).unwrap_or("wave");
        let power = call.args.get("power").and_then(Value::as_i64).unwrap_or(50);
        let shout = call.args.get("shout").and_then(Value::as_bool).unwrap_or(false);

        // 这里就是游戏逻辑的位置：突变组件请通过消息回执（04_lifecycle），
        // 纯函数 handler 只做计算/校验/转发。
        Ok(ToolOutput::json(json!({
            "performed": action,
            "power": power,
            "shouted": shout,
        })))
    });

    // ② 普通函数。
    register_tool_handler(world, "div", div_handler);

    // ③ panic 演示：handler 里 panic 不会炸帧 —— 分发系统 catch_unwind
    //    并转为 Failed，错误文本回喂引擎。
    register_tool_handler(world, "risky", |_call| {
        panic!("boom! (这是故意演示的 panic)");
    });

    // 未知工具**不需要**注册：插件在解析调用时查 ToolRegistry，
    // 查不到直接生成 Failed invocation（"unknown tool: xxx" 回喂引擎）。

    let handles = spawn_agent(world, NeedleAgentSpec::new("tool-lab"));
    for tool in [action_tool, div_tool, risky_tool] {
        attach_tool(world, handles.agent, tool).unwrap();
    }

    // 发 5 条指令对应脚本 5 轮（每条消息一个 run，顺序执行）。
    let queries = [
        "jump with power 80",
        "divide 1 by 0",           // → Err 路径
        "divide 9 by 3",           // → 模型自纠后的成功
        "teleport to the moon",    // → 未知工具路径
        "do the risky thing",      // → panic 路径
    ];
    for q in queries {
        app.world_mut().write_message(RunAgent::new(handles.agent, q));
    }

    // 驱动到全部 run 终结。
    let mut frames = 0;
    loop {
        app.update();
        frames += 1;
        let pending = app
            .world_mut()
            .query::<(&RunStatus, Option<&RunFinalized>)>()
            .iter(app.world())
            .any(|(s, fin)| matches!(s, RunStatus::Queued | RunStatus::Running) || (matches!(s, RunStatus::Completed | RunStatus::Failed) && fin.is_none()));
        if !pending || frames > 2000 {
            break;
        }
    }

    // ── 结果检视：把每个 ToolInvocation 的最终状态打出来 ──
    // 模式：先在作用域内把数据收集成 owned 值，再退出借用做打印/断言。
    println!("\n╭── 调用账本（ToolInvocation 实体终态）");
    let ledger: Vec<(String, ToolInvocationStatus)> = {
        let mut q = app
            .world_mut()
            .query::<(&ToolInvocationCall, &ToolInvocationStatus)>();
        let mut rows: Vec<(String, ToolInvocationStatus)> = q
            .iter(app.world())
            .map(|(call, status)| (call.0.name.clone(), *status))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows
    };
    for (name, status) in &ledger {
        println!("│  {name:<18} {status:?}");
    }

    // 断言覆盖四个路径：
    // ① player_action 成功；② div 首次除零失败 + 自纠成功；
    // ③ 未知工具立即失败；④ panic 被兜住转 Failed。
    let ok = ledger.iter().filter(|(_, s)| matches!(s, ToolInvocationStatus::Completed)).count();
    let bad = ledger.iter().filter(|(_, s)| matches!(s, ToolInvocationStatus::Failed)).count();
    println!("╰── 成功 {ok} / 失败 {bad}");
    assert_eq!(ok, 2, "player_action 与自纠后的 div 应成功");
    assert_eq!(bad, 3, "除零、未知工具、panic 各贡献一次失败");
}
