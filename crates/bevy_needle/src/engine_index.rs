//! 每个 agent 的引擎绑定快照（system facts + 工具集 JSON + 签名）。
//!
//! EngineSync 阶段重建：agent 的工具集变更后签名变化，下一次 turn 会
//! 触发引擎的 `needle_init` 重新绑定（与 Python `_bind()` 一致）。

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use serde_json::Value;

use crate::{
    agent::{AgentEngineOptions, AgentToolRefs, NeedleAgentSpec},
    engine::DEFAULT_BUFFER_SIZE,
    schema::{normalize_tool_schema, tools_json},
    tool::{ToolRegistry, ToolSpec},
};

/// 快照默认缓冲（后端特定大小可扩展 AgentEngineOptions 后覆盖）。
const DEFAULT_SNAPSHOT_BUFFER_SIZE: usize = DEFAULT_BUFFER_SIZE;

/// agent 的可执行快照。
#[derive(Clone, Debug)]
pub struct AgentToolSnapshot {
    /// `agent`（语义见类型文档）。
    pub agent: Entity,
    /// `signature`（语义见类型文档）。
    pub signature: u64,
    /// `system`（语义见类型文档）。
    pub system: String,
    /// `tools_json`（语义见类型文档）。
    pub tools_json: String,
    /// `tool_index_path`（语义见类型文档）。
    pub tool_index_path: Option<std::path::PathBuf>,
    /// `weights_path`（语义见类型文档）。
    pub weights_path: Option<std::path::PathBuf>,
    /// `tool_names`（语义见类型文档）。
    pub tool_names: Vec<String>,
    /// `max_new_tokens`（语义见类型文档）。
    pub max_new_tokens: u32,
    /// `max_steps`（语义见类型文档）。
    pub max_steps: u32,
    /// `confidence_threshold`（语义见类型文档）。
    pub confidence_threshold: Option<f32>,
    /// 输出缓冲大小（来自后端/配置）。
    pub buffer_size: usize,
}

#[derive(Resource, Default)]
/// `AgentToolIndex`（见类型级与模块级文档）。
pub struct AgentToolIndex {
    snapshots: HashMap<Entity, AgentToolSnapshot>,
    /// 无法生成快照的 agent 及原因（诊断用）。
    pub errors: HashMap<Entity, String>,
}

impl AgentToolIndex {
    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn get(&self, agent: Entity) -> Option<&AgentToolSnapshot> {
        self.snapshots.get(&agent)
    }

    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn error(&self, agent: Entity) -> Option<&String> {
        self.errors.get(&agent)
    }
}

fn hash_signature(parts: &[&str]) -> u64 {
    // FNV-1a 64bit：签名只需要稳定，不需要密码学强度。
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for byte in part.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= 0x1f as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// EngineSync：为所有 agent 重建快照。
pub fn rebuild_agent_tool_index(world: &mut World) {
    let agents: Vec<(Entity, NeedleAgentSpec, AgentToolRefs, AgentEngineOptions)> = {
        let mut query = world.query::<(
            Entity,
            &NeedleAgentSpec,
            &AgentToolRefs,
            &AgentEngineOptions,
        )>();
        query
            .iter(world)
            .map(|(e, spec, tools, options)| (e, spec.clone(), tools.clone(), options.clone()))
            .collect()
    };

    let tool_specs: Vec<(Entity, String, String, Value)> = {
        let registry = world.resource::<ToolRegistry>();
        registry
            .iter()
            .filter_map(|registered| {
                let entity = world.get_entity(registered.entity).ok()?;
                let spec = entity.get::<ToolSpec>()?;
                Some((
                    registered.entity,
                    spec.name.clone(),
                    spec.description.clone(),
                    spec.parameters.clone(),
                ))
            })
            .collect()
    };

    let mut index = AgentToolIndex::default();

    for (agent, spec, tools, options) in agents {
        let mut names: Vec<String> = Vec::new();
        let mut schemas: Vec<Value> = Vec::new();
        let mut error: Option<String> = None;

        for tool_entity in &tools.0 {
            let Some((_, name, description, parameters)) =
                tool_specs.iter().find(|(e, _, _, _)| e == tool_entity)
            else {
                continue;
            };
            match normalize_tool_schema(parameters) {
                Ok(params_schema) => {
                    names.push(name.clone());
                    schemas.push(serde_json::json!({
                        "name": name,
                        "description": description,
                        "parameters": params_schema,
                    }));
                }
                Err(err) => {
                    error = Some(err.to_string());
                    break;
                }
            }
        }

        let tools_json = if error.is_none() {
            match tools_json(&schemas) {
                Ok(json) => json,
                Err(err) => {
                    error = Some(err.to_string());
                    String::new()
                }
            }
        } else {
            String::new()
        };

        if let Some(message) = error {
            index.errors.insert(agent, message);
            continue;
        }

        let signature = hash_signature(&[
            spec.system_facts.as_deref().unwrap_or(""),
            &tools_json,
            &options
                .tool_index_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            &options
                .weights
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        ]);

        index.snapshots.insert(
            agent,
            AgentToolSnapshot {
                agent,
                signature,
                system: spec.system_facts.clone().unwrap_or_default(),
                tools_json,
                tool_index_path: options.tool_index_path.clone(),
                weights_path: options.weights.clone(),
                tool_names: names,
                max_new_tokens: spec.max_new_tokens,
                max_steps: spec.max_steps,
                confidence_threshold: spec.confidence_threshold,
                buffer_size: DEFAULT_SNAPSHOT_BUFFER_SIZE,
            },
        );
    }

    // 完全重建：不保留过期 agent 的快照
    *world.resource_mut::<AgentToolIndex>() = index;
}
