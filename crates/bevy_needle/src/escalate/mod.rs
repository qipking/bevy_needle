//! 升级能力层（PR-B 核心，`escalate` feature；**无 rig 依赖**）。
//!
//! 使命（规格 §0）：让 bevy_needle 在「置信度不足」时，能把同一次 Run 的
//! 推理责任 handoff 给另一个 Driver，而 crate 核心永远不知道 rig 是什么。
//!
//! 模块划分：
//!
//! | 文件 | 职责 | 不变量 |
//! |---|---|---|
//! | [`driver`] | `Driver` trait / `DriverId` / `DriverError` / `DriverAttemptCtx` / `DriverOutcome` | I2/I5/I6/I16 |
//! | [`state`] | `EscalationState` 组件（tier/epoch/attempt/deadline/…） | I8/I10/I17 |
//! | [`registry`] | `DriverRegistry`（tier → DriverId 映射，持有 driver） | I6/I7 |
//! | [`bus`] | `DriverEventBus`（channel 回灌，ECS 只 drain） | I16 |
//! | [`coordinator`] | `DriverCoordinator` 系统（submit/drain/超时/取消/终态规则） | I9/I17/§6/§7 |
//! | [`mock`] | `MockDriver`（脚本化，无 rig 跑通升级链路） | §0 判据 |
//!
//! 接线（`app.rs`）：`DriverEventBus` / `DriverRegistry` 以 Resource 注入；
//! `driver_coordinator` 注册进 `RunResolutionSystems` 且
//! `.before(finalize_escalations)`（规格 §3 接线约定 1）。

pub mod bus;
pub mod coordinator;
pub mod driver;
pub mod mock;
pub mod registry;
pub mod state;

pub use bus::DriverEventBus;
pub use coordinator::driver_coordinator;
pub use driver::{
    AttemptHandle, Driver, DriverAttemptCtx, DriverError, DriverEvent, DriverId, DriverOutcome,
    DriverProtocolError, EscalationReason,
};
pub use mock::{MockDriver, MockStep};
pub use registry::{DriverRegistry, RegistryError};
pub use state::EscalationState;
