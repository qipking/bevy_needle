//! # 07 · MockBackend：不装引擎也能开发与测试
//!
//! 引擎是可选的运行时依赖。通过 `NeedleBackend` trait 注入任何实现，
//! 全部插件行为（run 状态机/工具分发/置信度门控/重绑/转录）都能在
//! 没有动态库的环境下开发与测试 —— CI 友好，协作者克隆即跑。
//!
//! 三种注入方式：
//! 1. `MockBackend::new(vec![...])`  —— 脚本回放（顺序固定，简单直观）；
//! 2. `MockBackend::dynamic(fn)`     —— 按输入生成（可测多轮回喂）；
//! 3. 自己实现 `NeedleBackend`       —— 例如包一层远程服务、录音回放、
//!    或 `--features link` 构建期链接的真引擎。
//!
//! ```bash
//! cargo run -p bevy_needle --example 07_mock_backend
//! cargo test -p bevy_needle          # tests/turn_loop.rs 就是这么写的
//! ```

use bevy_app::App;
use bevy_ecs::prelude::*;
use bevy_needle::prelude::*;
use serde_json::json;

fn main() {
    // ──────────────────────────────────────────────────────────────────────
    // 方式 1：脚本回放。信封就是引擎 JSON 的原样结构 —— 你可以先用官方
    // Python 绑定（或真引擎）跑一遍真实对话，把信封逐轮拷贝进脚本，
    // 便得到一个行为与真引擎一致的"录制回放"测试环境。
    // ──────────────────────────────────────────────────────────────────────
    let scripted = MockBackend::new(vec![
        json!({
            "type": "call",
            "success": true,
            "confidence": 0.8,
            "reasoning": "'lamp' -> device",
            "function_calls": [
                { "name": "toggle_light", "arguments": { "on": true } }
            ],
        }),
        json!({ "type": "respond", "success": true, "reasoning": "lamp on", "confidence": 0.7 }),
    ]);

    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(scripted));

    let world = app.world_mut();
    let lamp = world
        .spawn(ToolBundle::new(ToolSpec::new(
            "toggle_light",
            "Turn the lamp on or off.",
            ParametersBuilder::new().boolean("on", "new state").build(),
        )))
        .id();
    register_tool_handler(world, "toggle_light", |call| {
        Ok(ToolOutput::json(json!({ "lamp_is": call.args["on"] })))
    });
    let handles = spawn_agent(world, NeedleAgentSpec::new("scripted"));
    attach_tool(world, handles.agent, lamp).unwrap();

    app.world_mut().write_message(RunAgent::new(handles.agent, "turn on the lamp"));
    drive(&mut app, handles.agent, 300);

    println!("脚本回放 run 完成 ✓");

    // ──────────────────────────────────────────────────────────────────────
    // 方式 2：dynamic 闭包。输入是本轮喂给引擎的文本（首轮=用户提问，
    // 后续轮=工具结果 JSON）。按内容分支即可精确测试多轮逻辑。
    // ──────────────────────────────────────────────────────────────────────
    let dynamic = MockBackend::dynamic(|input| {
        // 首轮：要求调用；看到工具结果（以 '[' 开头的 JSON 数组）后收尾。
        if input.starts_with('[') {
            json!({ "type": "respond", "success": true, "reasoning": "saw results" })
        } else {
            json!({
                "type": "call", "success": true, "confidence": 0.9,
                "function_calls": [{ "name": "roll", "arguments": { "sides": 6 } }]
            })
        }
    });

    let mut app2 = App::new();
    app2.add_plugins(BevyNeedlePlugin::with_backend(dynamic));

    let world2 = app2.world_mut();
    let dice = world2
        .spawn(ToolBundle::new(ToolSpec::new(
            "roll",
            "Roll a dice.",
            ParametersBuilder::new().integer("sides", "number of sides").build(),
        )))
        .id();
    register_tool_handler(world2, "roll", |call| {
        let sides = call.args.get("sides").and_then(|v| v.as_i64()).unwrap_or(6);
        // 真·随机数也可以（handler 是你的代码）。
        let roll = (1..=sides).nth(rand_below(sides as u64) as usize).unwrap_or(1);
        Ok(ToolOutput::json(json!({ "rolled": roll })))
    });
    let h2 = spawn_agent(world2, NeedleAgentSpec::new("dynamic"));
    attach_tool(world2, h2.agent, dice).unwrap();

    app2.world_mut().write_message(RunAgent::new(h2.agent, "roll a dice"));
    drive(&mut app2, h2.agent, 300);

    // ──────────────────────────────────────────────────────────────────────
    // 方式 3：自定义后端。实现 4 个方法即可 —— 这里演示一个"把全部调用
    // 记录到 stderr 的调试代理"（真实场景：包一层鉴权/限流/录制）。
    // ──────────────────────────────────────────────────────────────────────
    let logging = LoggingBackend::new(MockBackend::new(vec![
        json!({ "type": "respond", "success": true, "reasoning": "via proxy" }),
    ]));
    let mut app3 = App::new();
    app3.add_plugins(BevyNeedlePlugin::with_backend(logging));
    let h3 = spawn_agent(app3.world_mut(), NeedleAgentSpec::new("proxied"));
    app3.world_mut().write_message(RunAgent::new(h3.agent, "through the proxy"));
    drive(&mut app3, h3.agent, 300);

    println!("全部三种注入方式跑通 ✓");
}

// 极简伪随机（示例用，勿用于生产）。
fn rand_below(n: u64) -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().subsec_nanos();
    nanos as u64 % n.max(1)
}

// ───────────────────────────────────────────────────────────────────────────
// 自定义后端骨架：委托内部后端并加横切逻辑。
// 需要 Arc（插件要求 Clone-able 的共享所有权吗？不需要 —— 但 worker 线程
// 需要 'static，内部包 Arc 即可）。
// ───────────────────────────────────────────────────────────────────────────
struct LoggingBackend {
    inner: MockBackend,
}

impl LoggingBackend {
    fn new(inner: MockBackend) -> Self {
        Self { inner }
    }
}

impl NeedleBackend for LoggingBackend {
    fn bind(
        &self,
        signature: u64,
        system: &str,
        tools_json: &str,
        tool_index: Option<&std::path::Path>,
    ) -> Result<(), NeedleError> {
        eprintln!("[logging-backend] bind sig={signature} tools={tools_json}");
        self.inner.bind(signature, system, tools_json, tool_index)
    }

    fn complete(
        &self,
        input: &str,
        max_new_tokens: u32,
        buffer: &mut [u8],
    ) -> Result<NeedleResponse, NeedleError> {
        eprintln!("[logging-backend] complete({input:.60}…)");
        self.inner.complete(input, max_new_tokens, buffer)
    }

    fn reset(&self) {
        eprintln!("[logging-backend] reset");
        self.inner.reset();
    }
}

fn drive(app: &mut App, agent: Entity, max_frames: usize) {
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
