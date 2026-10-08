//! 工具的 ECS 建模与本地执行（对齐 bevy_rig 的 `tool.rs` 形态）。
//!
//! - 工具是实体（`ToolSpec` 描述 schema），注册进 [`ToolRegistry`]；
//! - 游戏侧通过 [`ToolHandlers`] 注册纯函数 handler，也可以在
//!   [`ToolDispatchSystems`] 集里用自己的系统处理调用；
//! - 每一次被模型发起的调用会生成一个 `ToolInvocation` 实体，
//!   状态机为 Queued → Running → Completed/Failed → Published。

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use bevy_ecs::{message::Messages, prelude::*};
use serde_json::Value;
use thiserror::Error;

use crate::schema::{normalize_tool_schema, ToolSchemaError};

static NEXT_TOOL_CALL_ID: AtomicU64 = AtomicU64::new(1);

/// 工具实体标记。
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tool;

/// 工具声明：名字 + 描述 + JSON Schema parameters。
#[derive(Component, Clone, Debug)]
pub struct ToolSpec {
    /// 工具名（引擎语法约束与调用回喂的键）。

        /// 工具名。

        pub name: String,
    /// 工具描述：模型据此选择调用与填充参数。

        pub description: String,
    /// JSON Schema 的 `parameters`（object）。

        pub parameters: Value,
}

impl ToolSpec {
    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }

    /// 返回规范化后的 schema（引擎语法约束的输入）。
    pub fn normalized_parameters(&self) -> Result<Value, ToolSchemaError> {
        normalize_tool_schema(&self.parameters)
    }
}

#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
/// `ToolKind`（见类型级与模块级文档）。
pub enum ToolKind {
    #[default]
    /// 普通调用型工具（目前唯一种类，为未来流式/资源型预留）。
    Invocation,
}

#[derive(Bundle)]
/// `ToolBundle`（见类型级与模块级文档）。
pub struct ToolBundle {
    /// `tool`（语义见类型文档）。
    pub tool: Tool,
    /// `spec`（语义见类型文档）。
    pub spec: ToolSpec,
    /// `kind`（语义见类型文档）。
    pub kind: ToolKind,
}

impl ToolBundle {
    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn new(spec: ToolSpec) -> Self {
        Self {
            tool: Tool,
            spec,
            kind: ToolKind::Invocation,
        }
    }
}

/// 一次工具调用（由模型产出，指向 run 与工具实体）。
#[derive(Clone, Debug)]
pub struct ToolCall {
    /// 发起该调用的 run 实体。

        pub run: Entity,
    /// 解析到的工具实体（未知工具为占位符）。

        pub tool: Entity,
    /// `name`（语义见类型文档）。
    pub name: String,
    /// 稳定唯一 ID（run/tool/nonce 组合）。

        pub call_id: String,
    /// 模型给出的参数对象。

        pub args: Value,
}

impl ToolCall {
    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn new(run: Entity, tool: Entity, name: impl Into<String>, args: Value) -> Self {
        let nonce = NEXT_TOOL_CALL_ID.fetch_add(1, Ordering::Relaxed);
        Self {
            run,
            tool,
            name: name.into(),
            call_id: format!("run{:x}-tool{:x}-call{nonce}", run.to_bits(), tool.to_bits()),
            args,
        }
    }
}

/// 工具执行产物（会按 Needle 约定回喂给引擎）。
#[derive(Clone, Debug, Default)]
pub struct ToolOutput {
    /// 回喂给引擎的 JSON 值。

        pub value: Value,
}

impl ToolOutput {
    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn json(value: Value) -> Self {
        Self { value }
    }

    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn text(value: impl Into<String>) -> Self {
        Self {
            value: Value::String(value.into()),
        }
    }

    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn ok() -> Self {
        Self {
            value: serde_json::json!({ "ok": true }),
        }
    }

    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn as_text(&self) -> Option<&str> {
        self.value.as_str()
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{message}")]
/// `ToolExecutionError`（见类型级与模块级文档）。
pub struct ToolExecutionError {
    /// 人类可读错误信息。

        pub message: String,
}

impl ToolExecutionError {
    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// 工具执行结果：`ToolOutput` 或 [`ToolExecutionError`]。
/// 工具执行结果：[`ToolOutput`] 或 [`ToolExecutionError`]。
pub type ToolExecutionResult = Result<ToolOutput, ToolExecutionError>;

/// 纯函数 handler：不需要访问 World（引擎执行循环里随时可调用）。
pub type ToolHandlerFn = Arc<dyn Fn(&ToolCall) -> ToolExecutionResult + Send + Sync>;

/// handler 注册表（按工具名）。
#[derive(Resource, Default, Clone)]
pub struct ToolHandlers {
    /// 按名索引的 handler 表。
    handlers: Arc<HashMap<String, ToolHandlerFn>>,
}

impl ToolHandlers {
    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn get(&self, name: &str) -> Option<&ToolHandlerFn> {
        self.handlers.get(name)
    }

    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn register(&mut self, name: impl Into<String>, handler: ToolHandlerFn) {
        let handlers = Arc::make_mut(&mut self.handlers);
        handlers.insert(name.into(), handler);
    }

    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn len(&self) -> usize {
        self.handlers.len()
    }

    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }
}

/// 在 World 上便捷注册 handler。
pub fn register_tool_handler(
    world: &mut World,
    name: impl Into<String>,
    handler: impl Fn(&ToolCall) -> ToolExecutionResult + Send + Sync + 'static,
) {
    world
        .resource_mut::<ToolHandlers>()
        .register(name, Arc::new(handler));
}

/// 调用分发策略：
/// - `RegistryHandler`: 由内置分发系统执行 [`ToolHandlers`] 里的 handler；
/// - `External`: 留给游戏自己的系统（在 [`ToolDispatchSystems`] 集内处理）。
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
    /// 由插件内置分发系统执行 `ToolHandlers` 中注册的纯函数。
pub enum ToolDispatchPolicy {
    /// 由插件内置分发系统执行 `ToolHandlers` 中注册的纯函数。
    #[default]
    RegistryHandler,
    /// 留给游戏自己的系统（在 `ToolDispatchSystems` 集内处理）。
    External,
}

#[derive(Component, Clone, Debug, Default, PartialEq, Eq)]
/// `ToolInvocation`（见类型级与模块级文档）。
pub struct ToolInvocation;

#[derive(Component, Clone, Debug)]
/// 调用载荷。

    /// 已入队等待分发。
pub struct ToolInvocationCall(pub ToolCall);
    /// 执行成功。

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
/// `ToolInvocationStatus`（见类型级与模块级文档）。
pub enum ToolInvocationStatus {
    /// 已入队等待分发。
    Queued,
    /// handler 执行中。
    Running,
    /// 执行成功。
    Completed,
    /// 执行失败。
    Failed,
}

#[derive(Component, Clone, Debug)]
/// 成功产物。

pub struct ToolInvocationOutput(pub ToolOutput);

#[derive(Component, Clone, Debug, PartialEq, Eq)]
/// `ToolInvocationError`（见类型级与模块级文档）。
pub struct ToolInvocationError(pub String);

#[derive(Component, Clone, Debug, Default, PartialEq, Eq)]
/// `ToolInvocationPublished`（见类型级与模块级文档）。
pub struct ToolInvocationPublished;

/// 该调用所属的 run 轮次（用于按轮收集结果回喂引擎）。
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ToolInvocationTurn(pub u32);

#[derive(Bundle)]
/// `ToolInvocationBundle`（见类型级与模块级文档）。
pub struct ToolInvocationBundle {
    /// `invocation`（语义见类型文档）。
    pub invocation: ToolInvocation,
    /// `call`（语义见类型文档）。
    pub call: ToolInvocationCall,
    /// `status`（语义见类型文档）。
    pub status: ToolInvocationStatus,
    /// `policy`（语义见类型文档）。
    pub policy: ToolDispatchPolicy,
}

impl ToolInvocationBundle {
    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn new(call: ToolCall) -> Self {
        Self {
            invocation: ToolInvocation,
            call: ToolInvocationCall(call),
            status: ToolInvocationStatus::Queued,
            policy: ToolDispatchPolicy::default(),
        }
    }
}

#[derive(Clone, Debug)]
/// `RegisteredTool`（见类型级与模块级文档）。
pub struct RegisteredTool {
    /// 工具实体。
    pub entity: Entity,
    /// 工具名。
    pub name: String,
    /// 工具种类。
    pub kind: ToolKind,
}

#[derive(Resource, Default)]
/// `ToolRegistry`（见类型级与模块级文档）。
pub struct ToolRegistry {
    by_entity: HashMap<Entity, RegisteredTool>,
    by_name: HashMap<String, Entity>,
}

impl ToolRegistry {
    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn get(&self, entity: Entity) -> Option<&RegisteredTool> {
        self.by_entity.get(&entity)
    }

    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn get_by_name(&self, name: &str) -> Option<Entity> {
        self.by_name.get(name).copied()
    }

    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn iter(&self) -> impl Iterator<Item = &RegisteredTool> {
        self.by_entity.values()
    }
}

#[derive(Message, Clone, Debug)]
/// `ToolCallRequested`（见类型级与模块级文档）。
pub struct ToolCallRequested {
    /// 被请求的调用。
    pub call: ToolCall,
}

#[derive(Message, Clone, Debug)]
/// `ToolCallCompleted`（见类型级与模块级文档）。
pub struct ToolCallCompleted {
    /// 已完成的调用。
    pub call: ToolCall,
    /// handler 产物（回喂引擎）。
    pub output: ToolOutput,
}

#[derive(Message, Clone, Debug)]
/// `ToolCallFailed`（见类型级与模块级文档）。
pub struct ToolCallFailed {
    /// 失败的调用。
    pub call: ToolCall,
    /// 失败原因。
    pub error: String,
}

/// EngineSync 阶段：重建按名索引的工具注册表。
pub fn rebuild_tool_registry(world: &mut World) {
    let mut tools = {
        let mut query = world.query::<(Entity, &ToolSpec)>();
        query
            .iter(world)
            .map(|(entity, spec)| (entity, spec.name.clone()))
            .collect::<Vec<_>>()
    };
    tools.sort_by_key(|(entity, name)| (name.clone(), entity.index()));

    let mut by_entity = HashMap::new();
    let mut by_name = HashMap::new();
    for (entity, name) in tools {
        if by_name.contains_key(&name) {
            continue; // 重名时先到先得，保持确定性
        }
        by_name.insert(name.clone(), entity);
        by_entity.insert(
            entity,
            RegisteredTool {
                entity,
                name,
                kind: ToolKind::Invocation,
            },
        );
    }
    *world.resource_mut::<ToolRegistry>() = ToolRegistry { by_entity, by_name };
}

/// 把 `ToolCallRequested` 消息物化为 Queued 的调用实体。
///
/// 默认管线中模型调用由引擎直接物化（`ToolCallRequested` 仅作通知），
/// 不会经过本系统；游戏若想以消息驱动的方式手动发起调用，可自行把本
/// 系统加进 `ToolDispatchSystems` 之前。
pub fn queue_requested_tool_calls(world: &mut World) {
    let calls: Vec<ToolCall> = {
        let mut messages = world.resource_mut::<Messages<ToolCallRequested>>();
        messages.drain().map(|message| message.call).collect()
    };
    for call in calls {
        world.spawn(ToolInvocationBundle::new(call));
    }
}

/// 内置分发：执行注册了 handler 且策略为 RegistryHandler 的 Queued 调用。
pub fn dispatch_registered_tool_calls(world: &mut World) {
    let mut pending: Vec<(Entity, ToolCall, ToolHandlerFn)> = {
        let handlers = world.resource::<ToolHandlers>().clone();
        let mut query = world.query::<(
            Entity,
            &ToolInvocationCall,
            &ToolInvocationStatus,
            &ToolDispatchPolicy,
        )>();
        query
            .iter(world)
            .filter(|(_, _, status, policy)| {
                **status == ToolInvocationStatus::Queued && **policy == ToolDispatchPolicy::RegistryHandler
            })
            .filter_map(|(entity, call, _, _)| {
                handlers
                    .get(call.0.name.as_str())
                    .cloned()
                    .map(|handler| (entity, call.0.clone(), handler))
            })
            .collect()
    };
    pending.sort_by_key(|(entity, _, _)| entity.index());

    for (invocation, call, handler) in pending {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(&call)));
        match result {
            Ok(Ok(output)) => complete_tool_invocation(
                world,
                invocation,
                output,
            ),
            Ok(Err(error)) => fail_tool_invocation(world, invocation, error.message),
            Err(panic) => {
                let message = panic_message(panic);
                fail_tool_invocation(world, invocation, format!("handler panicked: {message}"));
            }
        }
    }
}

/// 把终态调用发布为消息（游戏系统读取这些消息来施加效果）。
pub fn publish_tool_invocation_results(world: &mut World) {
    let ready = {
        let mut query = world.query::<(
            Entity,
            &ToolInvocationCall,
            &ToolInvocationStatus,
            Option<&ToolInvocationOutput>,
            Option<&ToolInvocationError>,
            Option<&ToolInvocationPublished>,
        )>();
        query
            .iter(world)
            .filter_map(|(entity, call, status, output, error, published)| {
                if published.is_some() {
                    return None;
                }
                match status {
                    ToolInvocationStatus::Completed => {
                        output.map(|o| (entity, call.0.clone(), Some(o.0.clone()), None))
                    }
                    ToolInvocationStatus::Failed => {
                        error.map(|e| (entity, call.0.clone(), None, Some(e.0.clone())))
                    }
                    ToolInvocationStatus::Queued | ToolInvocationStatus::Running => None,
                }
            })
            .collect::<Vec<_>>()
    };

    for (invocation, call, output, error) in ready {
        if let Some(output) = output {
            world.write_message(ToolCallCompleted { call, output });
        } else if let Some(error) = error {
            world.write_message(ToolCallFailed { call, error });
        }
        world.entity_mut(invocation).insert(ToolInvocationPublished);
    }
}

/// 构造/执行入口（错误经 `Result` 返回，不 panic）。
pub fn mark_tool_invocation_running(commands: &mut Commands, invocation: Entity) {
    commands
        .entity(invocation)
        .insert(ToolInvocationStatus::Running)
        .remove::<ToolInvocationOutput>()
        .remove::<ToolInvocationError>()
        .remove::<ToolInvocationPublished>();
}

/// 构造/执行入口（错误经 `Result` 返回，不 panic）。
pub fn complete_tool_invocation(world: &mut World, invocation: Entity, output: ToolOutput) {
    if let Ok(mut entity) = world.get_entity_mut(invocation) {
        entity
            .insert((
                ToolInvocationStatus::Completed,
                ToolInvocationOutput(output),
            ))
            .remove::<ToolInvocationError>()
            .remove::<ToolInvocationPublished>();
    }
}

/// 构造/执行入口（错误经 `Result` 返回，不 panic）。
pub fn fail_tool_invocation(world: &mut World, invocation: Entity, error: impl Into<String>) {
    if let Ok(mut entity) = world.get_entity_mut(invocation) {
        entity
            .insert((
                ToolInvocationStatus::Failed,
                ToolInvocationError(error.into()),
            ))
            .remove::<ToolInvocationOutput>()
            .remove::<ToolInvocationPublished>();
    }
}

pub(crate) fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = panic.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = panic.downcast_ref::<String>() {
        text.clone()
    } else {
        "unknown panic".to_string()
    }
}
