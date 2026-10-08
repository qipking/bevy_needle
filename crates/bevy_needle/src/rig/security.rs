//! NeedleSecurityPolicy：bevy_needle 保留的**薄安全层**（升级计划 §26.2/§26.6；
//! §29.2 任务 A：fail-closed；`rig-ecs` feature）。
//!
//! 职责只有一件事（§26.2）：回答「这个模型能不能被这个 Run 使用」——
//! `LocalModelOnly` / capability ceiling。**不回答** confidence、重试、
//! 取消、checkpoint（那些归 Rig 执行策略）。
//!
//! 为什么不能下沉（§26.3 实测）：Rig 的 `Capabilities` 四字段
//! （completion/max_documents/ndims/declared）全部与 local/remote、网络、
//! 隐私无关——今天 Rig 协议层没有承载 local-only 的载体，本层必须存在。
//!
//! ## fail-closed 语义（§29.2 任务 A）
//!
//! 分类与注册**绑定为同一事务**（[`crate::rig::host::register_local_model`] /
//! [`crate::rig::host::register_remote_model`]，取代旧
//! 「先 `register()`、以后再 `insert_remote()`」的两阶段语义）。护栏矩阵：
//!
//! ```text
//! LocalModelOnly:
//!     classified Local   → allow
//!     classified Remote  → deny
//!     unclassified       → deny      ← 关键：fail-closed（v28 审计 A 修正）
//!
//! CLOUD（显式允许云端）：一律 allow（fail-closed 是 LocalModelOnly 的红线语义）
//! ```
//!
//! 也就是说：**未分类 handler 在 LocalModelOnly 下默认视为 remote（拒绝）**
//! ——不存在靠调用方另行登记才获得的保护缺口；宿主绕过本 crate 助手直接
//! `Handlers::register` 的模型在 LocalModelOnly 下不可被 Run 选中。
//!
//! 执行机制（rig-ecs 原生，非第二个 runtime）：[`security_guard`] 系统挂在
//! RigSchedule（Select 之后、Assemble 之前），对命中禁止判据的 run 写
//! rig-ecs 标准停止钩子 [`rig_ecs::agent::Cancelled`]——上游 `run_cancelled`
//! observer 落 `Failed(Cancelled)` 并清理在途 effect。
//!
//! 与 legacy 的关系（⑦ 冻结纪律）：legacy 的 `OnlineFallback` /
//! `EscalationPolicy` 不被本模块依赖。

use std::collections::HashMap;

use bevy_ecs::prelude::*;

/// 模型类别（安全护栏语义的二值分类；§26.2）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelClass {
    /// 本地模型（不联网）。
    Local,
    /// 远端模型（需要网络；宿主自担密钥防护）。
    Remote,
}

/// 薄安全策略：`LocalModelOnly` / 允许远端。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NeedleSecurityPolicy {
    /// 是否允许 Remote 类模型被 Run 选中。`false` = LocalModelOnly（隐私红线）。
    pub remote_allowed: bool,
}

impl NeedleSecurityPolicy {
    /// `LocalModelOnly`：远端永不选中；**未分类的 handler 一律视为 remote**。
    pub const LOCAL_ONLY: Self = Self {
        remote_allowed: false,
    };

    /// 允许云端档（宿主自担密钥防护与限流）。
    pub const CLOUD: Self = Self { remote_allowed: true };

    /// 该类别是否被允许。
    pub const fn allows(&self, class: ModelClass) -> bool {
        match class {
            ModelClass::Local => true,
            ModelClass::Remote => self.remote_allowed,
        }
    }
}

impl Default for NeedleSecurityPolicy {
    fn default() -> Self {
        Self::LOCAL_ONLY
    }
}

/// 拒绝原因（显式拒绝时的错误载荷）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("security: model class {class:?} is not allowed (LocalModelOnly)")]
pub struct SecurityRefusal {
    /// 被拒的类别。
    pub class: ModelClass,
}

/// 安全护栏的运行时分类表（Resource；§29.2 任务 A）。
///
/// `classifications` 是**经本 crate 注册助手完成注册+分类同事务**的 handler
/// 实体表；[`security_guard`] 按上文的 fail-closed 矩阵判据拦截。宿主
/// `add_resource` 默认 LocalModelOnly。
#[derive(Resource, Debug, Default)]
pub struct SecurityGuard {
    /// 安全策略（LocalModelOnly 默认 = fail-closed）。
    pub policy: NeedleSecurityPolicy,
    /// 注册+分类同事务产出的 handler 分类表。
    pub classifications: HashMap<bevy_ecs::entity::Entity, ModelClass>,
    /// 遥测：护栏拦下的 run 数（诊断面）。
    pub blocked: u64,
}

impl SecurityGuard {
    /// 注册+分类同事务：把 handler 实体登记为指定类别。
    pub fn classify(
        &mut self,
        handler: bevy_ecs::entity::Entity,
        class: ModelClass,
    ) {
        self.classifications.insert(handler, class);
    }

    /// 该 handler 是否被当前策略禁止（fail-closed 矩阵，§29.2）。
    pub fn forbids(&self, handler: bevy_ecs::entity::Entity) -> bool {
        match self.policy {
            // LocalModelOnly：只有显式分类为 Local 的才放行。
            NeedleSecurityPolicy {
                remote_allowed: false,
            } => self.classifications.get(&handler) != Some(&ModelClass::Local),
            // CLOUD：显式允许云端档，一律放行。
            NeedleSecurityPolicy {
                remote_allowed: true,
            } => false,
        }
    }
}

/// 模型选择护栏系统（RigSchedule；Select 之后、Assemble 之前）。
///
/// 读 run 的 [`rig_ecs::agent::UsesModel`]，命中禁止判据（§29.2 fail-closed
/// 矩阵）即写 rig-ecs 标准停止钩子 [`rig_ecs::agent::Cancelled`]（上游
/// observer 落终态与清理——bevy_needle 不另造 Run 状态机，I13）。
pub fn security_guard(
    mut guard: ResMut<SecurityGuard>,
    selected: Query<(Entity, &rig_ecs::agent::UsesModel)>,
    live: Query<
        (),
        (
            With<rig_ecs::agent::Run>,
            Without<rig_ecs::agent::Failed>,
            Without<rig_ecs::agent::Settled>,
            Without<rig_ecs::agent::Cancelled>,
        ),
    >,
    mut commands: Commands,
) {
    for (run, rig_ecs::agent::UsesModel(target)) in selected.iter() {
        if live.contains(run) && guard.forbids(*target) {
            guard.blocked += 1;
            commands.entity(run).insert(rig_ecs::agent::Cancelled(
                "security: LocalModelOnly forbids this model (unclassified counts as remote)"
                    .to_owned(),
            ));
        }
    }
}

/// 把 [`security_guard`] 挂进 RigSchedule（Select 之后、Assemble 之前）。
///
/// 宿主在 `add_plugins(RigPlugin)` 之后调用一次。
pub fn install_security_guard(app: &mut bevy_app::App) {
    use bevy_ecs::schedule::IntoScheduleConfigs;
    if !app.world().is_resource_added::<SecurityGuard>() {
        app.init_resource::<SecurityGuard>();
    }
    app.add_systems(
        rig_ecs::bus::RigSchedule,
        security_guard
            .after(rig_ecs::systems::RigSet::Select)
            .before(rig_ecs::systems::RigSet::Assemble),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_only_forbids_remote_only() {
        let policy = NeedleSecurityPolicy::LOCAL_ONLY;
        assert!(policy.allows(ModelClass::Local));
        assert!(!policy.allows(ModelClass::Remote));
        let cloud = NeedleSecurityPolicy::CLOUD;
        assert!(cloud.allows(ModelClass::Remote));
    }

    /// §29.2 fail-closed 矩阵的单元层验收（三类全覆盖）。
    #[test]
    fn fail_closed_matrix_local_only() {
        let mut guard = SecurityGuard::default();
        assert_eq!(guard.policy, NeedleSecurityPolicy::LOCAL_ONLY);

        let mut world = bevy_ecs::world::World::new();
        let local = world.spawn_empty().id();
        let remote = world.spawn_empty().id();
        let unclassified = world.spawn_empty().id();

        guard.classify(local, ModelClass::Local);
        guard.classify(remote, ModelClass::Remote);

        // classified Local → allow
        assert!(!guard.forbids(local), "classified Local 必须 allow");
        // classified Remote → deny
        assert!(guard.forbids(remote), "classified Remote 必须 deny");
        // unclassified → deny ← 关键：fail-closed（v28 审计 A 修正）
        assert!(guard.forbids(unclassified), "unclassified 必须 deny");
    }

    /// CLOUD 策略下一律放行（含 unclassified——fail-closed 是
    /// LocalModelOnly 的红线语义，Cloud 是显式允许云端的另一种声明）。
    #[test]
    fn cloud_allows_all_classes() {
        let mut guard = SecurityGuard {
            policy: NeedleSecurityPolicy::CLOUD,
            ..SecurityGuard::default()
        };
        let mut world = bevy_ecs::world::World::new();
        let remote = world.spawn_empty().id();
        let unclassified = world.spawn_empty().id();
        guard.classify(remote, ModelClass::Remote);
        assert!(!guard.forbids(remote), "Cloud 放行 Remote");
        assert!(!guard.forbids(unclassified), "Cloud 放行 unclassified");
    }
}
