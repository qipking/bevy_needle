//! 会话与转录：把对话历史留在 ECS 世界里（对齐 bevy_rig 的 `session.rs`）。
//!
//! Needle 引擎自身维护 256 token 滑窗（工具集作为 KV sink 钉住），
//! 这里的会话实体只做镜像持久化，供游戏查询/调试/存档。
//!
//! 顺序用显式 [`ChatMessageSeq`] 保证：CommandQueue 的实体物化顺序
//! 不等于调用顺序，不能依赖实体 id 排序。

use std::sync::atomic::{AtomicU64, Ordering};

use bevy_ecs::prelude::*;

static NEXT_MESSAGE_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
/// `Session`（见类型级与模块级文档）。
pub struct Session;

#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
/// `ChatMessage`（见类型级与模块级文档）。
pub struct ChatMessage;

#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
/// `ChatMessageRole`（见类型级与模块级文档）。
pub enum ChatMessageRole {
    #[default]
    /// 环境事实（对应引擎的 system 轮）。
    System,
    /// 用户输入。
    User,
    /// 引擎/模型侧内容（推导链或结果）。
    Assistant,
    /// 工具执行结果。
    Tool,
}

#[derive(Component, Clone, Debug, Default, PartialEq, Eq)]
/// `ChatMessageText`（见类型级与模块级文档）。
pub struct ChatMessageText(pub String);

/// 消息的显式时间序号（spawn 时分配，单调递增）。
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChatMessageSeq(pub u64);

#[derive(Bundle)]
/// `ChatMessageBundle`（见类型级与模块级文档）。
pub struct ChatMessageBundle {
    /// `message`（语义见类型文档）。
    pub message: ChatMessage,
    /// `role`（语义见类型文档）。
    pub role: ChatMessageRole,
    /// `text`（语义见类型文档）。
    pub text: ChatMessageText,
    /// `owner`（语义见类型文档）。
    pub owner: ChatMessageSession,
    /// `seq`（语义见类型文档）。
    pub seq: ChatMessageSeq,
}

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
/// `ChatMessageSession`（见类型级与模块级文档）。
pub struct ChatMessageSession(pub Entity);

#[derive(Bundle)]
/// `SessionBundle`（见类型级与模块级文档）。
pub struct SessionBundle {
    /// `session`（语义见类型文档）。
    pub session: Session,
}

/// 构造/执行入口（错误经 `Result` 返回，不 panic）。
pub fn spawn_session(world: &mut World) -> Entity {
    world.spawn(SessionBundle { session: Session }).id()
}

/// 通过 Commands 生成消息（延迟应用；序号在入队时已确定）。
pub fn spawn_chat_message(
    commands: &mut Commands,
    session: Entity,
    role: ChatMessageRole,
    text: impl Into<String>,
) -> Entity {
    let seq = NEXT_MESSAGE_SEQ.fetch_add(1, Ordering::Relaxed);
    commands
        .spawn(ChatMessageBundle {
            message: ChatMessage,
            role,
            text: ChatMessageText(text.into()),
            owner: ChatMessageSession(session),
            seq: ChatMessageSeq(seq),
        })
        .id()
}

/// 立即在 World 上生成消息（独占系统内同步生效）。
pub fn spawn_chat_message_now(
    world: &mut World,
    session: Entity,
    role: ChatMessageRole,
    text: impl Into<String>,
) -> Entity {
    let seq = NEXT_MESSAGE_SEQ.fetch_add(1, Ordering::Relaxed);
    world
        .spawn(ChatMessageBundle {
            message: ChatMessage,
            role,
            text: ChatMessageText(text.into()),
            owner: ChatMessageSession(session),
            seq: ChatMessageSeq(seq),
        })
        .id()
}

/// 与 bevy_rig 对齐的转录读取器（按显式序号排序）。
pub fn collect_transcript(world: &mut World, session: Entity) -> Vec<(ChatMessageRole, String)> {
    let mut query = world.query::<(
        &ChatMessageSession,
        &ChatMessageRole,
        &ChatMessageText,
        &ChatMessageSeq,
    )>();
    let mut ordered: Vec<(u64, (ChatMessageRole, String))> = query
        .iter(world)
        .filter(|(owner, _, _, _)| owner.0 == session)
        .map(|(_, role, text, seq)| (seq.0, (role.clone(), text.0.clone())))
        .collect();
    ordered.sort_by_key(|(seq, _)| *seq);
    ordered.into_iter().map(|(_, pair)| pair).collect()
}
