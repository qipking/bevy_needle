//! Agent 实体：工具集绑定 + 会话归属 + 推理参数（对齐 bevy_rig 的 `agent.rs`）。

use std::path::PathBuf;

use bevy_ecs::prelude::*;

use crate::{
    session::{spawn_session, Session},
    tool::ToolSpec,
};

/// Needle agent 规格。Needle 没有 system prompt 概念，`system_facts`
/// 只承载环境事实（date/device/...），不要放指令。
#[derive(Component, Clone, Debug, Default)]
pub struct NeedleAgentSpec {
    /// `name`（语义见类型文档）。
    pub name: String,
    /// `system_facts`（语义见类型文档）。
    pub system_facts: Option<String>,
    /// `max_new_tokens`（语义见类型文档）。
    pub max_new_tokens: u32,
    /// `max_steps`（语义见类型文档）。
    pub max_steps: u32,
    /// 置信度门限：低于该值的 run 视为需要升级处理（见 `RunEscalation`）。
    pub confidence_threshold: Option<f32>,
}

impl NeedleAgentSpec {
    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            system_facts: None,
            max_new_tokens: 256,
            max_steps: 8,
            confidence_threshold: None,
        }
    }

    /// builder：设置 `system_facts`。
    pub fn with_system_facts(mut self, facts: impl Into<String>) -> Self {
        self.system_facts = Some(facts.into());
        self
    }

    /// builder：设置 `confidence_threshold`。
    pub fn with_confidence_threshold(mut self, threshold: f32) -> Self {
        self.confidence_threshold = Some(threshold);
        self
    }

    /// builder：设置 `max_steps`。
    pub fn with_max_steps(mut self, max_steps: u32) -> Self {
        self.max_steps = max_steps;
        self
    }

    /// builder：设置 `max_new_tokens`。
    pub fn with_max_new_tokens(mut self, max_new_tokens: u32) -> Self {
        self.max_new_tokens = max_new_tokens;
        self
    }
}

/// agent 绑定的工具实体列表（数据即绑定，可随时增删）。
#[derive(Component, Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentToolRefs(pub Vec<Entity>);

/// agent 的主会话实体。
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrimarySession(pub Entity);

/// agent 级引擎选项：调优权重（`.cact`）与工具索引缓存路径。
#[derive(Component, Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentEngineOptions {
    /// `weights`（语义见类型文档）。
    pub weights: Option<PathBuf>,
    /// `tool_index_path`（语义见类型文档）。
    pub tool_index_path: Option<PathBuf>,
}

#[derive(Bundle)]
/// `NeedleAgentBundle`（见类型级与模块级文档）。
pub struct NeedleAgentBundle {
    /// `spec`（语义见类型文档）。
    pub spec: NeedleAgentSpec,
    /// `tools`（语义见类型文档）。
    pub tools: AgentToolRefs,
    /// `session`（语义见类型文档）。
    pub session: PrimarySession,
    /// `options`（语义见类型文档）。
    pub options: AgentEngineOptions,
}

#[derive(Debug, thiserror::Error)]
/// `AgentLinkError`（见类型级与模块级文档）。
pub enum AgentLinkError {
    #[error("agent {0:?} 不存在")]
    /// agent 实体不存在。
    MissingAgent(Entity),
    #[error("工具 {0:?} 缺少 ToolSpec")]
    /// 工具实体缺少 `ToolSpec`。
    MissingToolSpec(Entity),
    #[error("工具 {name:?} 已绑定到该 agent")]
    /// 同名工具已绑定到该 agent。
    ///
    /// 字段：已绑定的工具名。
    AlreadyAttached {
        /// 已绑定的工具名。
        name: String,
    },
}

/// 在 World 上创建 agent + 主会话，返回两者句柄。
pub fn spawn_agent(world: &mut World, spec: NeedleAgentSpec) -> AgentHandles {
    let session = spawn_session(world);
    let agent = world
        .spawn(NeedleAgentBundle {
            spec,
            tools: AgentToolRefs::default(),
            session: PrimarySession(session),
            options: AgentEngineOptions::default(),
        })
        .id();
    AgentHandles { agent, session }
}

#[derive(Clone, Copy, Debug)]
/// `AgentHandles`（见类型级与模块级文档）。
pub struct AgentHandles {
    /// `agent`（语义见类型文档）。
    pub agent: Entity,
    /// `session`（语义见类型文档）。
    pub session: Entity,
}

/// 便捷校验：从 agent 上读取会话实体（没有则返回错误）。
pub fn primary_session(world: &World, agent: Entity) -> Result<Entity, AgentLinkError> {
    world
        .get::<PrimarySession>(agent)
        .map(|s| s.0)
        .ok_or(AgentLinkError::MissingAgent(agent))
}

/// 绑定工具实体到 agent（幂等检查：同名工具重复绑定报错）。
pub fn attach_tool(world: &mut World, agent: Entity, tool: Entity) -> Result<(), AgentLinkError> {
    let name = world
        .get::<ToolSpec>(tool)
        .map(|spec| spec.name.clone())
        .ok_or(AgentLinkError::MissingToolSpec(tool))?;
    let mut tools = world
        .get_mut::<AgentToolRefs>(agent)
        .ok_or(AgentLinkError::MissingAgent(agent))?;
    if tools.0.contains(&tool) {
        return Err(AgentLinkError::AlreadyAttached { name });
    }
    tools.0.push(tool);
    Ok(())
}

/// 解绑工具实体。
pub fn detach_tool(world: &mut World, agent: Entity, tool: Entity) -> Result<(), AgentLinkError> {
    let mut tools = world
        .get_mut::<AgentToolRefs>(agent)
        .ok_or(AgentLinkError::MissingAgent(agent))?;
    tools.0.retain(|bound| *bound != tool);
    Ok(())
}

/// 会话实体存在性校验（供诊断用）。
pub fn session_alive(world: &World, session: Entity) -> bool {
    world.get::<Session>(session).is_some()
}
