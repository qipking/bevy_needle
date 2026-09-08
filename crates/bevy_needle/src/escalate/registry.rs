//! Driver 注册表（I7：模型构造能力属于 DriverRegistry，不属于 EscalationTarget）。
//!
//! - `register`：按 [`DriverId`] 注册 driver 执行者；
//! - `map_tier`：声明「tier 下标 → 哪个 Driver」（`EscalationPolicy.tiers[tier]`
//!   的能力类别 → DriverId 的映射在**注册时**完成，`RunResolutionSystems`
//!   运行时只按 tier 下标查表，永远不 match 具体 provider——I6）。
//!
//! 所有权（规格 §4.3）：driver 实例是**全局一份**（`Arc`），模型初始化是秒级、
//! 不可取消的（candle load 进 blocking pool 后 drop future 不停），故模型
//! 生命周期与帧级、可取消的 Run 解耦——由各 driver 内部全局缓存。

use std::collections::HashMap;
use std::sync::Arc;

use bevy_ecs::prelude::Resource;

use super::driver::{Driver, DriverId};

/// tier 下标 → driver 的注册表（Resource）。
#[derive(Resource, Default)]
pub struct DriverRegistry {
    drivers: HashMap<DriverId, Arc<dyn Driver>>,
    tier_map: HashMap<u32, DriverId>,
}

impl DriverRegistry {
    /// 注册 driver 执行者（按 `Driver::id`）。
    pub fn register(&mut self, driver: Arc<dyn Driver>) {
        self.drivers.insert(driver.id(), driver);
    }

    /// 声明 tier 下标对应的 driver。
    ///
    /// `tier` 是 `EscalationPolicy.tiers` 的数组下标（I10：下标，不是 rank）。
    pub fn map_tier(&mut self, tier: u32, driver: DriverId) {
        self.tier_map.insert(tier, driver);
    }

    /// 一键：注册 driver 并映射 tier。
    pub fn register_for_tier(&mut self, driver: Arc<dyn Driver>, tier: u32) {
        let id = driver.id();
        self.register(driver);
        self.map_tier(tier, id);
    }

    /// tier → DriverId（只返回 id，不暴露 driver；I6）。
    pub fn driver_id_for_tier(&self, tier: u32) -> Option<DriverId> {
        self.tier_map.get(&tier).copied()
    }

    /// 该 tier 是否有已注册 driver。
    pub fn has_driver(&self, tier: u32) -> bool {
        self.tier_map
            .get(&tier)
            .map(|id| self.drivers.contains_key(id))
            .unwrap_or(false)
    }

    /// 解析 tier → driver 执行者（`Arc` 共享，全局一份）。
    pub fn resolve(&self, tier: u32) -> Option<Arc<dyn Driver>> {
        let id = self.tier_map.get(&tier)?;
        self.drivers.get(id).cloned()
    }

    /// 按 id 取 driver（诊断/取消用）。
    pub fn get(&self, id: DriverId) -> Option<Arc<dyn Driver>> {
        self.drivers.get(&id).cloned()
    }
}

impl std::fmt::Debug for DriverRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DriverRegistry")
            .field("drivers", &self.drivers.keys().collect::<Vec<_>>())
            .field("tier_map", &self.tier_map)
            .finish()
    }
}
