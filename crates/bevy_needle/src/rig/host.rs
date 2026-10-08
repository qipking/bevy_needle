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

/// 注册 Needle3 完成模型：**注册 + Local 分类同一事务**（§29.2 任务 A）。
///
/// Needle 是本地引擎（模型类别 [`ModelClass::Local`] 天然成立）；本助手在
/// 注册成功后立即把 handler 实体写入 [`SecurityGuard`] 的分类表。剩余路径
/// 上「未分类 → deny」（fail-closed）对宿主用原始 `Handlers::register`
/// 绕过本助手的模型自动成立。
///
/// `gate` 是可选的置信度门控 Layer（`after` 拒绝，拒绝点在 tool
/// materialise 之前——§27.4）。返回 handler 实体（Agent 的 `UsesModel` 目标）。
///
/// 需要先 `install_security_guard`（或等价地 `init_resource::<SecurityGuard>`）。
///
/// # Errors
/// [`rig_core::error::ErrorReport`]——键被其它 family 占用等 registry 冲突。
pub fn register_local_model(
    world: &mut bevy_ecs::world::World,
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
    let entity = Handlers::with(world, |handlers| {
        handlers.register(needle_model_key(label), handler)
    })??;
    // 注册+分类同事务（§29.2）：分类表写入紧随注册，同一助手调用内完成
    //（护栏资源缺失时就地初始化——needle 属 Local 是类型级事实）。
    world.init_resource::<super::SecurityGuard>();
    world
        .resource_mut::<super::SecurityGuard>()
        .classify(entity, super::ModelClass::Local);
    Ok(entity)
}

/// 注册一个**远端**模型 handler：**注册 + Remote 分类同一事务**
/// （§29.2 任务 A；§26.7 语义——remote 可注册，LocalModelOnly 下永不选中）。
///
/// `handler` 是宿主提供的任意 rig `Serve` 实现（如 OpenAI 等 cloud 模型，
/// 或测试哨兵）。LocalModelOnly 下被选中会被护栏拒绝。
///
/// # Errors
/// [`rig_core::error::ErrorReport`]——键被其它 family 占用等 registry 冲突。
pub fn register_remote_model(
    world: &mut bevy_ecs::world::World,
    key: impl Into<rig_core::effect::HandlerKey>,
    handler: impl rig_core::serve::Serve + 'static,
) -> Result<bevy_ecs::entity::Entity, rig_core::error::ErrorReport> {
    let entity = Handlers::with(world, |handlers| handlers.register(key, handler))??;
    world.init_resource::<super::SecurityGuard>();
    world
        .resource_mut::<super::SecurityGuard>()
        .classify(entity, super::ModelClass::Remote);
    Ok(entity)
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

/// G2-B2 World 工具轨（§27.3 硬第二条）：ToolCall 的 World 效果走 **rig-ecs
/// 的正典 world-served handler**——`Handlers::register_open(key, Tool family)`
/// + 宿主系统提交 `WorldOutcome`（上游 CONTRACT §8.3；`Asked<E>`/`Answer<E>`
/// 是 Custom effect 的通道——Tool family 的广告由 family descriptor 承担）。
///
/// 广告面：Tool family descriptor（name/description/parameters）与 B1 同源；
/// **执行面**：key 只绑到一个 world 系统——dispatch 的 effect 实体本身是
/// "问题"，宿主系统读它的 `PendingEffect`（`EffectKind::ToolCall`），可以用
/// **任意 World 访问**（含 `Query<&mut Selection>`），然后把
/// [`rig_ecs::bus::WorldOutcome`] 插到 effect 实体上回答：
///
/// ```ignore
/// fn answer_select_clip(
///     effects: Query<(Entity, &rig_ecs::bus::PendingEffect), Added<rig_ecs::bus::InFlight>>,
///     selection: Query<&auk::Selection>,
///     mut commands: Commands,
/// ) {
///     for (entity, effect) in &effects {
///         let rig_core::effect::EffectKind::ToolCall { name, args } = &effect.kind else { continue };
///         if name != "select_clip" { continue; }
///         // …用 selection 做 Bevy 世界操作…
///         commands.entity(entity).insert(rig_ecs::bus::WorldOutcome::new(
///             Ok(rig_core::effect::Outcome::ToolResult {
///                 result: rig_core::tool::ToolResult::success(
///                     rig_core::tool::ToolOutput::json(json!({ "clipped": clip })),
///                 ),
///             }),
///         ));
///     }
/// }
/// ```
///
/// # Errors
/// [`rig_core::error::ErrorReport`]——registry 冲突。
pub fn register_world_tool(
    handlers: &mut Handlers<'_, '_>,
    name: &str,
    description: &str,
    parameters: serde_json::Value,
) -> Result<bevy_ecs::entity::Entity, rig_core::error::ErrorReport> {
    handlers.register_open(
        tool_key(name),
        rig_core::effect::FamilyDescriptor::Tool {
            name: name.to_owned(),
            description: description.to_owned(),
            parameters,
            embedding: None,
        },
    )
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
