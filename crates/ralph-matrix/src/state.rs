//! State persistence for the Matrix bot.
//!
//! Manages the [`MatrixState`] (sync tokens, pending questions) with
//! atomic writes (temp file + rename) via [`StateManager`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::MatrixResult;

/// Persistent state for the Matrix bot, stored at `.ralph/matrix-state.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatrixState {
    /// Sync token for incremental `/sync` requests.
    pub since_token: Option<String>,

    /// Matrix room ID the bot is operating in.
    pub room_id: Option<String>,

    /// Pending questions keyed by Matrix event ID (the bot's question message).
    #[serde(default)]
    pub pending_questions: HashMap<String, PendingQuestion>,
}

/// A question sent to the human that is awaiting a response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingQuestion {
    /// Which orchestration loop asked this question.
    pub loop_id: Option<String>,

    /// The question text that was sent.
    pub question: String,

    /// When the question was sent (ISO 8601 timestamp).
    pub asked_at: String,
}

/// Manages persistence of Matrix bot state to disk.
pub struct StateManager {
    path: PathBuf,
}

impl StateManager {
    /// Create a new StateManager that reads/writes to the given path.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Load state from disk. Returns `None` if the file doesn't exist.
    pub fn load(&self) -> MatrixResult<Option<MatrixState>> {
        if !self.path.exists() {
            return Ok(None);
        }
        let contents = std::fs::read_to_string(&self.path)?;
        let state: MatrixState = serde_json::from_str(&contents)?;
        Ok(Some(state))
    }

    /// Save state to disk using atomic write (temp file + rename).
    pub fn save(&self, state: &MatrixState) -> MatrixResult<()> {
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
    pub fn load_or_default(&self) -> MatrixResult<MatrixState> {
        Ok(self.load()?.unwrap_or_else(|| MatrixState {
            since_token: None,
            room_id: None,
            pending_questions: HashMap::new(),
        }))
    }

    /// Add a pending question keyed by the bot's Matrix event ID.
    pub fn add_pending_question(
        &self,
        state: &mut MatrixState,
        event_id: &str,
        loop_id: Option<&str>,
        question: &str,
    ) -> MatrixResult<()> {
        state.pending_questions.insert(
            event_id.to_string(),
            PendingQuestion {
                loop_id: loop_id.map(String::from),
                question: question.to_string(),
                asked_at: chrono::Utc::now().to_rfc3339(),
            },
        );
        self.save(state)
    }

    /// Remove a pending question by event ID.
    pub fn remove_pending_question(
        &self,
        state: &mut MatrixState,
        event_id: &str,
    ) -> MatrixResult<()> {
        state.pending_questions.remove(event_id);
        self.save(state)
    }

    /// Given a reply-to event ID, find which loop asked the question.
    pub fn get_loop_for_reply(
        &self,
        state: &MatrixState,
        reply_to_event_id: &str,
    ) -> Option<String> {
        state
            .pending_questions
            .get(reply_to_event_id)
            .and_then(|q| q.loop_id.clone())
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
        let path = dir.path().join("matrix-state.json");
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
        let state = MatrixState {
            since_token: Some("s123_456".to_string()),
            room_id: Some("!room:example.com".to_string()),
            pending_questions: HashMap::new(),
        };
        mgr.save(&state).unwrap();

        let loaded = mgr.load().unwrap().unwrap();
        assert_eq!(loaded.since_token, Some("s123_456".to_string()));
        assert_eq!(loaded.room_id, Some("!room:example.com".to_string()));
        assert!(loaded.pending_questions.is_empty());
    }

    #[test]
    fn load_or_default_returns_empty_state() {
        let (mgr, _dir) = test_manager();
        let state = mgr.load_or_default().unwrap();
        assert!(state.since_token.is_none());
        assert!(state.room_id.is_none());
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

        mgr.add_pending_question(
            &mut state,
            "$ev1:example.com",
            Some("main"),
            "Should I proceed?",
        )
        .unwrap();

        assert!(state.pending_questions.contains_key("$ev1:example.com"));
        let q = &state.pending_questions["$ev1:example.com"];
        assert_eq!(q.loop_id, Some("main".to_string()));
        assert_eq!(q.question, "Should I proceed?");

        mgr.remove_pending_question(&mut state, "$ev1:example.com")
            .unwrap();
        assert!(!state.pending_questions.contains_key("$ev1:example.com"));
    }

    #[test]
    fn reply_routing_lookup() {
        let (mgr, _dir) = test_manager();
        let mut state = mgr.load_or_default().unwrap();

        mgr.add_pending_question(&mut state, "$ev1:example.com", Some("main"), "Question 1?")
            .unwrap();
        mgr.add_pending_question(
            &mut state,
            "$ev2:example.com",
            Some("feature-auth"),
            "Question 2?",
        )
        .unwrap();

        assert_eq!(
            mgr.get_loop_for_reply(&state, "$ev1:example.com"),
            Some("main".to_string())
        );
        assert_eq!(
            mgr.get_loop_for_reply(&state, "$ev2:example.com"),
            Some("feature-auth".to_string())
        );
        assert_eq!(mgr.get_loop_for_reply(&state, "$ev999:example.com"), None);
    }

    #[test]
    fn pending_question_without_loop_id() {
        let (mgr, _dir) = test_manager();
        let mut state = mgr.load_or_default().unwrap();

        mgr.add_pending_question(&mut state, "$ev1:example.com", None, "General question?")
            .unwrap();

        let q = &state.pending_questions["$ev1:example.com"];
        assert_eq!(q.loop_id, None);
        assert_eq!(mgr.get_loop_for_reply(&state, "$ev1:example.com"), None);
    }
}
