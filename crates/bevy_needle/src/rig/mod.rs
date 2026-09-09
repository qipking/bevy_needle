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
//! - **I22**：能 `block_on()` 的类型只存在于 worker 侧（[`runtime`]）；
//! - **I26**：[`RigModelInbox`] 是模型回合的**唯一注入点**。
//!
//! # 接入（规格 §4.7 注册入口 + P1-7 裁决）
//!
//! rig 层的 inbox / 步进系统都是 `M: CompletionModel` 泛型（该 trait 是
//! RPITIT，**非 dyn-compatible**，P1-7 定案）——无法在不知 `M` 的情况下注册。
//! 宿主用 [`register_with_model`] 带启动期预热完成的模型接入：
//!
//! ```ignore
//! let model: Arc<MyCandleModel> = /* startup 期构造（秒级加载不进升级路径） */;
//! bevy_needle::rig::register_with_model(
//!     &mut app,
//!     DriverId("rig-candle"),
//!     EscalationTarget::Local,
//!     model,
//!     &[0],
//! )?; // tier 0
//! ```
//!
//! 注册内容：`RigModelInbox<M>` 资源（含 worker 线程）+ `RigDriver<M>`
//! 按 tier 入 `DriverRegistry` + `rig_step_system::<M>`（排在 coordinator
//! 之前）。

pub mod agent;
pub mod driver;
pub mod runtime;
pub mod session_bridge;
pub mod tool_bridge;
pub mod transcript;
pub mod transport;

use std::sync::Arc;

use bevy_app::App;
use bevy_ecs::schedule::IntoScheduleConfigs;

use crate::app::RunExecution;
use crate::escalate::{DriverRegistry, RegistryError};
use rig_core::completion::CompletionModel;


pub use agent::{
    model_turn_with_tools, rig_step_system, ModelJob, RigAwait, RigDriverIds, RigDriverState,
    RigModelInbox, RigModelTurn,
};
pub use driver::RigDriver;
pub use runtime::RigRuntime;
pub use session_bridge::RigSessionBridge;
pub use tool_bridge::{pending_to_tool_call, preresolved_content};
pub use transcript::transcript_to_history;

/// 把 rig driver（带启动期预热完成的模型）接入升级链路（规格 §4.7 注册入口）。
///
/// 身份与能力都是**显式参数**（I30：不存在固定常量）；身份冲突经
/// [`RegistryError`] 显式返回（I29：不覆盖）。
///
/// - `id`：本 driver 的身份（如 `"rig-candle"`；重复注册报
///   [`RegistryError::DuplicateDriverId`]）；
/// - `capability`：能力类别（I25——必须与所绑 tier 的 `EscalationTarget`
///   匹配，`Remote` 模型声明 `Local` 会让该 tier 永远不可用，§16.3）；
/// - `model`：启动期已构造完成的共享模型（`completion().await` 的执行体）；
///   构造失败 → 宿主不调用本函数 → 该 tier 无 driver → §6 走 `Escalated`；
/// - `tiers`：本 driver 绑定的 `EscalationPolicy.tiers` 下标（I10：下标；
///   **一个实例可绑多个 tier**，I32）。
///
/// **I32**：同一 `M` 多次调用本函数 → 复用同一 [`RigModelInbox<M>`]（不重建
/// worker）+ 同一 `rig_step_system::<M>`（不重复注册）+ 同一
/// [`RigDriverIds<M>`] 集合；不同 `M` → 各自独立的 inbox/worker/system。
///
/// # Errors
/// [`RegistryError`]——preflight 阶段（任何副作用之前）的身份冲突：id 已注册
/// 或任一 tier 已绑到其它 id（I29；失败 = 整体零变更）。
///
/// # Panics
/// `tiers` 为空（注册无意义，属编程错误）。
pub fn register_with_model<M: CompletionModel + 'static>(
    app: &mut App,
    id: crate::escalate::DriverId,
    capability: crate::policy::EscalationTarget,
    model: Arc<M>,
    tiers: &[u32],
) -> Result<(), RegistryError> {
    assert!(!tiers.is_empty(), "rig::register_with_model: tiers is empty");

    // P0-1（v14.2）：**preflight 先于一切副作用**——registry 校验不通过时，
    // inbox 不 spawn、RigDriverIds 不登记、系统不注册，零半注册状态（I29）。
    {
        let registry = app.world().resource::<DriverRegistry>();
        registry.precheck_register_and_bind(id, tiers)?;
    }

    // I32：get-or-create——同 M 复用既有 inbox（绝不重建/覆盖 worker 线程）。
    if app.world().get_resource::<RigModelInbox<M>>().is_none() {
        app.insert_resource(RigModelInbox::<M>::spawn(RigRuntime::lazy()));
    }

    let driver = {
        let inbox = app.world().resource::<RigModelInbox<M>>();
        Arc::new(RigDriver::new(id, capability, Arc::clone(&model), inbox))
            as Arc<dyn crate::escalate::Driver>
    };
    {
        let mut registry = app.world_mut().resource_mut::<DriverRegistry>();
        registry.register(driver)?; // 一次（preflight 已保证不冲突）
        for tier in tiers {
            registry.bind_tier(*tier, id)?; // 多次（preflight 已保证不冲突）
        }
    }

    // I31：per-M 身份集合登记（ensure_states/step_all 按它认领）。
    let first_install = app.world().get_resource::<RigDriverIds<M>>().is_none();
    app.init_resource::<RigDriverIds<M>>();
    app.world_mut()
        .resource_mut::<RigDriverIds<M>>()
        .insert(id);

    // 只在首次安装该 M 时注册步进系统（去重，I32）。
    if first_install {
        app.add_systems(
            RunExecution,
            rig_step_system::<M>
                .in_set(crate::app::RunResolutionSystems)
                .before(crate::escalate::driver_coordinator),
        );
    }
    Ok(())
}
