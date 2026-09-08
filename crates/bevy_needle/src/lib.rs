#![deny(unsafe_code)]
#![cfg_attr(docsrs, feature(doc_cfg))]
//! **bevy_needle** — 把 [needle2](https://github.com/cactus-compute/needle)
//! （14 MB、45M 参数、纯本地推理的工具调用模型引擎）装进 Bevy ECS。
//!
//! 设计对齐 [`bevy_rig`](https://crates.io/crates/bevy_rig) 的精神：
//! provider/model/agent/tool/session/run 全部是实体与组件，调度是普通系统。
//!
//! # 特性
//!
//! - **零 Python**：直接 FFI（dlopen）引擎的四个 C 入口；
//! - **不卡帧**：阻塞解码跑在独立工作线程，ECS 每帧只做提交与收割；
//! - **可注入后端**：[`MockBackend`] 脚本化信封，完整轮次循环可在无引擎环境测试；
//! - **Needle 语义完整**：多轮工具回喂、`max_steps`、置信度门控（`RunEscalation`）、
//!   会话重置、调优 `.cact` 权重、工具检索索引缓存。
//!
//! # 快速上手
//!
//! ```no_run
//! use bevy_app::App;
//! use bevy_needle::prelude::*;
//! use serde_json::json;
//!
//! let mut app = App::new();
//! app.add_plugins(BevyNeedlePlugin::default());
//!
//! // 声明工具（schema 是数据；参数枚举/区间会被编译进解码语法）
//! let tool = app.world_mut().spawn(ToolBundle::new(ToolSpec::new(
//!     "set_volume",
//!     "Set the playback volume.",
//!     ParametersBuilder::new().int_range("percent", 0, 100, "volume percent").build(),
//! ))).id();
//!
//! // handler 是不需要 World 的纯函数
//! register_tool_handler(app.world_mut(), "set_volume", |call| {
//!     let percent = call.args.get("percent").and_then(|v| v.as_i64()).unwrap_or(0);
//!     Ok(ToolOutput::json(json!({ "volume_set": percent })))
//! });
//!
//! // agent = system facts + 工具集
//! let handles = spawn_agent(app.world_mut(), NeedleAgentSpec::new("agent"));
//! attach_tool(app.world_mut(), handles.agent, tool).unwrap();
//!
//! // 提问 = 发消息；游戏系统在 Update 读 ToolCallCompleted 施加效果
//! app.world_mut().write_message(RunAgent::new(handles.agent, "set volume to 80"));
//! ```
//!
//! # Crate features
//!
//! | feature | 默认 | 说明 |
//! |---|---|---|
//! | `dlopen` | ✅ | 运行时 dlopen 引擎（常规桌面流程）。关闭后 crate 不链接 libloading，引擎必须经 `with_backend` 注入（如构建期链接）。
//! | `escalate` | ❌ | 升级**能力层**（[`crate::escalate`]）：Driver 抽象 + Coordinator + 状态机，**无 rig 依赖**，MockDriver 即可跑通升级链路。
//! | `rig` | ❌ | rig **实现层**（[`crate::rig`]）：RigDriver 适配（AgentRun 手动步进 + 工具桥），git-pinned rig-core/rig-run + tokio。⚠️ crates.io 拒绝带 git 依赖的发布，rig 0.43 前带此 feature 的版本不可直接 publish。
//!
//! # MSRV
//!
//! 本 crate 的 MSRV 为 **1.98**（`rust-version = "1.98"`）：跟随 rig-core 0.42
//! 的 MSRV 预留升级路径（bevy 0.19 要求 ≥1.95）。
//!
//! # 升级路径（置信度门控 → Driver 接管）
//!
//! 置信度门控触发时，低置信度调用**不执行**，run 走 `Escalating { tier }`；
//! `escalate` feature 的 `DriverCoordinator` 按 [`crate::policy::EscalationPolicy`]
//! 的档位表接管（`rig` feature 提供 RigDriver），无可用 driver 或 policy 禁止
//! 时经 `finalize_escalations` 以 `Escalated` 正常收尾（不是失败）。`Failed`
//! 只留给引擎/调度错误与「试过且坏了」的 driver 失败。

pub mod agent;
pub mod app;
pub mod backend;
pub mod diagnostics;
#[cfg(feature = "escalate")]
pub mod escalate;
pub mod engine;
pub mod engine_index;
pub mod error;
#[cfg(feature = "dlopen")]
pub mod ffi;
#[cfg(feature = "dlopen")]
pub mod ffi_loading_guard;
pub mod needle_runtime;
pub mod policy;
pub mod prelude;
#[cfg(feature = "rig")]
pub mod rig;
pub mod run;
pub mod schema;
pub mod session;
pub mod tool;

pub use app::{
    BevyNeedlePlugin, EngineConfig, EngineSync, EngineSyncSystems, NeedleEngineConfig,
    NeedleEngineStatus, NeedleEngineStatusKind, RunCommit, RunCommitSystems, RunExecution,
    RunExecutionSystems, RunPreparation, RunPreparationSystems, Telemetry, ToolDispatchSystems,
};
pub use backend::{MockBackend, NeedleBackend};
pub use engine::{
    library_file_name, NeedleFunctionCall, NeedleResponse, DEFAULT_BUFFER_SIZE, ENGINE_VERSION,
};
pub use error::{NeedleError, NeedleRunError};
pub use run::RunExecutedResults;
