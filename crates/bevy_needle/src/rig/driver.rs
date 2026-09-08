//! RigDriver：把 rig-run 的 `AgentRun` 适配成 [`Driver`]（规格 §5）。
//!
//! 关键点：rig 的 `AgentRun` 是 **ECS 组件**（[`super::agent::RigDriverState`]），
//! 步进由 [`super::agent::rig_step_system`] 完成（I4：Bevy Run ⊃ AgentRun；
//! I3：工具执行在 ECS）。因此本 `Driver` 的 `submit` 只回执句柄——真正的
//! 接管发生在 rig 系统看到 `EscalationState.driver_id == rig` 且无
//! `RigDriverState` 时（下一帧）。异步模型调用留待 transport 层（worker 侧
//! block_on，I22）。

use std::sync::atomic::{AtomicU64, Ordering};

use crate::escalate::driver::{
    AttemptHandle, Driver, DriverAttemptCtx, DriverError, DriverId,
};

/// rig driver 的 [`DriverId`]（I6：RunResolutionSystems 只认它）。
pub const RIG_DRIVER_ID: DriverId = DriverId("rig");

static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

/// rig 适配 Driver。
#[derive(Debug, Default, Clone, Copy)]
pub struct RigDriver;

impl RigDriver {
    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn new() -> Self {
        Self
    }
}

impl Driver for RigDriver {
    fn id(&self) -> DriverId {
        RIG_DRIVER_ID
    }

    fn submit(
        &self,
        _ctx: DriverAttemptCtx,
        _bus: &crate::escalate::DriverEventBus,
    ) -> Result<AttemptHandle, DriverError> {
        // 实际接管由 rig_step_system 完成（AgentRun 是 ECS 组件，I4/I3）；
        // submit 只回执唯一句柄。ctx 里的转录/工具集快照由 rig 系统按 I14
        // 从 World 重读（collect_transcript 排序保证一致）。
        Ok(AttemptHandle(NEXT_HANDLE.fetch_add(1, Ordering::Relaxed)))
    }

    fn cancel(&self, _handle: AttemptHandle) {
        // 协作式取消：rig 系统按 RunStatus 与 epoch 失效丢弃在途结果（I17）。
        // 底层模型中断不保证（candle load 不可中止，规格 §7）。
    }
}
