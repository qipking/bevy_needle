//! `Needle3Model` → `Serve`：Rig 0.44 的官方扩展边界（升级计划 §7.2/§3.1）。
//!
//! 上游 `serve::adapters::ModelAdapter<Completion>::new(label, model)` 把
//! `Model<W, T>` 擦成 `DynModel<Completion>` 并自动实现 `Serve`（v23 §7.2：
//! **禁止**自写 `impl Serve for Needle3Model`——适配是上游 Adapter 的职责）。
//!
//! 本模块提供的是**注册入口**：`register_completion_handler` 把
//! `Needle3Model` 经 `ModelAdapter` 注册进 rig-ecs 的 [`rig_ecs::bus::Handlers`]
//! （POC 闸门 G1 的落点）。rig feature（无 rig-ecs）时只暴露模型构造。

use std::sync::Arc;

use super::model::Needle3Model;
use super::worker::Needle3Worker;

/// 注册标签（诊断面；升级计划 §14：身份由实例字段携带，不是常量）。
pub const NEEDLE_LABEL: &str = "needle3";

/// 从后端构造 Needle3 模型（Rig 0.44 `Model` 形态；§13 一 Agent 一会话）。
///
/// `backend` 应是启动期构造完成的引擎句柄（§13 预热纪律：升级路径上
/// 禁止触发模型/引擎加载）。
pub fn needle3_model(
    backend: Arc<dyn crate::backend::NeedleBackend>,
    label: impl Into<String>,
) -> Needle3Model {
    Needle3Model::new(Arc::new(Needle3Worker::new(backend)), label)
}

#[cfg(feature = "rig-ecs")]
mod ecs {
    use rig_core::driver::DynModel;
    use rig_core::serve::adapters::ModelAdapter;
    use rig_ecs::bus::Handlers;

    use super::Needle3Model;
    use crate::rig::NEEDLE_LABEL;

    /// 把 Needle3 模型注册进 rig-ecs handler registry（升级计划 §17）。
    ///
    /// `key` 是 dispatch 键（如 `"model:needle"`）；模型经上游 `ModelAdapter`
    /// 自动提供 `Serve`（v23 §7.1：不自写 `impl Serve`）。返回 handler 实体
    /// （Agent 的 `UsesModel` 目标）。
    ///
    /// # Errors
    /// [`rig_core::error::ErrorReport`]——键已被绑定到其它 family 等
    /// registry 冲突。
    pub fn register_completion_handler(
        handlers: &mut Handlers<'_, '_>,
        key: impl Into<rig_core::effect::HandlerKey>,
        model: Needle3Model,
    ) -> Result<bevy_ecs::entity::Entity, rig_core::error::ErrorReport> {
        handlers.register(
            key,
            ModelAdapter::new(NEEDLE_LABEL, DynModel::from(model.into_inner())),
        )
    }
}

#[cfg(feature = "rig-ecs")]
pub use ecs::register_completion_handler;
