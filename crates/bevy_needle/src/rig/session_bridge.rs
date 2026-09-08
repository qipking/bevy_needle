//! M2 会话记忆桥：`impl ConversationMemory`（rig 段 tier 链内部共享，规格 §5.3）。
//!
//! rig 的 `ConversationMemory` 只覆盖 rig 段（needle 轮次是 ECS 实体，走 M1
//! 快照交接——见 [`super::transcript`]）。本实现把 rig 段历史**镜像**到内存
//! buffer，配合上游 `Arc<M>` blanket impl：一个 `Arc<RigSessionBridge>` 可挂
//! 多个 tier 零克隆共享。
//!
//! 契约（上游 trait 文档原文）：`load` 返回完整历史（空会话返回空 Vec）；
//! `append` 由 agent 在成功 turn 之后调用（cheap）；`clear` 清空指定会话。
//!
//! future 形态：`WasmBoxedFuture<'a, ..>`，方法体直接 `Box::pin(async move { .. })`，
//! 无执行器依赖——与 rig-run 的 sans-I/O 立场一致。

use std::collections::HashMap;
use std::sync::Mutex;

use rig_core::completion::message::Message;
use rig_core::memory::{ConversationMemory, MemoryError};
use rig_core::wasm_compat::WasmBoxedFuture;

/// rig 段会话历史的内存镜像（按 `conversation_id` 分区）。
///
/// 预留升级点：把内部 buffer 换成「读 ECS 转录 + M1 映射」的实现，即可让
/// rig 段直接共享 needle 会话（两段收敛为一段半——规格 §5.3；需先确认
/// `validate_canonical` 对 needle `Tool` role 的覆盖，Q15）。
#[derive(Default)]
pub struct RigSessionBridge {
    inner: Mutex<HashMap<String, Vec<Message>>>,
}

impl RigSessionBridge {
    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 只读快照（诊断/存档用；不经过 trait 的异步边界）。
    pub fn snapshot(&self, conversation_id: &str) -> Vec<Message> {
        self.inner
            .lock()
            .map(|guard| guard.get(conversation_id).cloned().unwrap_or_default())
            .unwrap_or_default()
    }
}

impl ConversationMemory for RigSessionBridge {
    fn load<'a>(
        &'a self,
        conversation_id: &'a str,
    ) -> WasmBoxedFuture<'a, Result<Vec<Message>, MemoryError>> {
        Box::pin(async move { Ok(self.snapshot(conversation_id)) })
    }

    fn append<'a>(
        &'a self,
        conversation_id: &'a str,
        messages: Vec<Message>,
    ) -> WasmBoxedFuture<'a, Result<(), MemoryError>> {
        Box::pin(async move {
            match self.inner.lock() {
                Ok(mut map) => {
                    map.entry(conversation_id.to_string())
                        .or_default()
                        .extend(messages);
                    Ok(())
                }
                Err(_) => Err(MemoryError::Internal("RigSessionBridge lock poisoned".into())),
            }
        })
    }

    fn clear<'a>(
        &'a self,
        conversation_id: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), MemoryError>> {
        Box::pin(async move {
            match self.inner.lock() {
                Ok(mut map) => {
                    map.remove(conversation_id);
                    Ok(())
                }
                Err(_) => Err(MemoryError::Internal("RigSessionBridge lock poisoned".into())),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::completion::message::{AssistantContent, UserContent};
    use std::task::{Context, Poll, Waker};

    fn poll_now<R>(fut: &mut WasmBoxedFuture<'_, Result<R, MemoryError>>) -> Result<R, MemoryError> {
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        match std::pin::Pin::new(fut).poll(&mut cx) {
            Poll::Ready(r) => r,
            Poll::Pending => unreachable!("memory ops complete synchronously"),
        }
    }

    fn msg_pair() -> Vec<Message> {
        vec![
            Message::User {
                content: vec![UserContent::text("hi")],
            },
            Message::Assistant {
                id: None,
                content: vec![AssistantContent::text("hello")],
            },
        ]
    }

    #[test]
    fn append_load_clear_roundtrip() {
        let bridge = RigSessionBridge::new();
        let conv = String::from("conv-1");

        let mut fut = ConversationMemory::load(&bridge, &conv);
        assert!(poll_now(&mut fut).unwrap().is_empty());

        let mut fut = ConversationMemory::append(&bridge, &conv, msg_pair());
        poll_now(&mut fut).unwrap();
        let mut fut = ConversationMemory::load(&bridge, &conv);
        assert_eq!(poll_now(&mut fut).unwrap().len(), 2);

        let other = String::from("conv-2");
        let mut fut = ConversationMemory::load(&bridge, &other);
        assert!(poll_now(&mut fut).unwrap().is_empty());

        let mut fut = ConversationMemory::clear(&bridge, &conv);
        poll_now(&mut fut).unwrap();
        let mut fut = ConversationMemory::load(&bridge, &conv);
        assert!(poll_now(&mut fut).unwrap().is_empty());
    }
}
