//! M1 转录快照桥：needle 会话（ECS `ChatMessage*`）→ rig `Message` 历史。
//!
//! 规格 §5.3 / I14：首次交接时 needle 段是 ECS 实体，用 `collect_transcript`
//! （按 `ChatMessageSeq` 排序）取快照；**绝不**用 Entity id 或 HashMap 顺序。
//!
//! 语义映射（needle → rig）：
//! - `System` → `Message::System`（环境事实，只保留首条）；
//! - `User` → `Message::User { content: [Text] }`；
//! - `Assistant` → `Message::Assistant { content: [Text] }`——但 `[已升级]` /
//!   `[失败]` / `[已取消]` 是**宿主动作**的落转录，不是模型输出，跳过；
//! - `Tool` → 跳过（M1 只交接对话流；工具轮由 rig 段自己的
//!   tool_call/tool_result 结构管理，见 `tool_bridge`）。
//!
//! 空文本纪律：`Message::User/Assistant` 空文本会被上游 `EMPTY_RESPONSE_ERROR`
//! 语义拒绝，映射时跳过空串。

use rig_core::completion::message::{AssistantContent, Message, UserContent};

use crate::session::ChatMessageRole;

/// M1 快照：把 `collect_transcript` 的输出（已按 `ChatMessageSeq` 排序，
/// I14）转换为 rig 历史。
///
/// `system_facts` 为 agent 的环境事实（`NeedleAgentSpec::system_facts`），
/// 作为首条 `Message::System` 注入；转录里重复的 System 条目去重跳过。
///
/// 返回的历史可直接喂 `AgentRun::with_history`；调用方建议再过一遍
/// `rig_run::transcript::validate_canonical`（上游校验器，规格 §5.3）。
pub fn transcript_to_history(
    system_facts: Option<&str>,
    transcript: &[(ChatMessageRole, String)],
) -> Vec<Message> {
    let mut history = Vec::new();
    let mut system_seen = false;

    if let Some(facts) = system_facts.map(str::trim).filter(|f| !f.is_empty()) {
        history.push(Message::System {
            content: facts.to_string(),
        });
        system_seen = true;
    }

    for (role, raw_text) in transcript {
        let text = raw_text.trim();
        if text.is_empty() {
            continue;
        }
        match role {
            ChatMessageRole::System => {
                if system_seen {
                    continue;
                }
                history.push(Message::System {
                    content: text.to_string(),
                });
                system_seen = true;
            }
            ChatMessageRole::User => {
                history.push(Message::User {
                    content: vec![UserContent::text(text)],
                });
            }
            ChatMessageRole::Assistant => {
                // needle 宿主动作的落转录不是模型输出，不进 rig 历史
                if text.starts_with("[已升级]")
                    || text.starts_with("[失败]")
                    || text.starts_with("[已取消]")
                {
                    continue;
                }
                history.push(Message::Assistant {
                    id: None,
                    content: vec![AssistantContent::text(text)],
                });
            }
            ChatMessageRole::Tool => {
                // M1 只交接对话流；工具轮由 rig 段自己管理
            }
        }
    }
    history
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(role: ChatMessageRole, text: &str) -> (ChatMessageRole, String) {
        (role, text.to_string())
    }

    #[test]
    fn maps_roles_and_injects_system() {
        let transcript = vec![
            entry(ChatMessageRole::System, "device: desktop"),
            entry(ChatMessageRole::User, "set volume to 80"),
            entry(ChatMessageRole::Assistant, "volume_set"),
            entry(ChatMessageRole::Tool, r#"{"ok":true}"#),
        ];
        let history = transcript_to_history(Some("date: 2026-09-08"), &transcript);
        assert_eq!(history.len(), 3);
        assert!(matches!(&history[0], Message::System { content } if content == "date: 2026-09-08"));
        assert!(matches!(&history[1], Message::User { .. }));
        assert!(matches!(&history[2], Message::Assistant { .. }));
    }

    #[test]
    fn skips_host_actions_tool_rows_duplicate_system_and_empty() {
        let transcript = vec![
            entry(ChatMessageRole::System, "device: desktop"),
            entry(ChatMessageRole::System, "重复 system 去重"),
            entry(ChatMessageRole::User, "hi"),
            entry(ChatMessageRole::Assistant, "[已升级] 置信度 0.05 低于门限 0.50"),
            entry(ChatMessageRole::Assistant, "[失败] engine blew up"),
            entry(ChatMessageRole::Tool, "tool result"),
            entry(ChatMessageRole::Assistant, "   "),
        ];
        let history = transcript_to_history(Some("device: desktop"), &transcript);
        assert_eq!(history.len(), 2, "system+user only: {history:?}");
        assert!(matches!(&history[0], Message::System { .. }));
        assert!(matches!(&history[1], Message::User { .. }));
    }

    #[test]
    fn empty_facts_and_transcript_give_empty_history() {
        assert!(transcript_to_history(Some("  "), &[]).is_empty());
    }
}
