//! RigDriverState：`AgentRun` 作为一次 attempt 的内部状态（规格 §5.1，I4）。
//!
//! **I4（Bevy Run ⊃ AgentRun）**：`AgentRun` 只是**一次 attempt 的内部状态**
//! 组件，绝不是 Run 本身。一次 Bevy Run 可能经历 Needle attempt → Rig
//! attempt #1 → Rig attempt #2；每次 rig attempt 重建一个 `AgentRun`
//! （规格 P0-5 裁决），history 从会话转录快照读取。
//!
//! **I3（工具执行在 ECS）**：`CallTools` 在本系统里翻译成 `ToolInvocation`
//! 实体（复用现有 ECS pipeline），**绝不**在 rig async 回调里执行。
//!
//! 模型调用边界（PR-B 范围）：`CallModel` 的真实 provider 执行留待 transport
//! 层；本层通过 [`RigModelInbox`] 回灌 [`rig_run::ModelTurn`]（测试直接注入）。

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::Mutex;

use bevy_ecs::prelude::*;
use rig_core::completion::ToolDefinition;
use rig_core::completion::message::{Message, UserContent};
use rig_run::{AgentRun, AgentRunStep, ModelTurn, PendingToolCall};
use serde_json::json;

use crate::agent::NeedleAgentSpec;
use crate::escalate::{
    DriverError, DriverEvent, DriverEventBus, DriverOutcome, DriverProtocolError, EscalationState,
};
use crate::run::{RunOwner, RunSession, RunStatus};
use crate::session::collect_transcript;
use crate::tool::{
    ToolInvocationBundle, ToolInvocationCall, ToolInvocationError, ToolInvocationOutput,
    ToolInvocationStatus, ToolInvocationTurn, ToolRegistry, ToolSpec,
};

use super::driver::RIG_DRIVER_ID;
use super::tool_bridge::{definitions, pending_to_tool_call, tool_result, tripwires};
use super::transcript::transcript_to_history;

/// 每帧步进的最大轮数（防一帧内无限循环；`AgentRun` 自身有 `max_turns` 预算）。
const MAX_STEPS_PER_FRAME: usize = 8;

/// rig attempt 的内部状态组件（I4：`AgentRun` 只是一次 attempt 的状态）。
#[derive(Component)]
pub struct RigDriverState {
    /// rig 协议状态机（sans-I/O，无 runtime）。
    pub run: AgentRun,
    /// 当前等待什么。
    pub awaiting: RigAwait,
    /// ECS 工具批次的轮次标记（对应 `ToolInvocationTurn`）。
    pub tool_turn: u32,
    /// 每轮模型调用前 `advertise_tools` 上报的工具定义（本 attempt 固定）。
    pub tool_defs: Vec<ToolDefinition>,
}

impl std::fmt::Debug for RigDriverState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RigDriverState")
            .field("awaiting", &self.awaiting)
            .field("tool_turn", &self.tool_turn)
            .finish_non_exhaustive()
    }
}

/// rig 步进的等待状态。
#[derive(Debug)]
pub enum RigAwait {
    /// 可继续 `next_step`。
    Ready,
    /// 等待模型回合（经 [`RigModelInbox`] 回灌）。
    Model,
    /// 等待 ECS 工具批次终态；`calls` 保持 **CallTools 原始顺序**（I20）。
    Tools {
        /// 原始调用顺序（含 preresolved，回喂前按此序重排）。
        calls: Vec<PendingToolCall>,
        /// 该批次打上的 `ToolInvocationTurn`。
        tool_turn: u32,
    },
}

/// 模型回合回灌消息（I17：携带 `(run, epoch)`）。
#[derive(Clone, Debug)]
pub struct RigModelTurn {
    /// 目标 run。
    pub run: Entity,
    /// attempt 的 epoch。
    pub epoch: u64,
    /// 模型产物。
    pub turn: ModelTurn,
}

/// 从 provider 响应构造 [`ModelTurn`]（driver/worker 的规范构造入口）。
///
/// `executable_tool_names` / `allowed_tool_names` **必须**来自本 attempt
/// advertise 的工具集——留空会把每个 tool call 判为非法
/// （`ModelTurnOutcome::NeedsResolution`），这是上游协议的校验面。
pub fn model_turn_with_tools(
    message_id: Option<String>,
    choice: Vec<rig_core::completion::message::AssistantContent>,
    usage: rig_core::completion::Usage,
    advertised: &[ToolDefinition],
) -> ModelTurn {
    let names: std::collections::BTreeSet<String> =
        advertised.iter().map(|d| d.name.clone()).collect();
    ModelTurn::new(message_id, choice, usage, names.clone(), names)
}

/// 模型回合回灌通道（Resource；生产 worker 与测试都向此发 [`RigModelTurn`]）。
#[derive(Resource)]
pub struct RigModelInbox {
    tx: Sender<RigModelTurn>,
    rx: Mutex<Receiver<RigModelTurn>>,
}

impl RigModelInbox {
    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self {
            tx,
            rx: Mutex::new(rx),
        }
    }

    /// 发送端（测试/生产 worker 注入模型回合）。
    pub fn sender(&self) -> Sender<RigModelTurn> {
        self.tx.clone()
    }

    /// 便捷注入（测试用）。
    pub fn inject(&self, msg: RigModelTurn) {
        let _ = self.tx.send(msg);
    }

    fn drain(&self) -> Vec<RigModelTurn> {
        let mut out = Vec::new();
        let Ok(rx) = self.rx.lock() else {
            return out;
        };
        loop {
            match rx.try_recv() {
                Ok(msg) => out.push(msg),
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        out
    }
}

impl Default for RigModelInbox {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for RigModelInbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RigModelInbox")
    }
}

/// rig 步进系统（注册在 `RunResolutionSystems`，**排在 coordinator 之前**）。
///
/// 每帧三件事：① 回灌模型回合；② 为 rig 接管的升级创建 `RigDriverState`；
/// ③ 步进所有 rig run（工具批次终态 → `tool_results` 回喂 → 继续 `next_step`）。
pub fn rig_step_system(world: &mut World) {
    // ① 回灌模型回合（stale epoch 丢弃，I17）
    let turns = world.resource::<RigModelInbox>().drain();
    for msg in turns {
        apply_model_turn(world, msg);
    }

    // ② 为 rig 接管的升级创建状态（AgentRun 重建，P0-5）
    ensure_states(world);

    // ③ 步进
    step_all(world);
}

/// ① 把模型回合喂回 `AgentRun`（epoch 不匹配直接丢弃，不改 Run，I17）。
fn apply_model_turn(world: &mut World, msg: RigModelTurn) {
    let Some(esc) = world.get::<EscalationState>(msg.run).cloned() else {
        return;
    };
    let Some(status) = world.get::<RunStatus>(msg.run).copied() else {
        return;
    };
    if !matches!(status, RunStatus::Escalating { .. }) || esc.epoch != msg.epoch {
        return; // stale event：正常并发控制（规格 §7）
    }

    let result = {
        let Some(mut state) = world.get_mut::<RigDriverState>(msg.run) else {
            return;
        };
        if !matches!(state.awaiting, RigAwait::Model) {
            return; // 不该有模型回合
        }
        match state.run.model_response(msg.turn) {
            Ok(rig_run::ModelTurnOutcome::Continue { .. }) => {
                state.awaiting = RigAwait::Ready;
                None
            }
            // 回滚重试：history 已追加纠正反馈，等下一轮模型产物（CallModel）
            Ok(rig_run::ModelTurnOutcome::TurnRetried) => {
                state.awaiting = RigAwait::Model;
                None
            }
            // 模型给出了不在 allowed 集合里的 tool call：恢复策略（规格 §7
            // 「错误回灌列出合法选项」）——大小写修复优先，否则 Retry 回灌；
            // retry 预算耗尽由 AgentRun 报错 → Failed。
            Ok(rig_run::ModelTurnOutcome::NeedsResolution(context)) => {
                let repaired = state
                    .tool_defs
                    .iter()
                    .map(|d| d.name.as_str())
                    .find(|n| n.eq_ignore_ascii_case(&context.tool_name))
                    .map(|n| n.to_string());
                let action = match repaired {
                    Some(tool_name) => rig_run::InvalidToolCallAction::Repair { tool_name },
                    None => rig_run::InvalidToolCallAction::Retry {
                        feedback: format!(
                            "unknown tool '{}'; available tools: {}",
                            context.tool_name,
                            context.available_tools.join(", ")
                        ),
                    },
                };
                Some(state.run.resolve_invalid_tool_call(action))
            }
            Err(err) => Some(Err(err)),
        }
    };

    match result {
        // Retry：awaiting 已是 Model，等下一轮模型产物
        Some(Ok(rig_run::ModelTurnOutcome::TurnRetried)) => {}
        // Resolve 过程中又出现下一个非法调用（多个 tool call 混合）：
        // v1 简化处理——记为该档失败，进下一档或终态
        Some(Ok(rig_run::ModelTurnOutcome::NeedsResolution(_))) => {
            emit_failed(
                world,
                msg.run,
                DriverError::Model("multiple invalid tool calls in one turn".into()),
            );
        }
        // Repair/Skip：调用已合法化，继续步进
        Some(Ok(_)) => {
            if let Some(mut state) = world.get_mut::<RigDriverState>(msg.run) {
                state.awaiting = RigAwait::Ready;
            }
        }
        Some(Err(err)) => emit_failed(
            world,
            msg.run,
            DriverError::Protocol(DriverProtocolError::InvalidState(err.to_string())),
        ),
        None => {}
    }
}

/// ② 为 rig 接管的升级创建 `RigDriverState`（P0-5：每次 attempt 重建 AgentRun）。
fn ensure_states(world: &mut World) {
    let candidates: Vec<(Entity, EscalationState)> = {
        let mut query = world.query::<(
            Entity,
            &RunStatus,
            &EscalationState,
            Option<&RigDriverState>,
        )>();
        query
            .iter(world)
            .filter_map(|(run, status, esc, state)| match status {
                RunStatus::Escalating { .. }
                    if esc.driver_id == RIG_DRIVER_ID && state.is_none() =>
                {
                    Some((run, esc.clone()))
                }
                _ => None,
            })
            .collect()
    };

    for (run, _esc) in candidates {
        match build_agent_run(world, run) {
            Ok((agent_run, tool_defs)) => {
                let mut state = RigDriverState {
                    run: agent_run,
                    awaiting: RigAwait::Ready,
                    tool_turn: 0,
                    tool_defs,
                };
                // 首次 advertise（turn=1 的模型调用）在 step 中按 CallModel.turn 重录
                let _ = &mut state;
                if let Ok(mut entity) = world.get_entity_mut(run) {
                    entity.insert(state);
                }
            }
            Err(err) => {
                emit_failed(world, run, err);
            }
        }
    }
}

/// 从 World 重建一次 attempt 的 `AgentRun`（P0-5：history 从转录快照读，I14）。
fn build_agent_run(world: &mut World, run: Entity) -> Result<(AgentRun, Vec<ToolDefinition>), DriverError> {
    let session = world.get::<RunSession>(run).map(|s| s.0);
    let agent = world.get::<RunOwner>(run).map(|o| o.0);

    let transcript = session
        .map(|s| collect_transcript(world, s))
        .unwrap_or_default();

    let system_facts = agent
        .and_then(|a| world.get::<NeedleAgentSpec>(a))
        .and_then(|spec| spec.system_facts.as_deref());

    let mut history = transcript_to_history(system_facts, &transcript);
    // 最后一条是本次 prompt；之前的是 history
    let prompt = history.pop().unwrap_or_else(|| Message::User {
        content: vec![UserContent::text("")],
    });
    // invalid tool-call 的恢复预算（规格 §7：错误回灌自恢复，与 needle 一致）；
    // max_turns 是**模型调用总预算**（含工具回喂后的续轮）：默认 1 会在首轮
    // 工具执行后立刻越限，对齐 needle 的 max_steps 默认值 8。
    let agent_run = AgentRun::new(prompt)
        .with_history(history)
        .max_turns(8)
        .max_invalid_tool_call_retries(2);

    let tools = agent
        .map(|a| collect_tools(world, a))
        .unwrap_or_default();
    let tool_defs = definitions(&tripwires(&tools));

    Ok((agent_run, tool_defs))
}

/// 该 agent 的工具集快照（定义侧；执行永远在 ECS，I3）。
fn collect_tools(world: &mut World, agent: Entity) -> Vec<ToolSpec> {
    let Some(refs) = world.get::<crate::agent::AgentToolRefs>(agent) else {
        return Vec::new();
    };
    refs.0
        .iter()
        .filter_map(|entity| world.get::<ToolSpec>(*entity).cloned())
        .collect()
}

/// ③ 步进所有 rig run（有界）。
fn step_all(world: &mut World) {
    let runs: Vec<Entity> = {
        let mut query = world.query::<(Entity, &RunStatus, &RigDriverState)>();
        query
            .iter(world)
            .filter_map(|(run, status, _)| match status {
                RunStatus::Escalating { .. } => Some(run),
                _ => None,
            })
            .collect()
    };

    for run in runs {
        for _ in 0..MAX_STEPS_PER_FRAME {
            if !step_once(world, run) {
                break;
            }
        }
    }
}

/// 步进单个 run 一步；返回是否可继续（可继续 = 工具批次已完成，应再 step）。
fn step_once(world: &mut World, run: Entity) -> bool {
    enum Next {
        Stop,
        ResolveTools,
        Step,
    }

    let next = {
        let Some(state) = world.get::<RigDriverState>(run) else {
            return false;
        };
        match &state.awaiting {
            RigAwait::Model => Next::Stop,
            RigAwait::Tools { .. } => Next::ResolveTools,
            RigAwait::Ready => Next::Step,
        }
    };

    match next {
        Next::Stop => false,
        Next::ResolveTools => match resolve_tools_if_ready(world, run) {
            Ok(true) => true, // 工具批次完成 → 循环继续 step
            Ok(false) => false,
            Err(err) => {
                emit_failed(world, run, err);
                false
            }
        },
        Next::Step => {
            let step = {
                let mut state = world.get_mut::<RigDriverState>(run).expect("state exists");
                state.run.next_step()
            };

            match step {
                Ok(AgentRunStep::CallModel { turn, .. }) => {
                    if let Some(mut state) = world.get_mut::<RigDriverState>(run) {
                        // 每次模型调用前重录 advertise（defs 本 attempt 固定）
                        let defs = state.tool_defs.clone();
                        state.run.advertise_tools(turn, defs);
                        state.awaiting = RigAwait::Model;
                    }
                    // 生产 worker 在这里消费 (prompt, history, turn) 并回灌 RigModelTurn
                    false
                }
                Ok(AgentRunStep::CallTools { calls }) => {
                    match spawn_tool_batch(world, run, &calls) {
                        Ok(tool_turn) => {
                            if let Some(mut state) = world.get_mut::<RigDriverState>(run) {
                                state.tool_turn = tool_turn;
                                // calls 顺序存入 awaiting（I20 回喂前重排依据）
                                state.awaiting = RigAwait::Tools { calls, tool_turn };
                            }
                            false
                        }
                        Err(err) => {
                            emit_failed(world, run, err);
                            false
                        }
                    }
                }
                Ok(AgentRunStep::Done(response)) => {
                    emit_succeeded(world, run, response.output);
                    false
                }
                Err(err) => {
                    emit_failed(
                        world,
                        run,
                        DriverError::Protocol(DriverProtocolError::InvalidState(err.to_string())),
                    );
                    false
                }
            }
        }
    }
}

/// 处理等待中的工具批次：全部终态后按原始顺序回喂 `tool_results`（I20）。
///
/// 返回 `Ok(true)` 已回喂（可继续 step）；`Ok(false)` 仍等待；`Err` 协议错误。
fn resolve_tools_if_ready(world: &mut World, run: Entity) -> Result<bool, DriverError> {
    let Some(state) = world.get::<RigDriverState>(run) else {
        return Ok(false);
    };
    let RigAwait::Tools { calls, tool_turn } = &state.awaiting else {
        return Ok(false);
    };
    let calls = calls.clone();
    let tool_turn = *tool_turn;

    // 期望的 ECS 执行数（preresolved 不进 ECS，I19）
    let expected_ecs = calls
        .iter()
        .filter(|c| c.preresolved_result.is_none())
        .count();

    // 收割该批次终态结果
    let mut terminal: HashMap<String, (String, serde_json::Value)> = HashMap::new();
    {
        let mut query = world.query::<(
            &ToolInvocationCall,
            &ToolInvocationStatus,
            &ToolInvocationTurn,
            Option<&ToolInvocationOutput>,
            Option<&ToolInvocationError>,
        )>();
        for (call, status, turn, output, error) in query.iter(world) {
            if call.0.run != run || turn.0 != tool_turn {
                continue;
            }
            match status {
                ToolInvocationStatus::Completed => {
                    let value = output.map(|o| o.0.value.clone()).unwrap_or(serde_json::Value::Null);
                    terminal.insert(call.0.call_id.clone(), (call.0.name.clone(), value));
                }
                ToolInvocationStatus::Failed => {
                    let err = error
                        .map(|e| e.0.clone())
                        .unwrap_or_else(|| "tool failed".into());
                    terminal.insert(
                        call.0.call_id.clone(),
                        (call.0.name.clone(), json!({ "error": err })),
                    );
                }
                ToolInvocationStatus::Queued | ToolInvocationStatus::Running => {}
            }
        }
    }

    if terminal.len() < expected_ecs {
        return Ok(false); // 还有在途
    }

    // 按原始顺序重排（I20），preresolved 直接回灌（I19）
    let mut results = Vec::with_capacity(calls.len());
    for call in &calls {
        if let Some(content) = &call.preresolved_result {
            results.push(content.clone());
            continue;
        }
        let call_id = call.tool_call.id.as_str();
        let Some((name, payload)) = terminal.get(call_id) else {
            return Err(DriverError::Protocol(DriverProtocolError::InvalidState(
                "tool result missing for call id (desync?)".into(),
            )));
        };
        results.push(tool_result(call_id, name, payload.clone())?);
    }

    // 回喂 AgentRun
    {
        let Some(mut state) = world.get_mut::<RigDriverState>(run) else {
            return Ok(false);
        };
        state
            .run
            .tool_results(results)
            .map_err(|err| DriverError::Protocol(DriverProtocolError::InvalidState(err.to_string())))?;
        state.awaiting = RigAwait::Ready;
    }
    Ok(true)
}

/// 把 `CallTools` 翻译成 ECS `ToolInvocation`（I3/I18/I19）。
///
/// preresolved 调用**不产生 ECS 实体**（I19）；其余逐个 `pending_to_tool_call`
/// （I18 call_id 严格往返）并落 `ToolInvocation`。返回该批次 `tool_turn`。
/// 同批内 call_id 重复 → 拒绝（§8 测试矩阵）。
fn spawn_tool_batch(
    world: &mut World,
    run: Entity,
    calls: &[PendingToolCall],
) -> Result<u32, DriverError> {
    // 下一个 tool_turn：用 RigDriverState 里的计数
    let tool_turn = world
        .get::<RigDriverState>(run)
        .map(|s| s.tool_turn.wrapping_add(1))
        .unwrap_or(1);

    let mut seen = std::collections::HashSet::new();
    let mut to_spawn: Vec<crate::tool::ToolCall> = Vec::new();
    for call in calls {
        if call.preresolved_result.is_some() {
            continue; // I19：不进 ECS
        }
        let call_id = call.tool_call.id.as_str();
        if call_id.is_empty() {
            return Err(DriverError::Protocol(DriverProtocolError::MissingToolCallId));
        }
        if !seen.insert(call_id.to_string()) {
            return Err(DriverError::Protocol(DriverProtocolError::DuplicateToolCallId(
                call_id.to_string(),
            )));
        }
        let tool = {
            let registry = world.resource::<ToolRegistry>();
            registry.get_by_name(&call.tool_call.function.name)
        };
        let tool_call = pending_to_tool_call(run, call, tool)?;
        to_spawn.push(tool_call);
    }

    for tool_call in to_spawn {
        world
            .spawn(ToolInvocationBundle::new(tool_call.clone()))
            .insert(ToolInvocationTurn(tool_turn));
        world.write_message(crate::tool::ToolCallRequested { call: tool_call });
    }
    Ok(tool_turn)
}

/// 终态：成功 → bus（coordinator 转 Completed）。
fn emit_succeeded(world: &mut World, run: Entity, output: String) {
    let esc = world.get::<EscalationState>(run).cloned();
    let Some(esc) = esc else { return };
    let bus = world.resource::<DriverEventBus>();
    let _ = bus.sender().send(DriverEvent {
        run,
        epoch: esc.epoch,
        driver: RIG_DRIVER_ID,
        outcome: DriverOutcome::Succeeded { output },
    });
}

/// 终态：失败 → bus（coordinator 决定下一档或 Failed）。
fn emit_failed(world: &mut World, run: Entity, error: DriverError) {
    let esc = world.get::<EscalationState>(run).cloned();
    let Some(esc) = esc else { return };
    let bus = world.resource::<DriverEventBus>();
    let _ = bus.sender().send(DriverEvent {
        run,
        epoch: esc.epoch,
        driver: RIG_DRIVER_ID,
        outcome: DriverOutcome::Failed { error },
    });
}
