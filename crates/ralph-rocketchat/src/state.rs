//! State persistence for the Rocket.Chat bot.
//!
//! Manages the [`RocketChatState`] (sync timestamps, pending questions) with
//! atomic writes (temp file + rename) via [`StateManager`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::RocketChatResult;

/// Persistent state for the Rocket.Chat bot, stored at `.ralph/rocketchat-state.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RocketChatState {
    /// ISO timestamp for `chat.syncMessages` `lastUpdate` parameter.
    pub last_sync: Option<String>,

    /// Pending questions keyed by loop ID, tracking which message awaits a reply.
    #[serde(default)]
    pub pending_questions: HashMap<String, PendingQuestion>,
}

/// A question sent to the human that is awaiting a response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingQuestion {
    /// When the question was sent (ISO 8601 timestamp).
    pub asked_at: String,

    /// The Rocket.Chat message `_id`, used to match thread-based reply routing.
    pub message_id: String,
}

/// Manages persistence of Rocket.Chat bot state to disk.
pub struct StateManager {
    path: PathBuf,
}

impl StateManager {
    /// Create a new StateManager that reads/writes to the given path.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Load state from disk. Returns `None` if the file doesn't exist.
    pub fn load(&self) -> RocketChatResult<Option<RocketChatState>> {
        if !self.path.exists() {
            return Ok(None);
        }
        let contents = std::fs::read_to_string(&self.path)?;
        let state: RocketChatState = serde_json::from_str(&contents)?;
        Ok(Some(state))
    }

    /// Save state to disk using atomic write (temp file + rename).
    pub fn save(&self, state: &RocketChatState) -> RocketChatResult<()> {
        let json = serde_json::to_string_pretty(state)?;
        let tmp_path = self.path.with_extension("json.tmp");

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::write(&tmp_path, &json)?;
        std::fs::rename(&tmp_path, &self.path)?;
        Ok(())
    }

    /// Load existing state or create a fresh empty state.
    pub fn load_or_default(&self) -> RocketChatResult<RocketChatState> {
        Ok(self.load()?.unwrap_or_else(|| RocketChatState {
            last_sync: None,
            pending_questions: HashMap::new(),
        }))
    }

    /// Add a pending question for a given loop.
    pub fn add_pending_question(
        &self,
        state: &mut RocketChatState,
        loop_id: &str,
        message_id: &str,
    ) -> RocketChatResult<()> {
        state.pending_questions.insert(
            loop_id.to_string(),
            PendingQuestion {
                asked_at: chrono::Utc::now().to_rfc3339(),
                message_id: message_id.to_string(),
            },
        );
        self.save(state)
    }

    /// Remove a pending question for a given loop.
    pub fn remove_pending_question(
        &self,
        state: &mut RocketChatState,
        loop_id: &str,
    ) -> RocketChatResult<()> {
        state.pending_questions.remove(loop_id);
        self.save(state)
    }

    /// Given a thread message ID (`tmid`), find which loop it belongs to.
    pub fn get_loop_for_reply(
        &self,
        state: &RocketChatState,
        reply_message_id: &str,
    ) -> Option<String> {
        state
            .pending_questions
            .iter()
            .find(|(_, q)| q.message_id == reply_message_id)
            .map(|(loop_id, _)| loop_id.clone())
    }

    /// Return the path to the state file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_manager() -> (StateManager, TempDir) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("rocketchat-state.json");
        (StateManager::new(path), dir)
    }

    #[test]
    fn load_missing_file_returns_none() {
        let (mgr, _dir) = test_manager();
        assert!(mgr.load().unwrap().is_none());
    }

    #[test]
    fn save_and_load_round_trip() {
        let (mgr, _dir) = test_manager();
        let state = RocketChatState {
            last_sync: Some("2026-03-08T12:00:00.000Z".to_string()),
            pending_questions: HashMap::new(),
        };
        mgr.save(&state).unwrap();

        let loaded = mgr.load().unwrap().unwrap();
        assert_eq!(
            loaded.last_sync,
            Some("2026-03-08T12:00:00.000Z".to_string())
        );
        assert!(loaded.pending_questions.is_empty());
    }

    #[test]
    fn load_or_default_returns_empty_state() {
        let (mgr, _dir) = test_manager();
        let state = mgr.load_or_default().unwrap();
        assert!(state.last_sync.is_none());
        assert!(state.pending_questions.is_empty());
    }

    #[test]
    fn corrupted_json_returns_error() {
        let (mgr, _dir) = test_manager();
        std::fs::write(mgr.path(), "not json").unwrap();
        assert!(mgr.load().is_err());
    }

    #[test]
    fn pending_question_tracking() {
        let (mgr, _dir) = test_manager();
        let mut state = mgr.load_or_default().unwrap();

        mgr.add_pending_question(&mut state, "main", "msg-abc123")
            .unwrap();
        assert!(state.pending_questions.contains_key("main"));
        assert_eq!(state.pending_questions["main"].message_id, "msg-abc123");

        mgr.remove_pending_question(&mut state, "main").unwrap();
        assert!(!state.pending_questions.contains_key("main"));
    }

    #[test]
    fn reply_routing_lookup() {
        let (mgr, _dir) = test_manager();
        let mut state = mgr.load_or_default().unwrap();

        mgr.add_pending_question(&mut state, "main", "msg-001")
            .unwrap();
        mgr.add_pending_question(&mut state, "feature-auth", "msg-002")
            .unwrap();

        assert_eq!(
            mgr.get_loop_for_reply(&state, "msg-001"),
            Some("main".to_string())
        );
        assert_eq!(
            mgr.get_loop_for_reply(&state, "msg-002"),
            Some("feature-auth".to_string())
        );
        assert_eq!(mgr.get_loop_for_reply(&state, "msg-999"), None);
    }
}
