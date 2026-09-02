//! Run 生命周期：一次"文字指令 → 引擎解析 → 工具执行 → 结果回喂"的完整流程
//! （对齐 bevy_rig 的 `run.rs`）。
//!
//! 状态机：
//!
//! ```text
//! RunAgent 消息 → Queued → Running ⇄ (engine turn | awaiting tools) → Completed/Failed/Cancelled
//! ```
//!
//! Running 期间用 `RunEngineInFlight` 表示有引擎调用在后台线程执行，
//! `RunAwaitingTools` 表示本轮工具调用等待执行与回喂。

use bevy_ecs::{message::Messages, prelude::*};
use serde_json::Value;

use crate::{
    agent::PrimarySession,
    session::{spawn_chat_message, ChatMessageRole},
};

#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
/// `Run`（见类型级与模块级文档）。
pub struct Run;

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
/// `RunOwner`（见类型级与模块级文档）。
pub struct RunOwner(pub Entity);

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
/// `RunSession`（见类型级与模块级文档）。
pub struct RunSession(pub Entity);

#[derive(Component, Clone, Debug, PartialEq, Eq)]
/// `RunRequest`（见类型级与模块级文档）。
pub struct RunRequest {
    /// `prompt`（语义见类型文档）。
    pub prompt: String,
}

#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
/// `RunTurn`（见类型级与模块级文档）。
pub struct RunTurn(pub u32);

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
/// `RunStatus`（见类型级与模块级文档）。
pub enum RunStatus {
    /// 已接收，等待引擎快照与首轮提交。
    Queued,
    /// 至少一轮在途（引擎解码或工具执行中）。
    Running,
    /// 正常收尾（respond / 空调用 / 步数耗尽）。
    Completed,
    /// 失败（引擎错误、置信度门控等）。
    Failed,
    /// 逻辑取消（在途结果将被丢弃）。
    Cancelled,
}

/// 本轮等待执行/回喂的工具调用数量。
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunAwaitingTools {
    /// `expected`（语义见类型文档）。
    pub expected: u32,
}

/// 有引擎调用在途（防止重复提交）。
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunEngineInFlight;

/// 发给引擎的下一轮输入（首轮是用户文本，后续轮是工具结果 JSON）。
#[derive(Component, Clone, Debug, Default, PartialEq, Eq)]
pub struct RunPendingInput(pub String);

#[derive(Component, Clone, Debug, Default, PartialEq, Eq)]
/// `RunResultText`（见类型级与模块级文档）。
pub struct RunResultText(pub String);

#[derive(Component, Clone, Debug, PartialEq)]
/// `RunFailure`（见类型级与模块级文档）。
pub struct RunFailure(pub String);

#[derive(Component, Clone, Debug, Default, PartialEq)]
/// `RunNote`（见类型级与模块级文档）。
pub struct RunNote(pub String);

/// 最近一次引擎信封（含 confidence/reasoning/tps）。
#[derive(Component, Clone, Debug, Default)]
pub struct RunLastResponse(pub Option<Value>);

/// 已执行并回喂引擎的工具结果（按轮累积，对应 Python run() 的 results）。
#[derive(Component, Clone, Debug, Default)]
pub struct RunExecutedResults(pub Vec<Value>);

#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
/// `RunFinalized`（见类型级与模块级文档）。
pub struct RunFinalized;

#[derive(Bundle)]
/// `RunBundle`（见类型级与模块级文档）。
pub struct RunBundle {
    /// `run`（语义见类型文档）。
    pub run: Run,
    /// `owner`（语义见类型文档）。
    pub owner: RunOwner,
    /// `session`（语义见类型文档）。
    pub session: RunSession,
    /// `request`（语义见类型文档）。
    pub request: RunRequest,
    /// `turn`（语义见类型文档）。
    pub turn: RunTurn,
    /// `status`（语义见类型文档）。
    pub status: RunStatus,
    /// `pending_input`（语义见类型文档）。
    pub pending_input: RunPendingInput,
}

impl RunBundle {
    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn new(owner: Entity, session: Entity, prompt: impl Into<String>) -> Self {
        Self {
            run: Run,
            owner: RunOwner(owner),
            session: RunSession(session),
            request: RunRequest {
                prompt: prompt.into(),
            },
            turn: RunTurn(0),
            status: RunStatus::Queued,
            pending_input: RunPendingInput::default(),
        }
    }
}

/// 触发一次 run（对齐 bevy_rig 的 `RunAgent`）。
#[derive(Message, Clone, Debug)]
pub struct RunAgent {
    /// `agent`（语义见类型文档）。
    pub agent: Entity,
    /// `prompt`（语义见类型文档）。
    pub prompt: String,
}

impl RunAgent {
    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn new(agent: Entity, prompt: impl Into<String>) -> Self {
        Self {
            agent,
            prompt: prompt.into(),
        }
    }
}

#[derive(Message, Clone, Copy, Debug)]
/// `RunCommitted`（见类型级与模块级文档）。
pub struct RunCommitted {
    /// `run`（语义见类型文档）。
    pub run: Entity,
}

#[derive(Message, Clone, Debug)]
/// `RunFailed`（见类型级与模块级文档）。
pub struct RunFailed {
    /// `run`（语义见类型文档）。
    pub run: Option<Entity>,
    /// `error`（语义见类型文档）。
    pub error: String,
}

/// 置信度低于门限：按 Needle 约定应"升级"而不是执行。
#[derive(Message, Clone, Debug)]
pub struct RunEscalation {
    /// `run`（语义见类型文档）。
    pub run: Entity,
    /// `confidence`（语义见类型文档）。
    pub confidence: f64,
    /// `threshold`（语义见类型文档）。
    pub threshold: f32,
}

#[derive(Message, Clone, Copy, Debug)]
/// `CancelRun`（见类型级与模块级文档）。
pub struct CancelRun {
    /// `run`（语义见类型文档）。
    pub run: Entity,
}

/// 重置 agent 的引擎会话（对话回退，工具集保留）。
#[derive(Message, Clone, Copy, Debug)]
pub struct ResetAgent {
    /// `agent`（语义见类型文档）。
    pub agent: Entity,
}

/// RunPreparation 阶段：把 RunAgent 消息物化为 run 实体。
pub fn capture_run_requests(world: &mut World) {
    let requests: Vec<RunAgent> = {
        let mut messages = world.resource_mut::<Messages<RunAgent>>();
        messages.drain().collect()
    };

    for message in requests {
        let Some(session) = world.get::<PrimarySession>(message.agent).map(|s| s.0) else {
            world.write_message(RunFailed {
                run: None,
                error: format!("agent {:?} 缺少 PrimarySession", message.agent),
            });
            continue;
        };

        spawn_chat_message(
            &mut world.commands(),
            session,
            ChatMessageRole::User,
            message.prompt.clone(),
        );

        let mut entity = world.spawn(RunBundle::new(
            message.agent,
            session,
            message.prompt.clone(),
        ));
        entity.insert(RunPendingInput(message.prompt));
    }
}

/// 逻辑取消：引擎调用不可中断，只丢弃其结果。
pub fn cancel_runs(world: &mut World) {
    let cancellations: Vec<CancelRun> = {
        let mut messages = world.resource_mut::<Messages<CancelRun>>();
        messages.drain().collect()
    };
    for message in cancellations {
        if let Ok(mut entity) = world.get_entity_mut(message.run) {
            if let Some(status) = entity.get::<RunStatus>() {
                if matches!(
                    status,
                    RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
                ) {
                    continue;
                }
            }
            entity.insert(RunStatus::Cancelled);
        }
    }
}

/// 构造/执行入口（错误经 `Result` 返回，不 panic）。
pub fn mark_run_completed(world: &mut World, run: Entity, text: impl Into<String>) {
    if let Ok(mut entity) = world.get_entity_mut(run) {
        entity.insert((RunStatus::Completed, RunResultText(text.into())));
    }
}

/// 构造/执行入口（错误经 `Result` 返回，不 panic）。
pub fn mark_run_failed(world: &mut World, run: Entity, error: impl Into<String>) {
    if let Ok(mut entity) = world.get_entity_mut(run) {
        entity.insert((RunStatus::Failed, RunFailure(error.into())));
    }
}

/// RunCommit 阶段：完成的 run 落入会话转录。
pub fn persist_completed_runs(world: &mut World) {
    let ready = finalized_candidates(world, RunStatus::Completed);
    for (run, session, text, _) in ready {
        spawn_chat_message(
            &mut world.commands(),
            session,
            ChatMessageRole::Assistant,
            text,
        );
        world.write_message(RunCommitted { run });
        world.entity_mut(run).insert(RunFinalized);
    }
}

/// RunCommit 阶段：失败的 run 记录错误并落转录。
pub fn persist_failed_runs(world: &mut World) {
    let ready = finalized_candidates(world, RunStatus::Failed);
    for (run, session, text, failure) in ready {
        let error = failure.unwrap_or_else(|| text.clone());
        spawn_chat_message(
            &mut world.commands(),
            session,
            ChatMessageRole::Assistant,
            format!("[失败] {error}"),
        );
        world.write_message(RunFailed {
            run: Some(run),
            error,
        });
        world.entity_mut(run).insert(RunFinalized);
    }
}

/// RunCommit 阶段：取消的 run 收尾。
pub fn persist_cancelled_runs(world: &mut World) {
    let ready = finalized_candidates(world, RunStatus::Cancelled);
    for (run, session, _, _) in ready {
        spawn_chat_message(
            &mut world.commands(),
            session,
            ChatMessageRole::Assistant,
            "[已取消]".to_string(),
        );
        world.write_message(RunCommitted { run });
        world.entity_mut(run).insert(RunFinalized);
    }
}

/// 收集需要收尾的 run（无 RunFinalized 标记的终态 run）。
fn finalized_candidates(
    world: &mut World,
    status: RunStatus,
) -> Vec<(Entity, Entity, String, Option<String>)> {
    let mut query = world.query::<(
        Entity,
        &RunSession,
        &RunStatus,
        Option<&RunResultText>,
        Option<&RunFailure>,
        Option<&RunFinalized>,
    )>();
    query
        .iter(world)
        .filter(|(_, _, run_status, _, _, finalized)| {
            **run_status == status && finalized.is_none()
        })
        .map(|(run, session, _, text, failure, _)| {
            (
                run,
                session.0,
                text.map(|t| t.0.clone()).unwrap_or_default(),
                failure.map(|f| f.0.clone()),
            )
        })
        .collect()
}
