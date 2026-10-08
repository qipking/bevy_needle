//! rig-ecs host：把 Needle3 / 工具管线接进 rig-ecs（升级计划 §17/§18/§27.3；
//! `rig-ecs` feature，G2 闸门落点）。
//!
//! ```text
//! rig_ecs::RigPlugin（上游 bus/agent 运行时）
//!      ↓
//! Handlers::register("model:needle3", Layered(ModelAdapter(Needle3Model), ConfidenceGate?))
//!      ↓
//! G2-B 双轨（§27.3）：
//!   B1 纯工具轨   Handlers::register(tool:<name>, ToolFn)     ← register_tool_fn
//!   B2 World 工具轨 Handlers::register_world(tool:<name>, E)  ← register_world_tool
//!      ↓
//! Agent（Owner/Preamble/UsesModel/Grant…）+ spawn_run
//!      ↓
//! rig-ecs 原生 Run/Turn/Effect/Materialise（升级计划 §19：不再自建 AgentRun）
//! ```
//!
//! 工具执行边界（§27.3 硬禁止清单）：
//!
//! - `ToolFn` / async callback **直接拿 `&mut World`** —— 禁止。本模块的
//!   B1 轨不触碰 World（纯函数闭包）；
//! - callback **直接调用旧 `ToolHandlerFn`**（legacy `crate::tool` 函数表）
//!   —— 禁止。前一版 host 曾经伪造 `ToolCall { run: PLACEHOLDER, call_id:
//!   "" }` 去借道 legacy 分发，这正是 §27.3 点名的同类违规，已删除。
//!   B1 轨是宿主直注的 rig 原生闭包；B2 轨走 `Asked<E>` / `Answer<E>`。
//!
//! legacy 关系（⑦ 冻结纪律）：本文件**不依赖** `crate::tool` /
//! `crate::policy`（legacy Agent/Run/Tool runtime 与其策略面）。

use rig_ecs::bus::Handlers;
use rig_core::serve::adapters::{ModelAdapter, ToolFn};

use super::model::Needle3Model;
use super::NEEDLE_LABEL;

/// 注册键：Needle3 完成模型（`model:<label>` 语法）。
pub fn needle_model_key(label: &str) -> String {
    format!("model:{label}")
}

/// 注册键：工具（`tool:<name>` 语法；canonical call key）。
pub fn tool_key(name: &str) -> String {
    format!("tool:{name}")
}

/// 注册 Needle3 完成模型为 rig-ecs handler。
///
/// `gate` 是可选的置信度门控 Layer（`after` 拒绝，拒绝点在 tool
/// materialise 之前——§27.4）。返回 handler 实体（Agent 的 `UsesModel` 目标）。
///
/// # Errors
/// [`rig_core::error::ErrorReport`]——键被其它 family 占用等 registry 冲突。
pub fn register_needle_model(
    handlers: &mut Handlers<'_, '_>,
    label: &str,
    model: Needle3Model,
    gate: Option<super::gate::ConfidenceGate>,
) -> Result<bevy_ecs::entity::Entity, rig_core::error::ErrorReport> {
    let adapter = ModelAdapter::new(
        NEEDLE_LABEL,
        rig_core::driver::DynModel::from(model.into_inner()),
    );
    let handler = match gate {
        Some(gate) => rig_core::serve::ErasedHandler::new(adapter).layered(gate),
        None => rig_core::serve::ErasedHandler::new(adapter),
    };
    handlers.register(needle_model_key(label), handler)
}

/// G2-B1 纯工具轨：宿主直注的 rig 原生工具。
///
/// `callback` 是**纯函数**语义（不得触碰 `&mut World` / ECS 实体，§27.3
/// 硬禁止第一条）；`ToolFn` 是 rig 的官方 runtime-defined tool，
/// name/description/parameters 与模型广告面同源（无第二份 schema 表）。
///
/// HRTB 约束与上游 [`ToolCallback`] 的 blanket-for-`Fn` 一致——普通
/// 闭包（捕获 `Arc`/无捕获）都满足。
///
/// 返回 handler 实体（Agent 的 `Grant` 目标）。
///
/// [`ToolCallback`]: rig_core::serve::adapters::ToolCallback
///
/// # Errors
/// [`rig_core::error::ErrorReport`]——registry 冲突。
pub fn register_tool_fn<F>(
    handlers: &mut Handlers<'_, '_>,
    name: &str,
    description: &str,
    parameters: serde_json::Value,
    callback: F,
) -> Result<bevy_ecs::entity::Entity, rig_core::error::ErrorReport>
where
    F: for<'a> std::ops::Fn(
            &'a mut rig_core::tool::ToolContext,
            serde_json::Value,
        ) -> rig_core::wasm_compat::WasmBoxedFuture<
            'a,
            Result<rig_core::tool::ToolOutput, rig_core::tool::ToolExecutionError>,
        > + rig_core::wasm_compat::WasmCompatSend
        + rig_core::wasm_compat::WasmCompatSync
        + 'static,
{
    let tool_fn = ToolFn::new(name.to_owned(), description.to_owned(), parameters, callback);
    handlers.register(tool_key(name), tool_fn)
}

/// G2-B2 World 工具轨：World 效果必须走 rig-ecs 的 World handler
/// （§27.3 硬第二条——`Asked<E> → Bevy system（可 Query<&mut World>）→
/// `Answer<E>` → Rig`，execution semantics 归 rig-ecs）。
///
/// `E` 是宿主定义的 [`WorldEffect`]（`CustomEffect`：`]world`）——
/// dispatch 落到 effect 实体上成为 `Asked<E>`；宿主系统读它、写 `Answer<E>`。
///
/// 定义样板（应用侧）：
///
/// ```ignore
/// #[derive(serde::Serialize, serde::Deserialize)]
/// struct SelectClip { clip_id: String }
/// impl rig_core::effect::CustomEffect for SelectClip {
///     const KIND: &'static str = "bevy_needle.tool:select_clip";
///     type Answer = serde_json::Value;
/// }
///
///  // 注册（startup）+ 应答（用户系统，可用 World 访问）：
/// bevy_needle::rig::register_world_tool::<SelectClip>(handlers, "select_clip")?;
/// //   effect 实体上出现 Asked<SelectClip> → 系统读它 → insert(Answer(v))
/// ```
///
/// [`WorldEffect`]: rig_ecs::bus::WorldEffect
///
/// # Errors
/// [`rig_core::error::ErrorReport`]——registry 冲突。
pub fn register_world_tool<E>(
    handlers: &mut Handlers<'_, '_>,
    name: &str,
) -> Result<bevy_ecs::entity::Entity, rig_core::error::ErrorReport>
where
    E: rig_ecs::bus::WorldEffect,
{
    // 显式 turbofish：`register_world` 的 E 无法从返回值推导。
    handlers.register_world::<E>(tool_key(name))
}

/// Agent 装配输入（§17/§18：薄插件的数据面）。
pub struct AgentSpec<'a> {
    /// agent 名（`Owner`）。
    pub owner: &'a str,
    /// preamble（rig 侧 system 消息；Needle 的 system facts 不经此传递——
    /// 升级计划 §14：Needle 的 system 是环境事实，行为定义在工具描述里）。
    pub preamble: Option<String>,
    /// handler 实体（`UsesModel`）。
    pub model: bevy_ecs::entity::Entity,
    /// 工具 handler 实体（`Grant` 链接，注册顺序）。
    pub tools: Vec<bevy_ecs::entity::Entity>,
    /// 最大轮数。
    pub max_turns: usize,
}

/// 装配一个 rig-ecs Agent（组件形态，与上游 `examples/support::agent` 一致）。
///
/// 返回 agent 实体；宿主用 `world.spawn_run(agent, &[], prompt, false, None)`
/// 发起运行。
pub fn spawn_agent(
    commands: &mut bevy_ecs::system::Commands,
    spec: AgentSpec<'_>,
) -> bevy_ecs::entity::Entity {
    let agent = commands
        .spawn((
            rig_ecs::agent::Owner(spec.owner.to_owned()),
            rig_ecs::agent::Preamble(spec.preamble.clone()),
            rig_ecs::agent::Temperature(None),
            rig_ecs::agent::MaxTokens(Some(1024)),
            rig_ecs::agent::AdditionalParams(None),
            rig_ecs::agent::ToolChoiceSpec(None),
            rig_ecs::agent::Output::default(),
            rig_ecs::agent::DefaultMaxTurns(Some(spec.max_turns)),
            rig_ecs::agent::MaxTurns(spec.max_turns),
            rig_ecs::agent::InvalidCalls::default(),
            rig_ecs::agent::UsesModel(spec.model),
        ))
        .id();
    // Grant 链接是 ChildOf(agent) 的关系组件（上游 `agent_with_tools` 同形）。
    for tool in spec.tools {
        commands.spawn((
            rig_ecs::agent::Grant(tool),
            bevy_ecs::hierarchy::ChildOf(agent),
        ));
    }
    agent
}
