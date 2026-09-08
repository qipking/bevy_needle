//! rig 实现层（`rig` feature）：RigDriver 适配。
//!
//! 使命（规格 §5）：把 rig-run 的 `AgentRun`（sans-I/O 协议状态机）接进
//! bevy_needle 的升级链路。关键不变量：
//!
//! - **I4**：`Bevy Run ⊃ AgentRun`——`AgentRun` 只是**一次 attempt 的内部状态**
//!   （[`crate::rig::agent::RigDriverState`] 组件），不是 Run 本身；
//! - **I3**：rig 的 `CallTools` 翻译成 ECS `ToolInvocation` 实体，绝不
//!   在 rig async 回调里直接调 `ToolHandlerFn`（工具桥见 [`tool_bridge`]）；
//! - **I13**：依赖 rig-run（协议层），不依赖 rig-agent（它拥有 agent loop /
//!   工具执行生命周期，而我们只要 `AgentRun` 这个 protocol state machine）；
//! - **I22**：能 `block_on()` 的类型只存在于 worker 侧（[`runtime`]）。
//!
//! 模型调用边界（PR-B 范围说明）：`CallModel` 的真实 provider 执行（异步、
//! worker 侧 block_on）留待下一阶段——本层提供 [`agent::RigModelInbox`] 回灌
//! seam（测试直接注入 [`rig_run::ModelTurn`]）；工具路径与终态路径已完整可测。

pub mod agent;
pub mod driver;
pub mod runtime;
pub mod session_bridge;
pub mod tool_bridge;
pub mod transcript;
pub mod transport;

pub use agent::{
    model_turn_with_tools, rig_step_system, RigAwait, RigDriverState, RigModelInbox, RigModelTurn,
};
pub use driver::{RigDriver, RIG_DRIVER_ID};
pub use runtime::RigRuntime;
pub use session_bridge::RigSessionBridge;
pub use tool_bridge::{pending_to_tool_call, preresolved_content};
pub use transcript::transcript_to_history;
