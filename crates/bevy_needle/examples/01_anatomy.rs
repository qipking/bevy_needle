//! # 01 · 解剖一次指令：bevy_needle 的最小完整闭环
//!
//! 这是理解本插件最重要的示例。它展示一条用户指令从「发消息」到「拿到回执」
//! 经历的全部环节，每一行都有解释。跑完后你会看到整条消息链的时序日志。
//!
//! **无需引擎动态库**：本例使用 [`MockBackend`]（脚本化信封），因此可以
//! 在任何机器上直接运行，包括 CI。
//!
//! ```bash
//! cargo run -p bevy_needle --example 01_anatomy
//! ```

// bevy_app 只提供 App/Plugin/Update —— 本 crate 刻意不依赖完整 bevy，
// 因此可以嵌进任何 bevy_app 生态的程序（包括纯 ECS 服务端）。
use bevy_app::{App, Update};

// bevy_ecs 的预导出：Component/Message/Query/Commands 等全部在这里。
use bevy_ecs::prelude::*;

// 插件的预导出：日常使用只需这一行。等价于逐个 use 各模块的公共项。
use bevy_needle::prelude::*;

// handler 与 mock 信封要用到 JSON。
use serde_json::json;

// ───────────────────────────────────────────────────────────────────────────
// 1. 一个「效果观察」系统：游戏代码与插件交互的主要方式就是读消息。
// ───────────────────────────────────────────────────────────────────────────
/// 有状态的消息读取器需要 `Resource` + `Default`（MessageReader 内部维护
/// 游标，游标存放在 Component/Resource 里才能跨帧记忆"我读到哪了"）。
#[derive(Resource, Default)]
struct LogBook {
    /// 收集完成日志，main 循环最后统一打印。
    lines: Vec<String>,
}

fn watch_effects(
    // MessageReader 会自动去重：同一条消息只会被读到一次（双缓冲机制）。
    mut completed: MessageReader<ToolCallCompleted>,
    mut failed: MessageReader<ToolCallFailed>,
    // 自家资源：存日志。
    mut book: ResMut<LogBook>,
) {
    // 每个成功调用都会到这里 —— 游戏在此时突变 UI/世界/NPC 组件。
    for message in completed.read() {
        // call_id 是稳定唯一 ID（run 实体 + 工具实体 + 自增 nonce 组合）。
        // args 是模型给出的参数（未经 handler 加工的原始输入）。
        // output 是 handler 返回的结果（也就是回喂给引擎的内容）。
        book.lines.push(format!(
            "  ✓ {}({}) → {}",
            message.call.name, message.call.args, message.output.value
        ));
    }
    // 失败的调用也会发出消息：handler 报错、panic、未知工具名都算失败。
    // 关键：失败结果同样会回喂给引擎（{"error": …}），模型下一轮可以自纠。
    for message in failed.read() {
        book.lines
            .push(format!("  ✗ {} → {}", message.call.name, message.error));
    }
}

fn main() {
    // ──────────────────────────────────────────────────────────────────────
    // 2. 组装 App。真实引擎只需把 with_backend 换成 BevyNeedlePlugin::default()。
    // ──────────────────────────────────────────────────────────────────────
    let mock = MockBackend::new(vec![
        // 脚本第 1 轮：引擎「要求调用工具」。
        // confidence 字段就是引擎信封里的原值；门控只在你配置阈值时生效。
        json!({
            "type": "call",                       // 信封类型：有调用
            "success": true,
            "confidence": 0.42,
            "reasoning": "'hello' -> text",       // 引擎给的参数推导链（唯一"自由文本"）
            "function_calls": [
                { "name": "echo", "arguments": { "text": "hello" } }
            ],
        }),
        // 脚本第 2 轮：引擎看到工具结果后「收尾」。
        // 约定：type != "call"（或空调用 []）表示会话这一轮结束了。
        json!({
            "type": "respond",
            "success": true,
            "reasoning": "echoed the text, all done",
            "confidence": 0.6,
        }),
    ]);

    let mut app = App::new();

    // 插件做了这些事（详见 docs/architecture.md §6）：
    //   · 注册全部资源与消息
    //   · 把 backend 包进工作线程（NeedleRuntime），阻塞解码不卡帧
    //   · 安装五段调度：EngineSync → RunPreparation → RunExecution
    //                      → RunCommit → Telemetry
    app.add_plugins(BevyNeedlePlugin::with_backend(mock));

    // 游戏系统放在 Update：插件调度在 Update 之后运行，
    // 因此本帧 Update 读到的是上一帧 RunExecution 写的消息（单帧延迟，见 §6.4）。
    app.add_systems(Update, watch_effects);
    app.insert_resource(LogBook::default());

    // ──────────────────────────────────────────────────────────────────────
    // 3. 声明工具。三要素：名字（模型调用它）、描述（模型据此选择）、
    //    参数 schema（被编译成解码语法 —— 模型不可能产出不合 schema 的参数）。
    // ──────────────────────────────────────────────────────────────────────
    let echo_tool = {
        let world = app.world_mut();
        world
            .spawn(ToolBundle::new(ToolSpec::new(
                // 工具名：引擎按名字调用；蛇形命名 + 英文动词（模型是英文训练的）。
                "echo",
                // 描述写给模型看：把用户可能说的话的关键词放进来（词面对齐）。
                "Echo the given text back to the caller.",
                // schema builder：每种参数类型见 02_tools.rs。
                ParametersBuilder::new()
                    .string("text", "the text to echo")
                    .build(),
            )))
            .id() // spawn 返回 EntityCommands；.id() 取实体句柄
    };

    // ──────────────────────────────────────────────────────────────────────
    // 4. handler：工具的"真正实现"。约束：纯函数、不访问 World、Send+Sync。
    //    它运行在主线程的分发系统里，panic 会被捕获并转成 Failed（不会炸帧）。
    // ──────────────────────────────────────────────────────────────────────
    register_tool_handler(app.world_mut(), "echo", |call| {
        // call.args 是 serde_json::Value；安全取字段的标准写法：
        let text = call
            .args
            .get("text")                      // Option<&Value>
            .and_then(|v| v.as_str())         // Option<&str>
            .unwrap_or_default();             // 缺字段时给空串（引擎约束下其实不会缺）

        // 返回值会原样回喂给引擎 —— 让模型看到"工具执行成功了，结果是…"。
        Ok(ToolOutput::json(json!({ "echoed": text })))
        // 失败时返回 Err(ToolExecutionError::new("原因"))，
        // 引擎会收到 {"error": "原因"} 并自行决定下一步。
    });

    // ──────────────────────────────────────────────────────────────────────
    // 5. 创建 agent。agent = 参数 + 工具集绑定 + 会话指针，全部是组件。
    //    spawn_agent 会同时创建 Session 实体并互相指向。
    // ──────────────────────────────────────────────────────────────────────
    let handles = {
        let world = app.world_mut();
        spawn_agent(
            world,
            // NeedleAgentSpec 全部字段有默认值；builder 逐项覆盖。
            // system facts 是"环境事实"（date/device/…），不要放指令 ——
            // 引擎只认事实，指令放这里会被当事实解读。
            NeedleAgentSpec::new("demo-agent"),
        )
        // 返回 AgentHandles { agent, session } 两个实体句柄。
    };
    // 工具绑定也是数据：attach 就是把工具实体 id 推进 agent 的 AgentToolRefs。
    attach_tool(app.world_mut(), handles.agent, echo_tool).expect("工具绑定应成功");

    // ──────────────────────────────────────────────────────────────────────
    // 6. 提问 = 发一条消息。插件把它物化为 run 实体并驱动整个循环。
    // ──────────────────────────────────────────────────────────────────────
    app.world_mut()
        .write_message(RunAgent::new(handles.agent, "echo the text hello"));

    // ──────────────────────────────────────────────────────────────────────
    // 7. 主循环。真实游戏里这就是你的帧循环；这里手动跑到 run 终结。
    // ──────────────────────────────────────────────────────────────────────
    let mut frames = 0;
    loop {
        // 一次 app.update() = 一帧：五段调度按序执行 + Update 系统。
        // 工作线程的解码是异步的，通常 1~3 帧内回来（mock 立即返回）。
        app.update();
        frames += 1;

        // 完成 = 状态进入 Completed/Failed 且转录已落盘（RunFinalized 标记）。
        let done = app
            .world_mut()
            .query::<(&RunStatus, &RunOwner, Option<&RunFinalized>)>()
            .iter(app.world())
            .any(|(status, owner, finalized)| {
                owner.0 == handles.agent                       // 只看这个 agent 的 run
                    && matches!(status, RunStatus::Completed | RunStatus::Failed)
                    && finalized.is_some()
            });
        if done || frames > 500 {
            break; // 上限防御：插件出 bug 时测试不悬挂
        }
    }

    // ──────────────────────────────────────────────────────────────────────
    // 8. 检视结果。三种途径：转录（会话镜像）/ 消息（效果流）/ 诊断（统计）。
    // ──────────────────────────────────────────────────────────────────────
    println!("╭── 消息流（游戏 effect 系统看到的）");
    // 先把数据 clone 出来，避免与后面的 world_mut 可变借用冲突。
    let log_lines = app.world().resource::<LogBook>().lines.clone();
    for line in &log_lines {
        println!("│{line}");
    }

    println!("╰── 会话转录（Session 实体里的镜像）");
    let session = handles.session;
    for (role, text) in collect_transcript(app.world_mut(), session) {
        // 角色枚举：System（环境事实）/ User / Assistant / Tool。
        println!("    {role:?}: {text}");
    }

    // RunExecutedResults 累积了本次 run 全部工具结果（对应 Python run() 的 results）。
    // QueryState::query 需要 &mut World；作用域限定借用范围。
    let executed: Vec<serde_json::Value> = {
        let mut q = app.world_mut().query::<&RunExecutedResults>();
        q.iter(app.world())
            .next()
            .map(|r| r.0.clone())
            .unwrap_or_default()
    };
    println!("executed results: {executed:?}");

    // 诊断资源：引擎状态、run/turn/调用计数、最近一次吞吐与置信度。
    println!(
        "诊断: {}",
        app.world().resource::<RuntimeDiagnostics>().summary()
    );
    println!("（共 {frames} 帧）");

    // 断言：保证示例行为与文档一致（也防止将来改坏）。
    assert_eq!(log_lines.len(), 1, "应该恰好一次成功调用");
    assert!(log_lines[0].contains("hello"));
    assert_eq!(executed.len(), 1);
}
