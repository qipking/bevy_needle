//! 升级策略：宿主对"置信度门控触发之后怎么办"的数据化决策（PR-A 契约面）。
//!
//! 本模块**无 `#[cfg]`、无 rig 依赖**——"要不要联网升级"这个决定必须在
//! 没有 rig 的时候也能做（见计划 §5.3）。它只描述策略；执行者（当前是
//! `finalize_escalations` 的直接终结，未来是 `escalate` feature 下的 rig
//! driver）读取策略并行动：
//!
//! - [`OnlineFallback::Never`]：永不联网，升级语义保持 crate 内闭环（当前默认）。
//! - [`OnlineFallback::LocalModelOnly`]：允许落到本地第二档（如 candle 模型），
//!   仍不出进程。
//! - [`OnlineFallback::Cloud`]：允许云端档（此时宿主自担 API key 泄漏防护，
//!   见计划 §6.2——Bevy 产物分发到玩家机器，密钥不得编进客户端）。
//!
//! 档位单调递增、永不回退（[`EscalationTarget`] 的 `Ord` 按能力档位次比较，
//! 权威层总是"更强"；同档载函不参与比较），这是计划 §5.5 的硬性要求：
//! 否则 needle 反复低置信度会在两档之间死循环。

use bevy_ecs::prelude::*;
use std::time::Duration;

/// 允许的升级目标上限（能力档）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum OnlineFallback {
    /// 永不升级到引擎之外：升级语义在 crate 内闭环（低置信度调用不执行并终结）。
    Never,
    /// 只允许本地模型档（不联网；如 rig-candle 预置构造函数）。
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

/// 一个升级档位：`Needle → Local（任意 CompletionModel）→ Remote（任意 CompletionModel）`。
///
/// 当前阶段只实现 [`EscalationTarget::Needle`]；`Local`/`Remote` 的构造面随
/// `escalate` feature（rig driver）一起落地，枚举变体先行固化契约。
///
/// 比较语义：只按**能力档位次**（Needle < Local < Remote），载函不参与——
/// 因此 Ord 满足"档位单调递增、永不回退"，且闭包天然无需实现 PartialEq。
#[derive(Clone)]
pub enum EscalationTarget {
    /// 引擎本身（needle2）：最高置信度档，位序 0。
    Needle,
    /// 本地第二档：懒构造的 CompletionModel（需要时才创建，避免启动时失败）。
    ///
    /// 构造函数签名故意保持不透明（`fn() -> Box<dyn Any + Send + Sync>`）
    /// 是 PR-A 的临时形态：escalate feature 落地后收窄为
    /// `Box<dyn Fn() -> Box<dyn CompletionModel> + Send + Sync>`（计划 §3.6）。
    Local(
        /// 懒构造函数（PR-A 占位形态；escalate 落地后替换为 CompletionModel 构造器）。
        LazyModel,
    ),
    /// 云端档：经自有后端代理/网关的懒构造（禁止把 API key 编进客户端产物）。
    Remote(
        /// 懒构造函数（同上，PR-A 占位形态）。
        LazyModel,
    ),
}

impl std::fmt::Debug for EscalationTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EscalationTarget::Needle => f.write_str("Needle"),
            EscalationTarget::Local(_) => f.write_str("Local(<lazy>)"),
            EscalationTarget::Remote(_) => f.write_str("Remote(<lazy>)"),
        }
    }
}

/// 懒构造的模型载函（PR-A 占位形态，见 [`EscalationTarget::Local`]）。
pub type LazyModel = std::sync::Arc<dyn Fn() -> Box<dyn std::any::Any + Send + Sync> + Send + Sync>;

impl EscalationTarget {
    /// 能力档位次（0=Needle，1=Local，2=Remote）。
    pub fn rank(&self) -> u8 {
        match self {
            EscalationTarget::Needle => 0,
            EscalationTarget::Local(_) => 1,
            EscalationTarget::Remote(_) => 2,
        }
    }
}

impl PartialEq for EscalationTarget {
    fn eq(&self, other: &Self) -> bool {
        self.rank() == other.rank()
    }
}

impl Eq for EscalationTarget {}

impl PartialOrd for EscalationTarget {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for EscalationTarget {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.rank().cmp(&other.rank())
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
    /// 已注册的档位表（按 rank 升序；索引即 `RunStatus::Escalating.tier`）。
    pub tiers: Vec<EscalationTarget>,
    /// 置信度门限覆盖（`None` = 用各 agent `NeedleAgentSpec::confidence_threshold`）。
    ///
    /// 当前写入点在 `app.rs` 门控处只读 agent 快照；此字段为策略面预留，
    /// escalate driver 落地时接手为权威覆盖。
    pub confidence_threshold: Option<f32>,
    /// 升级流程总预算（超时按计划 §6.4 限流与收割）。
    pub total_budget: Duration,
    /// 单档位预算。
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

    /// builder：注册档位（落库按 rank 升序；同档重复注册按"更强"单调化，不回退）。
    pub fn with_tier(mut self, tier: EscalationTarget) -> Self {
        if let Some(existing) = self
            .tiers
            .iter_mut()
            .find(|t| t.rank() == tier.rank())
        {
            if *existing < tier {
                *existing = tier;
            }
        } else {
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
    pub fn allows(&self, target: &EscalationTarget) -> bool {
        target.rank() <= self.fallback.ceiling_rank()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lazy() -> LazyModel {
        std::sync::Arc::new(|| Box::new(()))
    }

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
            .with_tier(EscalationTarget::Local(lazy()))
            .with_tier(EscalationTarget::Needle)
            .with_tier(EscalationTarget::Remote(lazy()));
        // 注册顺序打乱，落库必须按 rank 升序：Needle < Local < Remote
        assert_eq!(policy.tiers.len(), 3);
        assert_eq!(policy.tiers[0], EscalationTarget::Needle);
        assert_eq!(policy.tiers[0].rank(), 0);
        assert!(policy.tiers[1] < policy.tiers[2]);
    }

    #[test]
    fn never_fallback_rejects_non_needle_targets() {
        let policy = EscalationPolicy::new().with_fallback(OnlineFallback::Never);
        assert!(policy.allows(&EscalationTarget::Needle));
        assert!(!policy.allows(&EscalationTarget::Local(lazy())));
        assert!(!policy.allows(&EscalationTarget::Remote(lazy())));
    }

    #[test]
    fn local_fallback_allows_local_but_not_remote() {
        let policy = EscalationPolicy::new().with_fallback(OnlineFallback::LocalModelOnly);
        assert!(policy.allows(&EscalationTarget::Needle));
        assert!(policy.allows(&EscalationTarget::Local(lazy())));
        assert!(!policy.allows(&EscalationTarget::Remote(lazy())));
        let cloud = EscalationPolicy::new().with_fallback(OnlineFallback::Cloud);
        assert!(cloud.allows(&EscalationTarget::Remote(lazy())));
    }

    #[test]
    fn same_rank_targets_compare_equal_regardless_of_payload() {
        let a = EscalationTarget::Local(lazy());
        let b = EscalationTarget::Local(std::sync::Arc::new(|| Box::new(42usize)));
        assert_eq!(a, b);
        assert!(!(a < b));
    }
}
