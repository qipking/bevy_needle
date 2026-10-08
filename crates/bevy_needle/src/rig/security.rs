//! NeedleSecurityPolicy：bevy_needle 保留的**薄安全层**（升级计划 §26.2/§26.6；
//! `rig-ecs` feature）。
//!
//! 职责只有一件事（§26.2）：回答「这个模型能不能被这个 Run 使用」——
//! `LocalModelOnly` / capability ceiling。**不回答** confidence、重试、
//! 取消、checkpoint（那些归 Rig 执行策略）。
//!
//! 为什么不能下沉（§26.3 实测）：Rig 的 `Capabilities` 四字段
//! （completion/max_documents/ndims/declared）全部与 local/remote、网络、
//! 隐私无关——今天 Rig 协议层没有承载 local-only 的载体，本层必须存在。
//!
//! 执行机制（rig-ecs 原生，非第二个 runtime）：
//!
//! 1. [`SecurityGuard`] 资源登记「按 Remote 分类登记的 handler 实体」；
//! 2. [`security_guard`] 系统挂在 RigSchedule（Select 之后、Assemble 之前），
//!    对选中了被禁止 handler 的 run 写 rig-ecs 的标准停止钩子
//!    [`rig_ecs::agent::Cancelled`]——上游 `run_cancelled` observer 落
//!    `Failed(Cancelled)` 并清理在途 effect。**Helper 侧有旁路**：宿主
//!    绕过本 crate 的注册助手直接 `Handlers::register` 是宿主自己的选择；
//!    本护栏以「已在本 crate 注册过的分类」为准（E2E 语义见
//!    §26.7 LocalModelOnly 行）。
//!
//! 与 legacy 的关系（⑦ 冻结纪律）：legacy 的 `OnlineFallback` /
//! `EscalationPolicy` 不被本模块依赖。

use std::collections::HashSet;

use bevy_ecs::prelude::*;

/// 允许的模型类别（安全护栏语义的二值分类；§26.2）。
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
    /// `LocalModelOnly`：远端永不选中。
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

/// 拒绝原因（注册边界显式拒绝时的错误载荷）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("security: model class {class:?} is not allowed (LocalModelOnly)")]
pub struct SecurityRefusal {
    /// 被拒的类别。
    pub class: ModelClass,
}

/// 安全护栏的运行时登记表（Resource）。
///
/// `remote_handlers` 是**经本 crate 助手注册且分类为 Remote** 的 handler
/// 实体集；[`security_guard`] 只拦这个集合（上游没有 local/remote 概念，
/// §26.3）。宿主 `add_resource` 默认 LocalModelOnly。
#[derive(Resource, Debug, Default)]
pub struct SecurityGuard {
    /// 安全策略（LocalModelOnly 默认）。
    pub policy: NeedleSecurityPolicy,
    /// 分类为 Remote 的 handler 实体。
    pub remote_handlers: HashSet<bevy_ecs::entity::Entity>,
    /// 遥测：护栏拦下的 run 数（诊断面）。
    pub blocked: u64,
}

impl SecurityGuard {
    /// 登记 Remote handler（宿主注册远端模型后调用；E2E 语义：已注册
    /// remote + LocalModelOnly → remote 永不选中，§26.7）。
    pub fn insert_remote(&mut self, handler: bevy_ecs::entity::Entity) {
        self.remote_handlers.insert(handler);
    }

    /// 该 handler 是否被当前策略禁止。
    pub fn forbids(&self, handler: bevy_ecs::entity::Entity) -> bool {
        self.remote_handlers.contains(&handler) && !self.policy.remote_allowed
    }
}

/// 模型选择护栏系统（RigSchedule；Select 之后、Assemble 之前）。
///
/// 读 run 的 [`rig_ecs::agent::UsesModel`]，命中被禁止的 handler 即写
/// rig-ecs 标准停止钩子 [`rig_ecs::agent::Cancelled`]（上游 observer 负责
/// 落终态与清理——bevy_needle 不另造 Run 状态机，I13）。
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
                "security: LocalModelOnly forbids the selected remote model".to_owned(),
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

    #[test]
    fn guard_forbids_only_registered_remote_handlers() {
        let mut guard = SecurityGuard::default();
        let remote = bevy_ecs::entity::Entity::PLACEHOLDER;
        // 登记的是"集合成员资格"：登记过的实体命中；未登记的不命中。
        let mut world = bevy_ecs::world::World::new();
        let unregistered = world.spawn_empty().id();
        guard.insert_remote(remote);
        assert!(guard.forbids(remote), "LocalModelOnly 下已登记 remote 必须被禁");
        assert!(!guard.forbids(unregistered), "未登记的实体不受影响");
        guard.policy = NeedleSecurityPolicy::CLOUD;
        assert!(!guard.forbids(remote), "Cloud 策略放行 remote");
    }
}
