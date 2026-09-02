//! # 08 · 结构化抽取：把"解析指令"当"调用工具"用
//!
//! Needle 的核心洞见：**抽取与函数调用是同一个操作** —— 只声明一个 schema
//! 工具，语法约束保证返回的 `arguments` 一定符合结构。这让"从文本抽字段"
//! 天然获得 JSON 级可靠性，不需要正则与宽容解析。
//!
//! 本例演示两种用法：
//! 1. run 循环式（与普通工具一致，handler 原样返回参数）；
//! 2. 手工触发一轮 `complete` 的等价说明（插件没有单独的 extract API ——
//!    因为不需要：一个单工具 agent 就是抽取器）。
//!
//! **无需引擎**。
//!
//! ```bash
//! cargo run -p bevy_needle --example 08_extraction
//! ```

use bevy_app::App;
use bevy_needle::prelude::*;
use serde_json::json;

fn main() {
    // 真实引擎下这样喂文本：
    //   "GreenMart receipt: oat milk 3.50, total 7.75 paid by visa"
    // mock 脚本直接给出"模型会返回的信封"。
    let mock = MockBackend::new(vec![json!({
        "type": "call",
        "success": true,
        "confidence": 0.86,
        "reasoning": "'GreenMart' -> merchant; '7.75' -> total",
        "function_calls": [{
            "name": "receipt",                       // 唯一声明的工具 = 抽取目标结构
            "arguments": {
                "merchant": "GreenMart",
                "total": 7.75,
                "currency": "usd",
            }
        }],
    })]);

    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(mock));

    let world = app.world_mut();

    // 声明抽取目标结构。要点：
    //   · 这是 agent 唯一的工具 —— 语法上"只能产出 receipt"；
    //   · required 决定哪些字段必有（语法强制）；
    //   · 离题输入（与 schema 无关的文本）返回空调用 [] —— 契约即"未匹配"。
    let receipt_tool = world
        .spawn(ToolBundle::new(ToolSpec::new(
            "receipt",
            "A purchase receipt shared as text.",
            json!({
                "type": "object",
                "properties": {
                    "merchant": { "type": "string" },
                    "total":    { "type": "number" },
                    "currency": { "type": "string" },
                    "items": {
                        "type": "array",
                        "items": { "type": "object" },
                    },
                },
                "required": ["merchant", "total"],
            }),
        )))
        .id();

    // 抽取场景的 handler 通常原样返回参数（"执行"就是"确认收到"）。
    register_tool_handler(world, "receipt", |call| Ok(ToolOutput::json(call.args.clone())));

    let handles = spawn_agent(world, NeedleAgentSpec::new("extractor"));
    attach_tool(world, handles.agent, receipt_tool).unwrap();

    app.world_mut().write_message(RunAgent::new(
        handles.agent,
        "GreenMart receipt: oat milk 3.50, total 7.75 paid by visa",
    ));

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
        if done || frames > 300 {
            break;
        }
    }

    // 抽取结果 = 第一个（唯一）工具调用的 arguments。
    let extracted: Option<serde_json::Value> = {
        let mut q = app.world_mut().query::<&ToolInvocationOutput>();
        q.iter(app.world()).next().map(|o| o.0.value.clone())
    };

    // 断言用副本（分支里会移动 value）。
    let extracted_for_assert = extracted.clone();
    match extracted {
        Some(value) => {
            // serde_json 的索引不 panic（缺键返回 Value::Null）。
            println!("  商家: {}", value["merchant"].as_str().unwrap_or("?"));
            println!("  金额: {}", value["total"].as_f64().unwrap_or(0.0));
            println!("抽取结果: {value:#}");
        }
        None => println!("空调用 [] = 未匹配（离题输入的契约行为）"),
    }

    // 复用要点：抽取 agent 是**一次性**的（单工具、单文本）。
    // 批量抽取 = 每条文本 spawn 一个新 agent（引擎串行执行，互不干扰）；
    // 或 reset 后复用同一 agent（推荐 —— 省一次工具集重绑）。
    app.world_mut().write_message(ResetAgent { agent: handles.agent });

    assert!(extracted_for_assert.is_some());
    assert_eq!(extracted_for_assert.unwrap()["merchant"], "GreenMart");
}
