//! 升级上下文组件（I8：`RunStatus` 只表示生命周期，上下文放 `EscalationState`）。
//!
//! 规格 §4.4：`tier` 与 `attempt` 必须分开——「tier 1 重试 3 次」是
//! `tier=1, attempt=3`，绝不能误表达成 `tier=4`。

use std::time::Instant;

use bevy_ecs::prelude::Component;

use super::driver::{AttemptHandle, DriverId, EscalationReason};

/// 一次升级流程的完整上下文（挂在 `RunStatus::Escalating` 的 run 实体上）。
///
/// 注意（I8）：本组件承载全部升级上下文，`RunStatus::Escalating { tier }`
/// 只保留生命周期标记，**不得**把 attempt/driver/error 塞进 `RunStatus` 变体。
#[derive(Component, Clone, Debug)]
pub struct EscalationState {
    /// `EscalationPolicy.tiers` 下标（I10：下标，不是 rank）。
    pub tier: u32,
    /// 当前执行者（I6：RunResolutionSystems 只认它）。
    pub driver_id: DriverId,
    /// 防 stale event（I17）——必须落进数据结构，不能只是文档约束。
    pub epoch: u64,
    /// 当前 tier 内的第几次尝试（重试不增 tier）。
    pub attempt: u32,
    /// `per_tier_timeout` 的到期时刻（P1-1 已裁决：进本组件，不扩展 RunStatus）。
    pub deadline: Instant,
    /// 升级流程起点（`total_budget` 锚点，规格 §7 总预算行为表）。
    pub started: Instant,
    /// 为什么升级。
    pub reason: EscalationReason,
    /// 在途 attempt 句柄（P0-3：放组件；协作式取消用）。
    pub handle: Option<AttemptHandle>,
}

impl EscalationState {
    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    ///
    /// ⚠️ `deadline` 是**占位值**（当前时刻）：直接手插本组件的调用方
    /// （如测试）必须自行覆盖 `deadline` 与 `started`；生产路径由
    /// coordinator 的 `submit_tier` 按策略预算覆盖。
    pub fn new(tier: u32, driver_id: DriverId, epoch: u64, reason: EscalationReason) -> Self {
        let now = Instant::now();
        Self {
            tier,
            driver_id,
            epoch,
            attempt: 0,
            deadline: now,
            started: now,
            reason,
            handle: None,
        }
    }
}
