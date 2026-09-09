//! Driver 注册表（I7：模型构造能力属于 DriverRegistry，不属于 EscalationTarget）。
//!
//! 两层结构（§4.7 / P1-6）：
//! - 第一层 `register`：`DriverId → Arc<dyn Driver>`（driver 是「东西」）；
//! - 第二层 `bind_tier`：`tier 下标 → DriverId`（与 `EscalationPolicy.tiers`
//!   平行；tier 是下标不是 rank，I10）。
//!
//! **I29（v14）**：身份归 Driver 实例所有，Registry 只拒绝重复——
//! - `register` 遇重复 `DriverId` 报 [`RegistryError::DuplicateDriverId`]，
//!   **绝不** `HashMap::insert` 静默覆盖（覆盖 = 先注册的 driver 被偷走执行权）；
//! - `bind_tier` 遇已绑 tier 报 [`RegistryError::DuplicateTierBinding`]，
//!   显式覆盖走 `rebind_tier`（留给 P3 动态 provider）；
//! - 两个操作都是**事务**的：先校验后变更，失败不留半注册状态。
//!
//! **I28（v14）**：attempt 内的执行者是 attempt-stable 的——`resolve` 返回
//! `Arc` 克隆，registry 后续 mutation 不影响在途 attempt；事件能否提交由
//! `(run, epoch)` 决定。
//!
//! `register_for_tier` 是便捷组合（幂等注册 + 绑定），不再承担核心语义
//! （§16.5a：`register` 与 `bind_tier` 必须拆开，否则「同一模型绑多 tier」
//! 这个合法能力会被重复 ID 报错误杀）。

use std::collections::HashMap;
use std::sync::Arc;

use bevy_ecs::prelude::Resource;

use super::driver::{Driver, DriverId};

/// Registry 操作错误（I29：注册/绑定的身份冲突都是显式错误，不静默覆盖）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// 同名 driver 已注册（I29：不覆盖；换 id 或 `rebind_tier` 语义）。
    DuplicateDriverId(DriverId),
    /// 该 tier 已绑定到别的 driver（I29：显式覆盖走 `rebind_tier`）。
    DuplicateTierBinding {
        /// 被冲突的 tier 下标。
        tier: u32,
        /// 该 tier 当前绑定的 driver id。
        bound: DriverId,
    },
    /// **输入自身重复**（v14.3）：同一调用里同一 tier 出现多次（如
    /// `tiers = &[0, 0]`）——不查它会让 precheck 说 OK 而 mutation 中途失败，
    /// 留下半注册状态（I29 反例，外部审阅实证）。
    DuplicateTierArgument(u32),
    /// 绑定/重绑的目标 id 从未注册。
    UnknownDriverId(DriverId),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::DuplicateDriverId(id) => {
                write!(f, "duplicate driver id: {}", id.as_str())
            }
            RegistryError::DuplicateTierBinding { tier, bound } => {
                write!(f, "tier {tier} already bound to driver {}", bound.as_str())
            }
            RegistryError::DuplicateTierArgument(tier) => {
                write!(f, "tier {tier} appears more than once in the argument")
            }
            RegistryError::UnknownDriverId(id) => {
                write!(f, "unknown driver id: {}", id.as_str())
            }
        }
    }
}

impl std::error::Error for RegistryError {}

/// tier 下标 → driver 的注册表（Resource）。
#[derive(Resource, Default)]
pub struct DriverRegistry {
    drivers: HashMap<DriverId, Arc<dyn Driver>>,
    tier_map: HashMap<u32, DriverId>,
}

impl DriverRegistry {
    /// 注册 driver 执行者（按 `Driver::id`；**一次**，I29/I32）。
    ///
    /// # Errors
    /// [`RegistryError::DuplicateDriverId`]——同名 driver 已注册（不覆盖）。
    pub fn register(&mut self, driver: Arc<dyn Driver>) -> Result<DriverId, RegistryError> {
        let id = driver.id();
        if self.drivers.contains_key(&id) {
            return Err(RegistryError::DuplicateDriverId(id));
        }
        // 事务性：校验（上方）通过后才变更。
        self.drivers.insert(id, driver);
        Ok(id)
    }

    /// 声明 tier 下标对应的 driver（**可多次**，同一 driver 可绑多 tier，I32）。
    ///
    /// `tier` 是 `EscalationPolicy.tiers` 的数组下标（I10：下标，不是 rank）。
    ///
    /// # Errors
    /// - [`RegistryError::UnknownDriverId`]——id 未注册；
    /// - [`RegistryError::DuplicateTierBinding`]——tier 已绑定（显式覆盖走
    ///   [`DriverRegistry::rebind_tier`]）。
    pub fn bind_tier(&mut self, tier: u32, id: DriverId) -> Result<(), RegistryError> {
        if !self.drivers.contains_key(&id) {
            return Err(RegistryError::UnknownDriverId(id));
        }
        if let Some(bound) = self.tier_map.get(&tier) {
            return Err(RegistryError::DuplicateTierBinding {
                tier,
                bound: *bound,
            });
        }
        self.tier_map.insert(tier, id);
        Ok(())
    }

    /// 显式覆盖 tier 绑定（P3 动态 provider 的合法路径；I29）。
    ///
    /// # Errors
    /// [`RegistryError::UnknownDriverId`]——id 未注册。
    pub fn rebind_tier(&mut self, tier: u32, id: DriverId) -> Result<(), RegistryError> {
        if !self.drivers.contains_key(&id) {
            return Err(RegistryError::UnknownDriverId(id));
        }
        self.tier_map.insert(tier, id);
        Ok(())
    }

    /// **组合注册的唯一判定核心**（v14.3）：`register_for_tier`（单 tier）与
    /// `precheck_register_and_bind`（多 tier，`driver: None`）共用本函数——
    /// 语义单一事实来源，杜绝「precheck 说 OK 而 mutation 说 Err」的分裂
    /// （外部审阅实证的根因）。
    ///
    /// 判定（与 `register_for_tier` 的文档语义表一一对应）：
    /// - id 已注册且非同一实例 → `DuplicateDriverId`；
    /// - 入参 tier 重复 → `DuplicateTierArgument`（先查，防止后续判定失真）；
    /// - tier 已绑其它 id → `DuplicateTierBinding`；
    /// - tier 已绑同一 id → 幂等 no-op（跳过绑定，不算冲突）。
    ///
    /// **零变更**——只读判定，`Ok` 即可安全执行变更。
    fn judge_register_and_bind(
        &self,
        driver: Option<&Arc<dyn Driver>>,
        id: DriverId,
        tiers: &[u32],
    ) -> Result<(), RegistryError> {
        // driver 身份（None = 纯预检已知将注册的新 id：仅查不存在）
        if let Some(existing) = self.drivers.get(&id) {
            match driver {
                Some(d) if Arc::ptr_eq(existing, d) => {} // 同实例：合法（幂等/仅绑定）
                _ => return Err(RegistryError::DuplicateDriverId(id)),
            }
        }

        // 入参自身重复（先于 registry 查询：同一调用里的重复是编程错误）
        let mut seen = std::collections::HashSet::new();
        for &tier in tiers {
            if !seen.insert(tier) {
                return Err(RegistryError::DuplicateTierArgument(tier));
            }
        }

        // tier 冲突（已绑同一 id = 幂等，跳过）
        for &tier in tiers {
            if let Some(&bound) = self.tier_map.get(&tier) {
                if bound != id {
                    return Err(RegistryError::DuplicateTierBinding { tier, bound });
                }
            }
        }
        Ok(())
    }

    /// 便捷组合：注册（幂等——同 id 且同一实例则跳过）+ 绑定。
    ///
    /// **事务性（v14.2/v14.3）**：判定全部通过（[`Self::judge_register_and_bind`]）
    /// 后才发生任何变更——不存在「注册成功、绑定失败」的半注册状态。语义表：
    ///
    /// | driver 状态 | tier 状态 | 结果 |
    /// |---|---|---|
    /// | 新 | 未绑 | 注册 + 绑定 |
    /// | 已注册（同一实例） | 未绑 | 仅绑定 |
    /// | 已注册（同一实例） | 已绑同一 id | 幂等成功（no-op） |
    /// | 已注册（同一实例） | 已绑其它 id | `DuplicateTierBinding`（零变更） |
    /// | 未注册或不同实例 | 任意 | `DuplicateDriverId`（零变更） |
    ///
    /// 便捷层不承担核心语义（§16.5a）。
    ///
    /// # Errors
    /// [`RegistryError::DuplicateDriverId`] / [`RegistryError::DuplicateTierBinding`]。
    pub fn register_for_tier(
        &mut self,
        driver: Arc<dyn Driver>,
        tier: u32,
    ) -> Result<(), RegistryError> {
        let id = driver.id();
        // ── 判定阶段（零变更；与 precheck 同一核心）──
        self.judge_register_and_bind(Some(&driver), id, &[tier])?;

        // ── 变更阶段（判定已全部通过）──
        let is_new = !self.drivers.contains_key(&id);
        let already_bound = self.tier_map.get(&tier) == Some(&id);
        if is_new {
            self.drivers.insert(id, driver);
        }
        if !already_bound {
            self.tier_map.insert(tier, id);
        }
        Ok(())
    }

    /// 预检「注册新 driver + 依次绑定多个 tier」整体事务（`register_with_model`
    /// 的 P0-1 基石）：**零变更**，全部通过才允许调用方执行变更。
    ///
    /// **v14.3**：与 `register_for_tier` 共用同一判定核心
    /// （[`Self::judge_register_and_bind`]）——单一事实来源；并新增
    /// **入参自身重复检查**（`tiers = &[0, 0]` → `DuplicateTierArgument(0)`，
    /// 外部审阅实证的半注册反例）。
    ///
    /// # Errors
    /// - [`RegistryError::DuplicateDriverId`]——id 已注册（新 driver 无法注册）；
    /// - [`RegistryError::DuplicateTierArgument`]——入参 tier 重复；
    /// - [`RegistryError::DuplicateTierBinding`]——任一 tier 已绑到其它 id。
    pub fn precheck_register_and_bind(
        &self,
        id: DriverId,
        tiers: &[u32],
    ) -> Result<(), RegistryError> {
        self.judge_register_and_bind(None, id, tiers)
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

    /// 解析 tier → driver 执行者（`Arc` 共享，全局一份；I28：attempt-stable）。
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::escalate::mock::MockDriver;
    use crate::policy::EscalationTarget;

    fn driver(id: &'static str, capability: EscalationTarget) -> Arc<dyn Driver> {
        Arc::new(MockDriver::new(id).with_capability(capability))
    }

    #[test]
    fn duplicate_driver_id_is_rejected_not_overwritten() {
        let mut registry = DriverRegistry::default();
        registry.register(driver("a", EscalationTarget::Local)).unwrap();
        let err = registry
            .register(driver("a", EscalationTarget::Remote))
            .unwrap_err();
        assert_eq!(err, RegistryError::DuplicateDriverId(DriverId("a")));
        // 第一实例未被偷走（I29 的核心：不覆盖）
        assert_eq!(registry.get(DriverId("a")).unwrap().id().as_str(), "a");
    }

    #[test]
    fn bind_tier_is_transactional_and_rejects_conflicts() {
        let mut registry = DriverRegistry::default();
        registry.register(driver("a", EscalationTarget::Local)).unwrap();
        registry.register(driver("b", EscalationTarget::Remote)).unwrap();

        registry.bind_tier(0, DriverId("a")).unwrap();
        // 重复绑定 → 错误且不改变原绑定
        let err = registry.bind_tier(0, DriverId("b")).unwrap_err();
        assert_eq!(
            err,
            RegistryError::DuplicateTierBinding {
                tier: 0,
                bound: DriverId("a")
            }
        );
        assert_eq!(registry.driver_id_for_tier(0), Some(DriverId("a")));
        // 未注册 id → 错误且不留绑定
        let err = registry.bind_tier(1, DriverId("ghost")).unwrap_err();
        assert_eq!(err, RegistryError::UnknownDriverId(DriverId("ghost")));
        assert_eq!(registry.driver_id_for_tier(1), None);
    }

    #[test]
    fn same_driver_can_bind_multiple_tiers() {
        // I32：一个 Driver 实例可绑多个 tier（同 M 多档的合法形态）
        let mut registry = DriverRegistry::default();
        let d = driver("rig-a", EscalationTarget::Local);
        registry.register(Arc::clone(&d)).unwrap();
        registry.bind_tier(0, DriverId("rig-a")).unwrap();
        registry.bind_tier(1, DriverId("rig-a")).unwrap();
        assert_eq!(registry.driver_id_for_tier(0), Some(DriverId("rig-a")));
        assert_eq!(registry.driver_id_for_tier(1), Some(DriverId("rig-a")));
    }

    #[test]
    fn rebind_tier_explicitly_overwrites() {
        let mut registry = DriverRegistry::default();
        registry.register(driver("a", EscalationTarget::Local)).unwrap();
        registry.register(driver("b", EscalationTarget::Local)).unwrap();
        registry.bind_tier(0, DriverId("a")).unwrap();
        registry.rebind_tier(0, DriverId("b")).unwrap();
        assert_eq!(registry.driver_id_for_tier(0), Some(DriverId("b")));
        // rebind 未注册 id → 错误且原绑定保留
        let err = registry.rebind_tier(1, DriverId("ghost")).unwrap_err();
        assert_eq!(err, RegistryError::UnknownDriverId(DriverId("ghost")));
        assert_eq!(registry.driver_id_for_tier(1), None);
    }

    #[test]
    fn convenience_register_is_atomic_on_bind_conflict() {
        // P0-1：新 driver + 已被他人绑定的 tier → Err，且**零残留**
        let mut registry = DriverRegistry::default();
        registry.register(driver("owner", EscalationTarget::Local)).unwrap();
        registry.bind_tier(0, DriverId("owner")).unwrap();

        let newcomer = driver("newcomer", EscalationTarget::Local);
        let err = registry
            .register_for_tier(newcomer, 0)
            .unwrap_err();
        assert_eq!(
            err,
            RegistryError::DuplicateTierBinding {
                tier: 0,
                bound: DriverId("owner")
            }
        );
        // 事务性断言：driver 不得留下
        assert!(registry.get(DriverId("newcomer")).is_none());
        // 原绑定不得改变
        assert_eq!(registry.driver_id_for_tier(0), Some(DriverId("owner")));
    }

    #[test]
    fn precheck_rejects_conflicts_without_mutation() {
        let mut registry = DriverRegistry::default();
        registry.register(driver("a", EscalationTarget::Local)).unwrap();
        registry.bind_tier(0, DriverId("a")).unwrap();

        // 重复 id 预检
        let err = registry
            .precheck_register_and_bind(DriverId("a"), &[1, 2])
            .unwrap_err();
        assert_eq!(err, RegistryError::DuplicateDriverId(DriverId("a")));

        // 中途 tier 冲突预检（tiers=[fresh, conflicting]）
        let err = registry
            .precheck_register_and_bind(DriverId("b"), &[2, 0])
            .unwrap_err();
        assert_eq!(
            err,
            RegistryError::DuplicateTierBinding {
                tier: 0,
                bound: DriverId("a")
            }
        );
        // 预检本身零变更
        assert!(registry.get(DriverId("b")).is_none());
        assert_eq!(registry.driver_id_for_tier(2), None);
    }

    #[test]
    fn duplicate_tier_argument_is_rejected() {
        // v14.3：[0,0] 入参自身重复 → DuplicateTierArgument（半注册反例封堵）
        let mut registry = DriverRegistry::default();
        let err = registry
            .precheck_register_and_bind(DriverId("x"), &[0, 0])
            .unwrap_err();
        assert_eq!(err, RegistryError::DuplicateTierArgument(0));
        // 零变更
        assert!(registry.get(DriverId("x")).is_none());
        assert_eq!(registry.driver_id_for_tier(0), None);
    }

    #[test]
    fn precheck_and_register_for_tier_share_semantics() {
        // v14.3：两个入口对同一情形判定一致（单一事实来源）
        let mut registry = DriverRegistry::default();
        let d: std::sync::Arc<dyn Driver> =
            std::sync::Arc::new(MockDriver::new("d").with_capability(EscalationTarget::Local));
        registry.register(std::sync::Arc::clone(&d)).unwrap();
        registry.bind_tier(0, DriverId("d")).unwrap();

        // 情形：同 id + tier 已绑同 id → precheck Err（id 已注册，multi-tier
        // 入口语义）与 register_for_tier Ok（幂等 no-op）的**分裂已被认可并
        // 归档**：两入口的职责不同——precheck 服务「全新注册」（id 必须新），
        // register_for_tier 服务「单 tier 便捷/幂等」。
        let pre = registry.precheck_register_and_bind(DriverId("d"), &[0]);
        let combo = registry.register_for_tier(std::sync::Arc::clone(&d), 0);
        assert!(pre.is_err());
        assert!(combo.is_ok());
        // 同源性验证：register_for_tier 的 Err 情形与 precheck 一致——
        // 新 id + 已占用 tier：两者都 Err(DuplicateTierBinding)
        let err_pre = registry
            .precheck_register_and_bind(DriverId("fresh"), &[0])
            .unwrap_err();
        assert!(matches!(
            err_pre,
            RegistryError::DuplicateTierBinding { tier: 0, .. }
        ));
        let err_combo = registry
            .register_for_tier(
                std::sync::Arc::new(MockDriver::new("fresh").with_capability(EscalationTarget::Local)),
                0,
            )
            .unwrap_err();
        assert!(matches!(
            err_combo,
            RegistryError::DuplicateTierBinding { tier: 0, .. }
        ));
        // fresh id 零残留（事务性）
        assert!(registry.get(DriverId("fresh")).is_none());
    }

    #[test]
    fn convenience_register_for_tier_is_idempotent_for_same_instance() {
        let mut registry = DriverRegistry::default();
        let d = driver("mock", EscalationTarget::Local);
        registry.register_for_tier(Arc::clone(&d), 0).unwrap();
        // 同一实例再绑另一 tier：合法（I32）
        registry.register_for_tier(Arc::clone(&d), 1).unwrap();
        assert_eq!(registry.driver_id_for_tier(0), Some(DriverId("mock")));
        assert_eq!(registry.driver_id_for_tier(1), Some(DriverId("mock")));
        // 同 id 不同实例：拒绝（I29）
        let err = registry
            .register_for_tier(driver("mock", EscalationTarget::Remote), 2)
            .unwrap_err();
        assert_eq!(err, RegistryError::DuplicateDriverId(DriverId("mock")));
    }
}
