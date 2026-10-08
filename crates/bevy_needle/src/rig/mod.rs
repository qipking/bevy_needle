//! rig 实现层（`rig` feature）：Rig 0.44 适配（升级计划 §3.2/§7）。
//!
//! ```text
//! Needle3 native backend（启动期预热，I7）
//!      ↓ Needle3Worker（I22：唯一阻塞解码点）
//! Needle3Model = Model<Needle3Wire, Needle3Transport>   ← rig-core 0.44 契约
//!      ↓ ModelAdapter（上游 serve::adapters）
//! Serve（v23 §7.1：不实现旧 CompletionModel，不自写 Serve）
//!      ↓ rig-ecs Handlers::register（`rig-ecs` feature）
//! rig-ecs Agent / Run / Tool / Effect
//! ```
//!
//! 模块划分（§19.1 职责不混合）：
//!
//! | 文件 | 职责 |
//! |---|---|
//! | [`model`] | Wire/Transport/Model：Rig 请求 → Needle 请求；Needle 响应 → Rig 响应 |
//! | [`codec`] | 纯数据转换（无 World、无 runtime） |
//! | [`worker`] | 阻塞解码的 worker 隔离（I1/I22/I27） |
//! | [`adapter`] | `ModelAdapter` 注册入口（`rig-ecs` feature） |
//! | [`gate`] | 置信度门控 Intercept（拒绝点在 tool materialise 前，§27.4） |
//! | [`host`] | G2-B 双轨（ToolFn / register_world）+ Agent 装配（§27.3；`rig-ecs`） |
//! | [`security`] | NeedleSecurityPolicy 薄层（LocalModelOnly；§26.2/§26.6，`rig-ecs`） |
//!
//! 旧 0.42 rig-run 适配层（AgentRun 手动步进）按升级计划 §23 Phase 3+
//! 由 rig-ecs 原生 Run/Effect 接管后退休；本版本不保留旧模块。

pub mod adapter;
pub mod codec;
pub mod gate;
#[cfg(feature = "rig-ecs")]
pub mod host;
pub mod model;
#[cfg(feature = "rig-ecs")]
pub mod security;
pub mod worker;

pub use adapter::{needle3_model, NEEDLE_LABEL};
pub use gate::{ConfidenceGate, GATE_LAYER};
pub use codec::{
    choice_from_envelope, encode_payload, terminal_input, tool_results_input, NeedlePayload,
    SessionSlot,
};
pub use model::{Needle3Model, Needle3Transport, Needle3Wire};
pub use worker::{Needle3Worker, Submitted, WaitFuture, WorkerError};

#[cfg(feature = "rig-ecs")]
pub use host::{
    needle_model_key, register_needle_model,
    // G2-B 双轨（§27.3）：B1 纯工具轨 / B2 World 工具轨
    register_tool_fn, register_world_tool,
    spawn_agent, tool_key, AgentSpec,
};

#[cfg(feature = "rig-ecs")]
pub use security::{
    install_security_guard, NeedleSecurityPolicy, SecurityGuard, security_guard,
};
