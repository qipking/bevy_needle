//! rig-ecs host：把 bevy_needle 的 Needle3 / 工具管线接进 rig-ecs（升级计划
//! §17/§18；`rig-ecs` feature，POC 闸门 G2 的落点）。
//!
//! ```text
//! rig_ecs::RigPlugin（上游 bus/agent 运行时）
//!      ↓
//! Handlers::register("model:needle3", ModelAdapter::new(label, Needle3Model))
//!      ↓
//! Handlers::register(tool_key, ToolFn)（每个 bevy_needle ToolSpec 一把）
//!      ↓
//! Agent（Owner/Preamble/UsesModel/Grant…）+ spawn_run
//!      ↓
//! rig-ecs 原生 Run/Turn/Effect/Materialise（升级计划 §19：不再自建 AgentRun）
//! ```
//!
//! 工具执行边界（I3）：rig-ecs 的 `ToolFn` 回调**直接调用 bevy_needle 的
//! `ToolHandlerFn`**（纯函数，无 World 访问），结果进 rig-ecs 的 tool batch
//! 回喂路径。这不违反 I3——bevy_needle 的 I3 是「rig async 回调不得触碰
//! `&mut World`/ECS 实体」，而 `ToolHandlerFn` 是宿主在 `ToolHandlers` 里
//! 注册的纯函数（与 `dispatch_registered_tool_calls` 执行的是同一函数表）。
//!
//! 世界效果（编辑器状态变更）仍由应用侧的 `ToolCallCompleted` 观察者执行
//! （bevy_needle 现有约定），或经 `register_world` 的 `WorldEffect` 接入。

use std::sync::Arc;

use rig_ecs::bus::Handlers;
use rig_core::serve::adapters::{ModelAdapter, ToolFn};

use crate::tool::ToolSpec;

use super::model::Needle3Model;
use super::NEEDLE_LABEL;

/// 注册键：Needle3 完成模型（`model:<label>` 语法）。
pub fn needle_model_key(label: &str) -> String {
    format!("model:{label}")
}

/// 注册键：bevy_needle 工具（`tool:<name>` 语法）。
pub fn tool_key(name: &str) -> String {
    format!("tool:{name}")
}

/// 注册 Needle3 完成模型为 rig-ecs handler。
///
/// 返回 handler 实体（Agent 的 `UsesModel` 目标）。
///
/// # Errors
/// [`rig_core::error::ErrorReport`]——键被其它 family 占用等 registry 冲突。
pub fn register_needle_model(
    handlers: &mut Handlers<'_, '_>,
    label: &str,
    model: Needle3Model,
    gate: Option<super::gate::ConfidenceGate>,
) -> Result<bevy_ecs::entity::Entity, rig_core::error::ErrorReport> {
    let adapter = ModelAdapter::new(NEEDLE_LABEL, rig_core::driver::DynModel::from(model.into_inner()));
    // 置信度门控是 handler 上的 Layer（Rig 0.44 官方 hook；拒绝点在
    // tool materialise 之前——升级计划 §12.3）。
    let handler = match gate {
        Some(gate) => rig_core::serve::ErasedHandler::new(adapter).layered(gate),
        None => rig_core::serve::ErasedHandler::new(adapter),
    };
    handlers.register(needle_model_key(label), handler)
}

/// 把 bevy_needle 工具面注册为 rig-ecs 工具 handler。
///
/// 每个 `ToolSpec` 注册一把 `ToolFn`（name/description/parameters 直接
/// 映射，§5 协议 1:1）；回调执行 `ToolHandlers` 里的纯函数 handler——
/// 与 bevy_needle 内置分发同一函数表，无第二份注册表（§8.1）。
/// 未注册 handler 的工具回调返回「必须由宿主处理」的明确错误。
///
/// 返回 handler 实体（Agent 的 `Grant` 目标，注册顺序即广告顺序）。
///
/// # Errors
/// [`rig_core::error::ErrorReport`]——registry 冲突。

/// bevy_needle 工具 handler 的 rig 侧回调（I3）。
///
/// 闭包捕获 `Arc` 化的函数表（clone 廉价、只读），因此是 `Fn`——
/// `ToolCallback` 的 blanket-for-`Fn` 自动成立（与上游 layer/tests.rs 同形）。
#[derive(Clone)]
struct BevyToolCallback {
    name: String,
    handler: Option<crate::tool::ToolHandlerFn>,
}

/// HRTB 适配：把具体回调提升为 `for<'a> Fn(&'a mut ToolContext, Value)`。
///
/// 泛型参数让每个 `'a` 独立实例化；闭包体只做 clone 与 spawn future。
fn make_tool_callback(
    state: BevyToolCallback,
) -> impl for<'a> Fn(
    &'a mut rig_core::tool::ToolContext,
    serde_json::Value,
) -> rig_core::wasm_compat::WasmBoxedFuture<
    'a,
    Result<rig_core::tool::ToolOutput, rig_core::tool::ToolExecutionError>,
> + WasmCompatSend2 {
    move |_context: &mut rig_core::tool::ToolContext, args: serde_json::Value| {
        let state = state.clone();
        Box::pin(async move { state.run(args).await })
    }
}

impl BevyToolCallback {
    /// 执行一次调用（clone 后进入 async，`Fn` 语义）。
    async fn run(
        self,
        args: serde_json::Value,
    ) -> Result<rig_core::tool::ToolOutput, rig_core::tool::ToolExecutionError> {
        match self.handler {
            Some(function) => {
                let call = crate::tool::ToolCall {
                    run: bevy_ecs::entity::Entity::PLACEHOLDER,
                    tool: bevy_ecs::entity::Entity::PLACEHOLDER,
                    name: self.name.clone(),
                    call_id: String::new(),
                    args,
                };
                match function(&call) {
                    Ok(output) => Ok(rig_core::tool::ToolOutput::json(output.value)),
                    Err(error) => Err(rig_core::tool::ToolExecutionError::other(error.message)),
                }
            }
            None => Err(rig_core::tool::ToolExecutionError::other(format!(
                "tool `{}` has no bevy_needle handler; register one via \
                 bevy_needle::register_tool_handler",
                self.name
            ))),
        }
    }
}

/// `WasmCompatSend` 的短别名（wasm 约束面）。
use rig_core::wasm_compat::WasmCompatSend as WasmCompatSend2;

/// 把 bevy_needle 工具面注册为 rig-ecs 工具 handler（见模块级文档）。
///
/// 每个 `ToolSpec` 注册一把 `ToolFn`（name/description/parameters 直接
/// 映射，升级计划 §5 协议 1:1）；回调执行 `ToolHandlers` 里的纯函数
/// handler——与 bevy_needle 内置分发同一函数表，无第二份注册表（§8.1）。
/// 未注册 handler 的工具回调返回「必须由宿主处理」的明确错误。
///
/// 返回 handler 实体（Agent 的 `Grant` 目标，注册顺序即广告顺序）。
///
/// # Errors
/// [`rig_core::error::ErrorReport`]——registry 冲突。
pub fn register_tool_specs(
    handlers: &mut Handlers<'_, '_>,
    specs: Vec<ToolSpec>,
    tool_handlers: &crate::tool::ToolHandlers,
) -> Result<Vec<bevy_ecs::entity::Entity>, rig_core::error::ErrorReport> {
    let mut entities = Vec::with_capacity(specs.len());
    for spec in specs {
        let name = spec.name.clone();
        let handler = tool_handlers.get(&name).cloned();
        let description = spec.description.clone();
        let callback = make_tool_callback(BevyToolCallback { name, handler });
        let tool_fn = ToolFn::new(spec.name.clone(), description, spec.parameters.clone(), callback);
        entities.push(handlers.register(tool_key(&spec.name), tool_fn)?);
    }
    Ok(entities)
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
pub fn spawn_agent(commands: &mut bevy_ecs::system::Commands, spec: AgentSpec<'_>) -> bevy_ecs::entity::Entity {
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
        commands.spawn((rig_ecs::agent::Grant(tool), bevy_ecs::hierarchy::ChildOf(agent)));
    }
    agent
}

/// 便捷：从 World 一次性装配（注册 + Agent）。
///
/// # Errors
/// [`rig_core::error::ErrorReport`]——registry 冲突。
pub fn assemble_default_agent(
    world: &mut bevy_ecs::world::World,
    model: Needle3Model,
    owner: &str,
) -> Result<bevy_ecs::entity::Entity, rig_core::error::ErrorReport> {
    // 先取快照（工具面在 EngineSync 已重建），再进 Handlers 闭包。
    let mut names: Vec<String> = {
        let mut query = world.query::<&ToolSpec>();
        query
            .iter(world)
            .map(|spec| spec.name.clone())
            .collect::<Vec<_>>()
    };
    names.sort();
    let specs: Vec<ToolSpec> = names
        .into_iter()
        .filter_map(|name| {
            let mut query = world.query::<&ToolSpec>();
            query.iter(world).find(|spec| spec.name == name).cloned()
        })
        .collect();
    let tool_handlers = world.resource::<crate::tool::ToolHandlers>().clone();
    let policy = Arc::new(std::sync::Mutex::new(
        world.resource::<crate::policy::EscalationPolicy>().clone(),
    ));
    let mut agent = None;
    let registration: Result<Result<(), rig_core::error::ErrorReport>, rig_core::error::ErrorReport> =
        Handlers::with(world, |handlers| {
        let model_entity = match register_needle_model(
            handlers,
            NEEDLE_LABEL,
            model,
            Some(super::gate::ConfidenceGate::new(Arc::clone(&policy))),
        ) {
            Ok(entity) => entity,
            Err(report) => return Err(report),
        };
        let tools = match register_tool_specs(handlers, specs, &tool_handlers) {
            Ok(tools) => tools,
            Err(report) => return Err(report),
        };
        agent = Some((model_entity, tools));
        Ok::<(), rig_core::error::ErrorReport>(())
    });
    // Agent 装配在闭包外（闭包期间 world 被 Handlers 独占借用）。
    registration??;
    let (model_entity, tools) = agent.expect("agent parts set by the closure above");
    let mut commands = world.commands();
    let agent = spawn_agent(
        &mut commands,
        AgentSpec {
            owner,
            preamble: None,
            model: model_entity,
            tools,
            max_turns: 8,
        },
    );
    world.flush();
    Ok(agent)
}
