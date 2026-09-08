//! 升级策略：宿主对"置信度门控触发之后怎么办"的**纯决策**（PR-A 契约面）。
//!
//! 本模块**无 `#[cfg]`、无 rig 依赖**——"要不要联网升级"这个决定必须在
//! 没有 rig 的时候也能做（计划 §5.3）。策略只描述决策；执行者是 `Driver`
//! （见 `escalate` feature 下的 `DriverRegistry` / `DriverCoordinator`），
//! 二者正交（不变量 I5：Policy 是纯决策，Driver 是纯执行）。
//!
//! - [`OnlineFallback::Never`]：永不升级到引擎之外（当前默认）；
//! - [`OnlineFallback::LocalModelOnly`]：允许落到本地第二档；
//! - [`OnlineFallback::Cloud`]：允许云端档（宿主自担 API key 泄漏防护）。
//!
//! # 档位语义（不变量 I10，必读）
//!
//! `tier` 是 [`EscalationPolicy::tiers`] 的**数组下标**，不是 capability rank。
//! `[Local(Candle), Local(GPTQ), Remote]` 中两个 Local 的 rank 相同（都是 1），
//! 若按 `tier = rank` 推导，第二个 Local 档永远选不中。正确做法：`tier` 就是
//! 下标，单调递增指「下标只增不减」；[`EscalationTarget::rank`] 只用于
//! capability 上限判断（如 `fallback = LocalModelOnly` 时拒绝 Remote）。
//!
//! # 0.2.0 → PR-B breaking（随 PR-B 一次性完成，见规格 §2.3）
//!
//! [`EscalationTarget`] 原形状 `Needles / Local(LazyModel) / Remote(LazyModel)`
//! 有两个缺陷：① 泄漏 execution（policy → LazyModel → Rig/OpenAI/Candle，
//! 违反 I5/I7）；② `Needles` 复数与 `rank()` 文档的单数不一致。改为
//! `Needle / Local / Remote`（无载函），模型构造能力归 `DriverRegistry`
//! （`escalate` 层）。`rank()` 保留，语义收窄为 capability 序。

use bevy_ecs::prelude::*;
use std::time::Duration;

/// 允许的升级目标上限（能力档）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum OnlineFallback {
    /// 永不升级到引擎之外：升级语义在 crate 内闭环（低置信度调用不执行并终结）。
    Never,
    /// 只允许本地模型档（不联网）。
    LocalModelOnly,
    /// 允许云端档（宿主自担密钥防护与限流；见模块级文档）。
    Cloud,
}

impl OnlineFallback {
    /// 对应 [`EscalationTarget`] 的能力档位上限（0=Needle，1=Local，2=Remote）。
    fn ceiling_rank(self) -> u8 {
        match self {
            OnlineFallback::Never => 0,
            OnlineFallback::LocalModelOnly => 1,
            OnlineFallback::Cloud => 2,
        }
    }
}

/// 一个升级档位：`Needle → Local → Remote` 的**能力类别**描述（纯数据）。
///
/// PR-B breaking（规格 §2.3）：不再携带 `LazyModel` 载函——模型构造能力属于
/// `DriverRegistry`（`escalate` feature），不变量 I7「Policy 不得持有模型」。
///
/// 派生 `Ord` 的变体序即 capability 序（Needle < Local < Remote），
/// [`EscalationTarget::rank`] 返回 0/1/2；该 rank 只用于 `fallback` 上限
/// 判断，**不用于推导 tier**（tier 是 `tiers` 数组下标，见模块级文档）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EscalationTarget {
    /// 引擎本身（needle2）：能力位序 0。
    Needle,
    /// 本地第二档：能力位序 1（candle / ollama / …，由 DriverRegistry 决定）。
    Local,
    /// 云端档：能力位序 2（自有后端代理/网关，禁止把 API key 编进客户端）。
    Remote,
}

impl EscalationTarget {
    /// 能力位序（0=Needle，1=Local，2=Remote）。
    ///
    /// 仅用于 capability 上限判断（[`EscalationPolicy::allows`]）；**不是 tier**。
    pub const fn rank(self) -> u8 {
        match self {
            EscalationTarget::Needle => 0,
            EscalationTarget::Local => 1,
            EscalationTarget::Remote => 2,
        }
    }
}

/// 升级策略资源。
///
/// 插件以 `init_resource` 注入默认值（`enabled = false`，[`OnlineFallback::Never`]）。
/// 宿主在 add_plugins 之后、发 RunAgent 之前改写字段即可生效——策略是数据，
/// 不是编译期开关。
#[derive(Resource, Clone, Debug)]
pub struct EscalationPolicy {
    /// 是否启用升级路径。`false` 时门控触发的 run 仍会走 Escalating→Escalated
    /// 终结（契约行为可见），只是没有外部升级动作发生。
    pub enabled: bool,
    /// 允许的升级目标上限（按 [`EscalationTarget::rank`] 比较，档位永不回退）。
    pub fallback: OnlineFallback,
    /// 已注册的档位表（按 rank 升序；索引即 `RunStatus::Escalating.tier`，
    /// 下标单调递增、永不回退——I10）。
    pub tiers: Vec<EscalationTarget>,
    /// 策略级置信度门限覆盖（`None` = 用各 agent `NeedleAgentSpec::confidence_threshold`）。
    pub confidence_threshold: Option<f32>,
    /// 升级流程总预算（超时直接终态，不再升档）。
    pub total_budget: Duration,
    /// 单档位预算（超时视为该档失败，进下一档）。
    pub per_tier_timeout: Duration,
}

impl Default for EscalationPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            fallback: OnlineFallback::Never,
            tiers: Vec::new(),
            confidence_threshold: None,
            total_budget: Duration::from_secs(30),
            per_tier_timeout: Duration::from_secs(10),
        }
    }
}

impl EscalationPolicy {
    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn new() -> Self {
        Self::default()
    }

    /// builder：启用升级路径。
    pub fn enabled(mut self) -> Self {
        self.enabled = true;
        self
    }

    /// builder：设置允许的升级目标上限。
    pub fn with_fallback(mut self, fallback: OnlineFallback) -> Self {
        self.fallback = fallback;
        self
    }

    /// builder：注册档位（落库按 rank 升序；同档重复注册去重，档位单调不回退）。
    pub fn with_tier(mut self, tier: EscalationTarget) -> Self {
        if !self.tiers.contains(&tier) {
            self.tiers.push(tier);
        }
        self.tiers.sort();
        self
    }

    /// builder：策略级置信度门限覆盖。
    pub fn with_confidence_threshold(mut self, threshold: f32) -> Self {
        self.confidence_threshold = Some(threshold);
        self
    }

    /// builder：预算。
    pub fn with_budgets(mut self, total: Duration, per_tier: Duration) -> Self {
        self.total_budget = total;
        self.per_tier_timeout = per_tier;
        self
    }

    /// 档位是否在允许范围内（`fallback` 是上限：`Never` 只放行 `Needle`，
    /// `LocalModelOnly` 放行到 `Local`，`Cloud` 全放行）。
    pub fn allows(&self, target: EscalationTarget) -> bool {
        target.rank() <= self.fallback.ceiling_rank()
    }

    /// 全局门限（策略覆盖优先，否则回退 agent 门限）。
    pub fn threshold_or(&self, agent_threshold: f32) -> f32 {
        self.confidence_threshold.unwrap_or(agent_threshold)
    }

    /// **f64/f32 cast 的唯一允许入口**（规格 §10）。
    ///
    /// needle 信封的 `confidence: f64` 与门限 `f32` 的比较只能发生在这里，
    /// 不得散落在各 systems 里。cast 方向是「门限抬升为 f64」，置信度精度无损。
    pub fn below_threshold(&self, confidence: f64, agent_threshold: f32) -> bool {
        let threshold = self.threshold_or(agent_threshold) as f64;
        confidence < threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_is_offline() {
        let policy = EscalationPolicy::default();
        assert!(!policy.enabled);
        assert_eq!(policy.fallback, OnlineFallback::Never);
        assert!(policy.tiers.is_empty());
    }

    #[test]
    fn tiers_are_monotonic_never_regress() {
        let policy = EscalationPolicy::new()
            .with_tier(EscalationTarget::Local)
            .with_tier(EscalationTarget::Needle)
            .with_tier(EscalationTarget::Remote);
        // 注册顺序打乱，落库必须按 rank 升序：Needle < Local < Remote
        assert_eq!(policy.tiers.len(), 3);
        assert_eq!(policy.tiers[0], EscalationTarget::Needle);
        assert_eq!(policy.tiers[0].rank(), 0);
        assert!(policy.tiers[1] < policy.tiers[2]);
    }

    #[test]
    fn duplicate_tier_is_deduplicated() {
        let policy = EscalationPolicy::new()
            .with_tier(EscalationTarget::Local)
            .with_tier(EscalationTarget::Local);
        assert_eq!(policy.tiers.len(), 1);
    }

    #[test]
    fn never_fallback_rejects_non_needle_targets() {
        let policy = EscalationPolicy::new().with_fallback(OnlineFallback::Never);
        assert!(policy.allows(EscalationTarget::Needle));
        assert!(!policy.allows(EscalationTarget::Local));
        assert!(!policy.allows(EscalationTarget::Remote));
    }

    #[test]
    fn local_fallback_allows_local_but_not_remote() {
        let policy = EscalationPolicy::new().with_fallback(OnlineFallback::LocalModelOnly);
        assert!(policy.allows(EscalationTarget::Needle));
        assert!(policy.allows(EscalationTarget::Local));
        assert!(!policy.allows(EscalationTarget::Remote));
        let cloud = EscalationPolicy::new().with_fallback(OnlineFallback::Cloud);
        assert!(cloud.allows(EscalationTarget::Remote));
    }

    #[test]
    fn below_threshold_uses_policy_override_first() {
        let policy = EscalationPolicy::new().with_confidence_threshold(0.5);
        // agent 门限 0.9，但策略覆盖 0.5
        assert!(policy.below_threshold(0.4, 0.9));
        assert!(!policy.below_threshold(0.6, 0.9));
    }

    #[test]
    fn below_threshold_falls_back_to_agent_threshold() {
        let policy = EscalationPolicy::new();
        assert!(policy.below_threshold(0.05, 0.5));
        assert!(!policy.below_threshold(0.9, 0.5));
    }
}
