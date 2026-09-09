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
//! **I26（RigModelInbox 是唯一注入点）**：`AgentRun::model_response` 只能从
//! [`RigModelInbox`] 的 drain 路径（[`apply_model_turn`]）触达——
//! `RigDriverState` 的字段全部私有，类型上封死其它注入口。
//!
//! **I22（worker 边界）**：`model.completion(req).await` 是**唯一的 async
//! 点**，只发生在 `RigModelInbox::spawn` 出的 worker 线程上
//! （`RigRuntime::block_on`）；ECS 侧只做 `next_step`（sans-I/O）与 drain。

use std::collections::{BTreeSet, HashMap};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};

use bevy_ecs::prelude::*;
use rig_core::completion::message::{Message, UserContent};
use rig_core::completion::{
    CompletionModel, CompletionRequest, CompletionResponse, ToolDefinition,
};
use rig_run::{AgentRun, AgentRunStep, ModelTurn, PendingToolCall};
use serde_json::json;

use crate::agent::NeedleAgentSpec;
use crate::escalate::{
    DriverError, DriverEvent, DriverEventBus, DriverId, DriverOutcome, DriverProtocolError,
    EscalationState,
};
use crate::run::{RunOwner, RunSession, RunStatus};
use crate::session::collect_transcript;
use crate::tool::{
    ToolInvocationBundle, ToolInvocationCall, ToolInvocationError, ToolInvocationOutput,
    ToolInvocationStatus, ToolInvocationTurn, ToolRegistry, ToolSpec,
};

use super::runtime::RigRuntime;
use super::tool_bridge::{definitions, pending_to_tool_call, tool_result, tripwires};
use super::transcript::transcript_to_history;

/// 每帧步进的最大轮数（防一帧内无限循环；`AgentRun` 自身有 `max_turns` 预算）。
const MAX_STEPS_PER_FRAME: usize = 8;

/// rig attempt 的内部状态组件（I4：`AgentRun` 只是一次 attempt 的状态）。
///
/// **字段全部私有（I26）**：`AgentRun::model_response` 的唯一合法触发路径是
/// [`RigModelInbox`] 回灌（[`apply_model_turn`]）；外部只读
/// [`RigDriverState::awaiting`] / [`RigDriverState::tool_turn`]。
#[derive(Component)]
pub struct RigDriverState {
    /// rig 协议状态机（sans-I/O，无 runtime）。
    run: AgentRun,
    /// 创建本状态时的 attempt epoch（P0-5：每次 attempt 重建——epoch 不匹配
    /// 即视为旧 attempt 残留，ensure_states 重建之）。
    epoch: u64,
    /// 当前等待什么。
    awaiting: RigAwait,
    /// ECS 工具批次的轮次标记（对应 `ToolInvocationTurn`）。
    tool_turn: u32,
    /// 每轮模型调用前 `advertise_tools` 上报的工具定义（本 attempt 固定）。
    tool_defs: Vec<ToolDefinition>,
}

impl RigDriverState {
    /// 只读：当前等待状态（诊断/测试）。
    pub fn awaiting(&self) -> &RigAwait {
        &self.awaiting
    }

    /// 只读：当前工具批次轮次。
    pub fn tool_turn(&self) -> u32 {
        self.tool_turn
    }
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

/// 模型回执（worker → ECS；I17 携带 `(run, epoch)`）。
///
/// `Err` = `model.completion` 失败（规格 §7「模型返回错误 → Failed」）。
pub struct RigModelTurn {
    /// 目标 run。
    pub run: Entity,
    /// attempt 的 epoch。
    pub epoch: u64,
    /// 模型产物或失败原因。
    pub outcome: Result<ModelTurn, DriverError>,
}

/// 模型作业（ECS → worker；I17 携带 `(run, epoch)`）。
pub enum ModelJob<M: CompletionModel + 'static> {
    /// submit 时把 attempt 的模型交给 worker（键 = run 实体）。
    Attach {
        /// 目标 run。
        run: Entity,
        /// attempt 的 epoch。
        epoch: u64,
        /// 启动期预热完成的共享模型（P1-7：`M` 泛型——`CompletionModel`
        /// 是 RPITIT，非 dyn-compatible）。
        model: Arc<M>,
    },
    /// CallModel 作业：worker 侧 `completion().await`（I22）。
    CallModel {
        /// 目标 run。
        run: Entity,
        /// attempt 的 epoch。
        epoch: u64,
        /// 规范请求（prompt 已折进 `chat_history` 末位——上游 builder 语义）。
        request: CompletionRequest,
        /// advertise 的工具名集合（回执转 `ModelTurn` 时填 executable/allowed）。
        tool_names: BTreeSet<String>,
    },
    /// 停止 worker（§7 shutdown 行：stop accepting → join）。
    Shutdown,
}

/// rig 模型链路资源（`M` 泛型；[`ModelJob`] 出站 + [`RigModelTurn`] 回灌）。
///
/// **I26**：模型回执只从本资源的回灌端进入 ECS；**I22**：`completion().await`
/// 只在本资源 spawn 的 worker 线程上执行。
#[derive(Resource)]
pub struct RigModelInbox<M: CompletionModel + 'static> {
    jobs_tx: Sender<ModelJob<M>>,
    turns_tx: Sender<RigModelTurn>,
    turns_rx: Mutex<Receiver<RigModelTurn>>,
    _worker: Option<std::thread::JoinHandle<()>>,
}

impl<M: CompletionModel + 'static> RigModelInbox<M> {
    /// 构造并启动 worker 线程（`completion().await` 的唯一执行地，I22）。
    pub fn spawn(runtime: RigRuntime) -> Self {
        let (jobs_tx, jobs_rx) = std::sync::mpsc::channel::<ModelJob<M>>();
        let (turns_tx, turns_rx) = std::sync::mpsc::channel::<RigModelTurn>();
        let worker_tx = turns_tx.clone();
        let worker = std::thread::Builder::new()
            .name("bevy_needle_rig_model".into())
            .spawn(move || model_worker_loop(runtime, jobs_rx, worker_tx))
            .ok();
        Self {
            jobs_tx,
            turns_tx,
            turns_rx: Mutex::new(turns_rx),
            _worker: worker,
        }
    }

    /// 作业发送端（[`super::driver::RigDriver`] 持有；submit 时发 Attach）。
    pub fn jobs(&self) -> Sender<ModelJob<M>> {
        self.jobs_tx.clone()
    }

    /// 便捷注入（worker 语义之外的**测试**注入口；仍走回灌通道，I26 不破）。
    pub fn inject(&self, msg: RigModelTurn) {
        let _ = self.turns_tx.send(msg);
    }

    /// 出站：提交 CallModel 作业（通道断开 = worker 已死，返回 false 由调用方
    /// 按 §7「TransportError」处理）。
    fn submit_call_model(
        &self,
        run: Entity,
        epoch: u64,
        request: CompletionRequest,
        tool_names: BTreeSet<String>,
    ) -> bool {
        self.jobs_tx
            .send(ModelJob::CallModel {
                run,
                epoch,
                request,
                tool_names,
            })
            .is_ok()
    }

    fn drain(&self) -> Vec<RigModelTurn> {
        let mut out = Vec::new();
        let rx = self.rx();
        loop {
            match rx.try_recv() {
                Ok(msg) => out.push(msg),
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        out
    }

    fn rx(&self) -> std::sync::MutexGuard<'_, Receiver<RigModelTurn>> {
        match self.turns_rx.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}


/// rig 侧身份集合（I31）：**按模型类型隔离**的 `DriverId` 集。
/// `ensure_states::<M>()` 只认领 `esc.driver_id ∈ RigDriverIds<M>` 的 run；
/// 多个 DriverId 共享同一 `M` 时同住一个集合（I32）。
#[derive(Resource)]
pub struct RigDriverIds<M: CompletionModel + 'static>(
    std::collections::HashSet<DriverId>,
    std::marker::PhantomData<fn() -> M>,
);

impl<M: CompletionModel + 'static> Default for RigDriverIds<M> {
    fn default() -> Self {
        Self(std::collections::HashSet::new(), std::marker::PhantomData)
    }
}

impl<M: CompletionModel + 'static> RigDriverIds<M> {
    /// 该 M 是否已接管过此身份。
    pub fn contains(&self, id: &DriverId) -> bool {
        self.0.contains(id)
    }

    /// 登记身份（register 成功后调用）。
    pub fn insert(&mut self, id: DriverId) {
        self.0.insert(id);
    }

    /// 已登记的身份数（诊断）。
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// 是否为空（诊断）。
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<M: CompletionModel + 'static> std::fmt::Debug for RigDriverIds<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("RigDriverIds").field(&self.0).finish()
    }
}

impl<M: CompletionModel + 'static> bevy_ecs::world::FromWorld for RigModelInbox<M> {
    fn from_world(_world: &mut bevy_ecs::world::World) -> Self {
        Self::spawn(RigRuntime::lazy())
    }
}

impl<M: CompletionModel + 'static> std::fmt::Debug for RigModelInbox<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RigModelInbox")
    }
}

/// worker 循环（I22 的唯一 block_on 所在地；§7 shutdown：sender drop → 循环退出）。
fn model_worker_loop<M: CompletionModel + 'static>(
    runtime: RigRuntime,
    jobs: Receiver<ModelJob<M>>,
    turns: Sender<RigModelTurn>,
) {
    let mut models: HashMap<Entity, Arc<M>> = HashMap::new();
    while let Ok(job) = jobs.recv() {
        match job {
            ModelJob::Shutdown => break,
            ModelJob::Attach { run, epoch: _, model } => {
                models.insert(run, model);
            }
            ModelJob::CallModel {
                run,
                epoch,
                request,
                tool_names,
            } => {
                // 未 Attach = driver 未接管该 run：**必须回执失败**，静默丢弃
                // 会让 run 悬挂在 awaiting=Model（违反 I9「不悬挂」精神）。
                let Some(model) = models.get(&run) else {
                    let _ = turns.send(RigModelTurn {
                        run,
                        epoch,
                        outcome: Err(DriverError::Unavailable(format!(
                            "no model attached for run (submit handshake missing)"
                        ))),
                    });
                    continue;
                };
                // I22：completion().await 只在此处 block_on
                let outcome = match runtime.block_on(model.completion(request)) {
                    Ok(Ok(resp)) => Ok(model_turn_from_response(resp, &tool_names)),
                    Ok(Err(err)) => Err(DriverError::Model(err.to_string())),
                    Err(err) => Err(DriverError::Transport(err)),
                };
                let _ = turns.send(RigModelTurn {
                    run,
                    epoch,
                    outcome,
                });
            }
        }
    }
}

/// `CompletionResponse` → [`ModelTurn`]（worker 侧规范转换）。
///
/// `executable`/`allowed` 集合来自 advertise 面（留空会把 tool call 判非法，
/// 上游协议校验面，见 [`model_turn_with_tools`]）。
fn model_turn_from_response(resp: CompletionResponse, names: &BTreeSet<String>) -> ModelTurn {
    ModelTurn::new(
        resp.message_id.clone(),
        resp.choice,
        resp.usage,
        names.clone(),
        names.clone(),
    )
    .with_identity(resp.response_id, resp.provider_request_id)
    .with_raw(resp.raw)
}

/// 从 provider 响应构造 [`ModelTurn`]（worker 内部使用；导出供测试/工具）。
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
    let names: BTreeSet<String> = advertised.iter().map(|d| d.name.clone()).collect();
    ModelTurn::new(message_id, choice, usage, names.clone(), names)
}

/// rig 步进系统（注册在 `RunResolutionSystems`，**排在 coordinator 之前**）。
///
/// 每帧三件事：① 回灌模型回合；② 为 rig 接管的升级创建 `RigDriverState`；
/// ③ 步进所有 rig run（工具批次终态 → `tool_results` 回喂 → 继续 `next_step`）。
pub fn rig_step_system<M: CompletionModel + 'static>(world: &mut World) {
    // ① 回灌模型回合（stale epoch 丢弃，I17）
    let turns = world.resource::<RigModelInbox<M>>().drain();
    for msg in turns {
        apply_model_turn(world, msg);
    }

    // ② 为 rig 接管的升级创建状态（AgentRun 重建，P0-5；认领按 I31 身份集合）
    ensure_states::<M>(world);

    // ③ 步进
    step_all::<M>(world);
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
    let state_epoch = world
        .get::<RigDriverState>(msg.run)
        .map(|s| s.epoch)
        .unwrap_or(u64::MAX);
    if state_epoch != esc.epoch {
        return; // 旧 attempt 的回执：新状态尚未就绪或已换代，丢弃（I17）
    }

    let turn = match msg.outcome {
        Ok(turn) => turn,
        // 模型调用失败（worker 回执）：规格 §7「模型返回错误 → Failed」
        Err(err) => {
            emit_failed(world, msg.run, err);
            return;
        }
    };
    let result = {
        let Some(mut state) = world.get_mut::<RigDriverState>(msg.run) else {
            return;
        };
        if !matches!(state.awaiting, RigAwait::Model) {
            return; // 不该有模型回合
        }
        match state.run.model_response(turn) {
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
fn ensure_states<M: CompletionModel + 'static>(world: &mut World) {
    // I31：只认领「本 M 已登记身份」的 run——身份是实例字段（I30），
    // 不再依赖任何固定常量。多个 M 各认各的，互不争抢。
    let claimed_ids = world
        .get_resource::<RigDriverIds<M>>()
        .map(|ids| ids.0.clone())
        .unwrap_or_default();
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
                    if claimed_ids.contains(&esc.driver_id)
                        && state.map(|s| s.epoch != esc.epoch).unwrap_or(true) =>
                {
                    Some((run, esc.clone()))
                }
                _ => None,
            })
            .collect()
    };

    for (run, esc) in candidates {
        // P0-5：每次 attempt 重建——旧 epoch 的残留状态先移除（其 awaiting 可能
        // 停在 Model/Tools，新 attempt 必须从 Ready 开始）。
        if let Ok(mut entity) = world.get_entity_mut(run) {
            entity.remove::<RigDriverState>();
        }
        match build_agent_run(world, run) {
            Ok((agent_run, tool_defs)) => {
                let state = RigDriverState {
                    run: agent_run,
                    epoch: esc.epoch,
                    awaiting: RigAwait::Ready,
                    tool_turn: 0,
                    tool_defs,
                };
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
fn step_all<M: CompletionModel + 'static>(world: &mut World) {
    // I31：步进与认领同源——只步进本 M 身份集合内的 run，防止同一 state
    // 被多个 M 的系统各步进一次（CallModel 双发）。
    let claimed_ids = world
        .get_resource::<RigDriverIds<M>>()
        .map(|ids| ids.0.clone())
        .unwrap_or_default();
    let runs: Vec<Entity> = {
        let mut query = world.query::<(Entity, &RunStatus, &RigDriverState, &EscalationState)>();
        query
            .iter(world)
            .filter_map(|(run, status, _state, esc)| match status {
                RunStatus::Escalating { .. } if claimed_ids.contains(&esc.driver_id) => Some(run),
                _ => None,
            })
            .collect()
    };

    for run in runs {
        for _ in 0..MAX_STEPS_PER_FRAME {
            if !step_once::<M>(world, run) {
                break;
            }
        }
    }
}

/// 步进单个 run 一步；返回是否可继续（可继续 = 工具批次已完成，应再 step）。
fn step_once<M: CompletionModel + 'static>(world: &mut World, run: Entity) -> bool {
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
                Ok(AgentRunStep::CallModel {
                    prompt,
                    history,
                    turn,
                }) => {
                    // 每次模型调用前重录 advertise（defs 本 attempt 固定）
                    let (defs, _esc) = {
                        let Some(mut state) = world.get_mut::<RigDriverState>(run) else {
                            return false;
                        };
                        let defs = state.tool_defs.clone();
                        state.run.advertise_tools(turn, defs.clone());
                        state.awaiting = RigAwait::Model;
                        (defs, ())
                    };
                    let Some(esc) = world.get::<EscalationState>(run).cloned() else {
                        return false;
                    };
                    // 规范请求：prompt 折进 chat_history 末位（上游 builder 语义）；
                    // system 已由 M1 快照作为首条 System 消息进入 history → preamble 为空
                    let mut chat_history = history;
                    chat_history.push(prompt);
                    let request = CompletionRequest {
                        model: None,
                        preamble: None,
                        chat_history,
                        documents: Vec::new(),
                        tools: defs.clone(),
                        temperature: None,
                        max_tokens: None,
                        tool_choice: None,
                        additional_params: None,
                        output_schema: None,
                        record_telemetry_content: false,
                    };
                    let tool_names: BTreeSet<String> = defs.iter().map(|d| d.name.clone()).collect();
                    world
                        .resource::<RigModelInbox<M>>()
                        .submit_call_model(run, esc.epoch, request, tool_names);
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
///
/// I30：事件的身份字段取 `EscalationState.driver_id`（真实执行者），
/// 不是任何常量。
fn emit_succeeded(world: &mut World, run: Entity, output: String) {
    let esc = world.get::<EscalationState>(run).cloned();
    let Some(esc) = esc else { return };
    let bus = world.resource::<DriverEventBus>();
    let _ = bus.sender().send(DriverEvent {
        run,
        epoch: esc.epoch,
        driver: esc.driver_id,
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
        driver: esc.driver_id,
        outcome: DriverOutcome::Failed { error },
    });
}
