//! Incoming message handler for Matrix.
//!
//! Routes incoming messages to the correct loop's `events.jsonl` file as
//! `human.response` or `human.guidance` events, using reply-to event ID,
//! `@loop-id` prefix, or default-to-main routing.

use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::error::{MatrixError, MatrixResult};
use crate::state::{MatrixState, StateManager};
use crate::types::MatrixMessage;

/// Result of handling a message.
#[derive(Debug, PartialEq)]
pub enum HandleResult {
    /// An event was written to events.jsonl (topic name).
    Event(String),
    /// A bot command was recognized; the response should be sent back to the room.
    CommandResponse(String),
}

/// Processes incoming Matrix messages and writes events to the correct loop's events.jsonl.
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

    /// Handle an incoming message from Matrix.
    ///
    /// Checks for bot commands first (messages starting with `!`). If a command
    /// is recognized, returns [`HandleResult::CommandResponse`] with the response
    /// text to send back. Otherwise, determines the target loop, classifies as
    /// response or guidance, and appends the appropriate event to events.jsonl.
    pub fn handle_message(
        &self,
        state: &mut MatrixState,
        message: &MatrixMessage,
    ) -> MatrixResult<HandleResult> {
        // Check for bot commands before routing as guidance
        let text = message.body.trim();
        if crate::commands::is_command(text)
            && let Some(response) = crate::commands::handle_command(text, &self.workspace_root)
        {
            return Ok(HandleResult::CommandResponse(response));
        }

        let target_loop =
            self.determine_target_loop(state, message.reply_to_event_id.as_deref(), &message.body);
        let events_path = self.get_events_path(&target_loop);

        // Find a pending question whose loop_id matches target_loop
        let pending_event_id = state
            .pending_questions
            .iter()
            .find(|(_, q)| q.loop_id.as_deref() == Some(&target_loop))
            .map(|(event_id, _)| event_id.clone());

        let is_response = pending_event_id.is_some();

        let topic = if is_response {
            "human.response"
        } else {
            "human.guidance"
        };

        let timestamp = Utc::now().to_rfc3339();
        let event_json = serde_json::json!({
            "topic": topic,
            "payload": message.body,
            "ts": timestamp,
        });
        let event_line = serde_json::to_string(&event_json)?;

        self.append_event(&events_path, &event_line)?;

        if let Some(event_id) = pending_event_id {
            self.state_manager
                .remove_pending_question(state, &event_id)?;
        }

        tracing::info!(
            topic,
            target_loop,
            "wrote {} event for loop {}",
            topic,
            target_loop
        );

        Ok(HandleResult::Event(topic.to_string()))
    }

    /// Determine which loop a message is targeted at.
    ///
    /// Priority:
    /// 1. `reply_to_event_id` match → pending question for that event → that loop
    /// 2. `@loop-id` prefix in message text → extracted loop ID
    /// 3. Default → "main"
    fn determine_target_loop(
        &self,
        state: &MatrixState,
        reply_to_event_id: Option<&str>,
        text: &str,
    ) -> String {
        // Check reply-to-based routing via event ID
        if let Some(event_id) = reply_to_event_id
            && let Some(loop_id) = self.state_manager.get_loop_for_reply(state, event_id)
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
    fn append_event(&self, path: &Path, event_line: &str) -> MatrixResult<()> {
        use std::fs::OpenOptions;
        use std::io::Write;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                MatrixError::EventWrite(format!(
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
                MatrixError::EventWrite(format!("failed to open {}: {}", path.display(), e))
            })?;

        writeln!(file, "{}", event_line).map_err(|e| {
            MatrixError::EventWrite(format!("failed to write to {}: {}", path.display(), e))
        })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn make_message(text: &str, reply_to: Option<&str>) -> MatrixMessage {
        MatrixMessage {
            sender_id: "@operator:example.com".to_string(),
            body: text.to_string(),
            event_id: "$msg-1".to_string(),
            timestamp: 1_700_000_000_000,
            room_id: "!room:example.com".to_string(),
            reply_to_event_id: reply_to.map(String::from),
        }
    }

    fn setup() -> (MessageHandler, TempDir, MatrixState) {
        let dir = TempDir::new().unwrap();
        let state_path = dir.path().join(".ralph/matrix-state.json");
        let state_manager = StateManager::new(state_path);
        let handler = MessageHandler::new(state_manager, dir.path());
        let state = MatrixState {
            since_token: None,
            room_id: None,
            pending_questions: HashMap::new(),
        };
        (handler, dir, state)
    }

    #[test]
    fn writes_guidance_event_to_main() {
        let (handler, dir, mut state) = setup();
        let msg = make_message("don't forget logging", None);

        let result = handler.handle_message(&mut state, &msg).unwrap();

        assert_eq!(result, HandleResult::Event("human.guidance".to_string()));
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
            "$q-evt-1".to_string(),
            crate::state::PendingQuestion {
                loop_id: Some("main".to_string()),
                question: "Should I proceed?".to_string(),
                asked_at: "2026-03-08T12:00:00Z".to_string(),
            },
        );

        let msg = make_message("use async", Some("$q-evt-1"));
        let result = handler.handle_message(&mut state, &msg).unwrap();

        assert_eq!(result, HandleResult::Event("human.response".to_string()));
        let events_path = dir.path().join(".ralph/events.jsonl");
        let contents = std::fs::read_to_string(events_path).unwrap();
        let event: serde_json::Value = serde_json::from_str(contents.trim()).unwrap();
        assert_eq!(event["topic"], "human.response");
        assert_eq!(event["payload"], "use async");

        // Pending question should be removed
        assert!(!state.pending_questions.contains_key("$q-evt-1"));
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
    fn routes_reply_to_event_to_correct_loop() {
        let (handler, dir, mut state) = setup();

        state.pending_questions.insert(
            "$q-evt-42".to_string(),
            crate::state::PendingQuestion {
                loop_id: Some("feature-auth".to_string()),
                question: "Which auth provider?".to_string(),
                asked_at: "2026-03-08T12:00:00Z".to_string(),
            },
        );

        let msg = make_message("yes, proceed", Some("$q-evt-42"));
        let result = handler.handle_message(&mut state, &msg).unwrap();

        assert_eq!(result, HandleResult::Event("human.response".to_string()));
        let events_path = dir
            .path()
            .join(".worktrees/feature-auth/.ralph/events.jsonl");
        let contents = std::fs::read_to_string(events_path).unwrap();
        let event: serde_json::Value = serde_json::from_str(contents.trim()).unwrap();
        assert_eq!(event["topic"], "human.response");
    }

    #[test]
    fn reply_to_takes_priority_over_at_prefix() {
        let (handler, dir, mut state) = setup();

        state.pending_questions.insert(
            "$q-evt-xyz".to_string(),
            crate::state::PendingQuestion {
                loop_id: Some("loop-a".to_string()),
                question: "Priority test?".to_string(),
                asked_at: "2026-03-08T12:00:00Z".to_string(),
            },
        );

        // Message has both @loop-b prefix AND a reply_to matching loop-a
        let msg = make_message("@loop-b some text", Some("$q-evt-xyz"));
        handler.handle_message(&mut state, &msg).unwrap();

        // Should route to loop-a (reply_to wins over @prefix)
        let events_path = dir.path().join(".worktrees/loop-a/.ralph/events.jsonl");
        assert!(
            events_path.exists(),
            "reply_to routing should take priority"
        );
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

    #[test]
    fn command_returns_command_response() {
        let (handler, _dir, mut state) = setup();
        let msg = make_message("!help", None);

        let result = handler.handle_message(&mut state, &msg).unwrap();

        match result {
            HandleResult::CommandResponse(response) => {
                assert!(response.contains("**Ralph Bot Commands**"));
                assert!(response.contains("!status"));
            }
            HandleResult::Event(_) => panic!("Expected CommandResponse, got Event"),
        }
    }

    #[test]
    fn unknown_command_falls_through_as_guidance() {
        let (handler, dir, mut state) = setup();
        let msg = make_message("!nonexistent", None);

        let result = handler.handle_message(&mut state, &msg).unwrap();

        assert_eq!(result, HandleResult::Event("human.guidance".to_string()));
        let events_path = dir.path().join(".ralph/events.jsonl");
        assert!(events_path.exists());
    }
}
