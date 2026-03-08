//! Incoming message handler for Rocket.Chat.
//!
//! Routes incoming messages to the correct loop's `events.jsonl` file as
//! `human.response` or `human.guidance` events, using thread-based (`tmid`),
//! `@loop-id` prefix, or default-to-main routing.

use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::error::{RocketChatError, RocketChatResult};
use crate::state::{RocketChatState, StateManager};
use crate::types::RcMessage;

/// Processes incoming Rocket.Chat messages and writes events to the correct loop's events.jsonl.
pub struct MessageHandler {
    state_manager: StateManager,
    workspace_root: PathBuf,
}

impl MessageHandler {
    /// Create a new message handler rooted at the given workspace.
    pub fn new(state_manager: StateManager, workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            state_manager,
            workspace_root: workspace_root.into(),
        }
    }

    /// Handle an incoming message from Rocket.Chat.
    ///
    /// Determines target loop, classifies as response or guidance, and appends
    /// the appropriate event to the loop's events.jsonl.
    ///
    /// Returns the event topic that was written (`"human.response"` or `"human.guidance"`).
    pub fn handle_message(
        &self,
        state: &mut RocketChatState,
        message: &RcMessage,
    ) -> RocketChatResult<String> {
        let target_loop = self.determine_target_loop(state, &message.msg, message.tmid.as_deref());
        let events_path = self.get_events_path(&target_loop);
        let is_response = state.pending_questions.contains_key(&target_loop);

        let topic = if is_response {
            "human.response"
        } else {
            "human.guidance"
        };

        let timestamp = Utc::now().to_rfc3339();
        let event_json = serde_json::json!({
            "topic": topic,
            "payload": message.msg,
            "ts": timestamp,
        });
        let event_line = serde_json::to_string(&event_json)?;

        self.append_event(&events_path, &event_line)?;

        if is_response {
            self.state_manager
                .remove_pending_question(state, &target_loop)?;
        }

        tracing::info!(
            topic,
            target_loop,
            "wrote {} event for loop {}",
            topic,
            target_loop
        );

        Ok(topic.to_string())
    }

    /// Determine which loop a message is targeted at.
    ///
    /// Priority:
    /// 1. `tmid` thread match → pending question for that thread → that loop
    /// 2. `@loop-id` prefix in message text → extracted loop ID
    /// 3. Default → "main"
    fn determine_target_loop(
        &self,
        state: &RocketChatState,
        text: &str,
        tmid: Option<&str>,
    ) -> String {
        // Check thread-based routing via tmid
        if let Some(thread_id) = tmid
            && let Some(loop_id) = self.state_manager.get_loop_for_reply(state, thread_id)
        {
            return loop_id;
        }

        // Check @loop-id prefix
        if let Some(loop_id) = text.strip_prefix('@')
            && let Some(id) = loop_id.split_whitespace().next()
            && !id.is_empty()
        {
            return id.to_string();
        }

        "main".to_string()
    }

    /// Get the active events file path for a given loop.
    ///
    /// Reads the `current-events` marker to find the timestamped events file.
    /// Falls back to the default `events.jsonl` if the marker doesn't exist.
    fn get_events_path(&self, loop_id: &str) -> PathBuf {
        let ralph_dir = if loop_id == "main" {
            self.workspace_root.join(".ralph")
        } else {
            self.workspace_root
                .join(".worktrees")
                .join(loop_id)
                .join(".ralph")
        };

        let marker_path = ralph_dir.join("current-events");
        if let Ok(contents) = std::fs::read_to_string(&marker_path) {
            let relative = contents.trim();
            if !relative.is_empty() {
                if loop_id == "main" {
                    return self.workspace_root.join(relative);
                } else {
                    return self
                        .workspace_root
                        .join(".worktrees")
                        .join(loop_id)
                        .join(relative);
                }
            }
        }

        ralph_dir.join("events.jsonl")
    }

    /// Append an event line to the given file atomically.
    fn append_event(&self, path: &Path, event_line: &str) -> RocketChatResult<()> {
        use std::fs::OpenOptions;
        use std::io::Write;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                RocketChatError::EventWrite(format!(
                    "failed to create directory {}: {}",
                    parent.display(),
                    e
                ))
            })?;
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| {
                RocketChatError::EventWrite(format!("failed to open {}: {}", path.display(), e))
            })?;

        writeln!(file, "{}", event_line).map_err(|e| {
            RocketChatError::EventWrite(format!("failed to write to {}: {}", path.display(), e))
        })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::RcUser;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn make_user(id: &str) -> RcUser {
        RcUser {
            id: id.to_string(),
            username: "operator".to_string(),
            name: Some("Operator".to_string()),
        }
    }

    fn make_message(text: &str, tmid: Option<&str>) -> RcMessage {
        RcMessage {
            id: "msg-1".to_string(),
            rid: "room-1".to_string(),
            msg: text.to_string(),
            u: make_user("operator-1"),
            ts: "2026-03-08T12:00:00.000Z".to_string(),
            tmid: tmid.map(String::from),
            t: None,
        }
    }

    fn setup() -> (MessageHandler, TempDir, RocketChatState) {
        let dir = TempDir::new().unwrap();
        let state_path = dir.path().join(".ralph/rocketchat-state.json");
        let state_manager = StateManager::new(state_path);
        let handler = MessageHandler::new(state_manager, dir.path());
        let state = RocketChatState {
            last_sync: None,
            pending_questions: HashMap::new(),
        };
        (handler, dir, state)
    }

    #[test]
    fn writes_guidance_event_to_main() {
        let (handler, dir, mut state) = setup();
        let msg = make_message("don't forget logging", None);

        let topic = handler.handle_message(&mut state, &msg).unwrap();

        assert_eq!(topic, "human.guidance");
        let events_path = dir.path().join(".ralph/events.jsonl");
        let contents = std::fs::read_to_string(events_path).unwrap();
        let event: serde_json::Value = serde_json::from_str(contents.trim()).unwrap();
        assert_eq!(event["topic"], "human.guidance");
        assert_eq!(event["payload"], "don't forget logging");
    }

    #[test]
    fn writes_response_event_when_pending_question() {
        let (handler, dir, mut state) = setup();

        state.pending_questions.insert(
            "main".to_string(),
            crate::state::PendingQuestion {
                asked_at: "2026-03-08T12:00:00Z".to_string(),
                message_id: "msg-q1".to_string(),
            },
        );

        let msg = make_message("use async", Some("msg-q1"));
        let topic = handler.handle_message(&mut state, &msg).unwrap();

        assert_eq!(topic, "human.response");
        let events_path = dir.path().join(".ralph/events.jsonl");
        let contents = std::fs::read_to_string(events_path).unwrap();
        let event: serde_json::Value = serde_json::from_str(contents.trim()).unwrap();
        assert_eq!(event["topic"], "human.response");
        assert_eq!(event["payload"], "use async");

        // Pending question should be removed
        assert!(!state.pending_questions.contains_key("main"));
    }

    #[test]
    fn routes_at_prefix_to_correct_loop() {
        let (handler, dir, mut state) = setup();
        let msg = make_message("@feature-auth check edge cases", None);

        handler.handle_message(&mut state, &msg).unwrap();

        let events_path = dir
            .path()
            .join(".worktrees/feature-auth/.ralph/events.jsonl");
        let contents = std::fs::read_to_string(events_path).unwrap();
        let event: serde_json::Value = serde_json::from_str(contents.trim()).unwrap();
        assert_eq!(event["topic"], "human.guidance");
    }

    #[test]
    fn routes_tmid_thread_to_correct_loop() {
        let (handler, dir, mut state) = setup();

        state.pending_questions.insert(
            "feature-auth".to_string(),
            crate::state::PendingQuestion {
                asked_at: "2026-03-08T12:00:00Z".to_string(),
                message_id: "thread-msg-42".to_string(),
            },
        );

        let msg = make_message("yes, proceed", Some("thread-msg-42"));
        let topic = handler.handle_message(&mut state, &msg).unwrap();

        assert_eq!(topic, "human.response");
        let events_path = dir
            .path()
            .join(".worktrees/feature-auth/.ralph/events.jsonl");
        let contents = std::fs::read_to_string(events_path).unwrap();
        let event: serde_json::Value = serde_json::from_str(contents.trim()).unwrap();
        assert_eq!(event["topic"], "human.response");
    }

    #[test]
    fn tmid_takes_priority_over_at_prefix() {
        let (handler, dir, mut state) = setup();

        state.pending_questions.insert(
            "loop-a".to_string(),
            crate::state::PendingQuestion {
                asked_at: "2026-03-08T12:00:00Z".to_string(),
                message_id: "tmid-xyz".to_string(),
            },
        );

        // Message has both @loop-b prefix AND a tmid matching loop-a
        let msg = make_message("@loop-b some text", Some("tmid-xyz"));
        handler.handle_message(&mut state, &msg).unwrap();

        // Should route to loop-a (tmid wins over @prefix)
        let events_path = dir.path().join(".worktrees/loop-a/.ralph/events.jsonl");
        assert!(events_path.exists(), "tmid routing should take priority");
    }

    #[test]
    fn defaults_to_main_without_routing_hints() {
        let (handler, dir, mut state) = setup();
        let msg = make_message("just a plain message", None);

        handler.handle_message(&mut state, &msg).unwrap();

        let events_path = dir.path().join(".ralph/events.jsonl");
        assert!(events_path.exists());
    }

    #[test]
    fn writes_to_timestamped_events_file_when_marker_exists() {
        let (handler, dir, mut state) = setup();

        let ralph_dir = dir.path().join(".ralph");
        std::fs::create_dir_all(&ralph_dir).unwrap();
        std::fs::write(
            ralph_dir.join("current-events"),
            ".ralph/events-20260201-210033.jsonl",
        )
        .unwrap();

        let msg = make_message("progress update", None);
        handler.handle_message(&mut state, &msg).unwrap();

        let timestamped_path = dir.path().join(".ralph/events-20260201-210033.jsonl");
        assert!(
            timestamped_path.exists(),
            "event should be written to timestamped events file"
        );

        let contents = std::fs::read_to_string(&timestamped_path).unwrap();
        let event: serde_json::Value = serde_json::from_str(contents.trim()).unwrap();
        assert_eq!(event["topic"], "human.guidance");
        assert_eq!(event["payload"], "progress update");

        let default_path = dir.path().join(".ralph/events.jsonl");
        assert!(
            !default_path.exists(),
            "event should NOT be written to default events.jsonl when marker exists"
        );
    }
}
