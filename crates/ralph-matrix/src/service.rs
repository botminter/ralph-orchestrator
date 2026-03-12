//! Matrix RObot service implementing `RobotService`.
//!
//! Wraps a [`MatrixApi`] client and coordinates message sending, response
//! polling, and background sync. Follows the same architecture as
//! `ralph-rocketchat`'s `RocketChatService`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32};
use std::time::{Duration, Instant};

use std::sync::atomic::Ordering;

use tracing::{debug, info, warn};

use crate::client::MatrixApi;
use crate::error::{MatrixError, MatrixResult};
use crate::handler::MessageHandler;
use crate::state::StateManager;

/// Maximum number of retry attempts for sending messages.
pub const MAX_SEND_RETRIES: u32 = 3;

/// Base delay for exponential backoff (1 second).
pub const BASE_RETRY_DELAY: Duration = Duration::from_secs(1);

/// Execute a fallible send operation with exponential backoff retry.
///
/// Retries up to [`MAX_SEND_RETRIES`] times with delays of 1s, 2s, 4s.
/// Returns the result on success, or `MatrixError::SendFailed` after all
/// retries are exhausted.
///
/// The `sleep_fn` parameter allows tests to substitute a no-op sleep.
pub fn retry_with_backoff<F, S>(mut send_fn: F, mut sleep_fn: S) -> MatrixResult<String>
where
    F: FnMut(u32) -> MatrixResult<String>,
    S: FnMut(Duration),
{
    let mut last_error = String::new();

    for attempt in 1..=MAX_SEND_RETRIES {
        match send_fn(attempt) {
            Ok(event_id) => return Ok(event_id),
            Err(e) => {
                last_error = e.to_string();
                warn!(
                    attempt = attempt,
                    max_retries = MAX_SEND_RETRIES,
                    error = %last_error,
                    "Matrix send failed, {}",
                    if attempt < MAX_SEND_RETRIES {
                        "retrying with backoff"
                    } else {
                        "all retries exhausted"
                    }
                );

                if attempt < MAX_SEND_RETRIES {
                    let delay = BASE_RETRY_DELAY * 2u32.pow(attempt - 1);
                    sleep_fn(delay);
                }
            }
        }
    }

    Err(MatrixError::SendFailed(format!(
        "failed after {} attempts: {}",
        MAX_SEND_RETRIES, last_error
    )))
}

/// Matrix-backed human-in-the-loop service.
///
/// Manages question/response lifecycle, background message polling, and
/// check-in notifications via the Matrix protocol.
pub struct MatrixService {
    workspace_root: PathBuf,
    room_id: String,
    operator_id: Option<String>,
    timeout: u64,
    loop_id: String,
    state_manager: StateManager,
    #[allow(dead_code)]
    handler: MessageHandler,
    client: Arc<dyn MatrixApi>,
    shutdown: Arc<AtomicBool>,
    message_counter: Arc<AtomicI32>,
}

impl MatrixService {
    /// Create a new MatrixService.
    ///
    /// All authentication parameters must be pre-resolved by the config layer.
    /// The `operator_id` filters messages in group chats to only process the
    /// designated human operator; when `None`, all non-bot messages are
    /// processed.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        workspace_root: PathBuf,
        room_id: String,
        operator_id: Option<String>,
        timeout: u64,
        loop_id: String,
        client: Arc<dyn MatrixApi>,
    ) -> Self {
        let state_path = workspace_root.join(".ralph/matrix-state.json");
        let state_manager = StateManager::new(&state_path);
        let handler_state_manager = StateManager::new(&state_path);
        let handler = MessageHandler::new(handler_state_manager, &workspace_root);
        let shutdown = Arc::new(AtomicBool::new(false));
        let message_counter = Arc::new(AtomicI32::new(0));

        Self {
            workspace_root,
            room_id,
            operator_id,
            timeout,
            loop_id,
            state_manager,
            handler,
            client,
            shutdown,
            message_counter,
        }
    }

    /// Get a reference to the workspace root.
    pub fn workspace_root(&self) -> &PathBuf {
        &self.workspace_root
    }

    /// Get the configured timeout in seconds.
    pub fn timeout_secs(&self) -> u64 {
        self.timeout
    }

    /// Get the loop ID this service is associated with.
    pub fn loop_id(&self) -> &str {
        &self.loop_id
    }

    /// Get the room ID this service is sending to.
    pub fn room_id(&self) -> &str {
        &self.room_id
    }

    /// Get the operator ID filter, if set.
    pub fn operator_id(&self) -> Option<&str> {
        self.operator_id.as_deref()
    }

    /// Get a reference to the state manager.
    pub fn state_manager(&self) -> &StateManager {
        &self.state_manager
    }

    /// Returns a clone of the shutdown flag.
    ///
    /// Signal handlers can set this flag to interrupt `wait_for_response()`
    /// without waiting for the full timeout.
    pub fn shutdown_flag(&self) -> Arc<AtomicBool> {
        self.shutdown.clone()
    }

    /// Send a message to the configured Matrix room with retry/backoff.
    ///
    /// Uses the host tokio runtime via `block_in_place` + `Handle::block_on`
    /// to bridge the sync event loop to the async Matrix API.
    ///
    /// Returns the server-assigned event ID string on success.
    fn send_with_retry(&self, payload: &str) -> MatrixResult<String> {
        let handle = tokio::runtime::Handle::try_current().map_err(|_| {
            MatrixError::SendFailed("no tokio runtime available for sending".to_string())
        })?;

        let room_id = self.room_id.clone();
        retry_with_backoff(
            |_attempt| {
                tokio::task::block_in_place(|| {
                    handle.block_on(self.client.send_message(&room_id, payload, None))
                })
            },
            |delay| std::thread::sleep(delay),
        )
    }

    /// Send a periodic check-in status message to the Matrix room.
    ///
    /// Formats a human-readable status with iteration count, elapsed time, and
    /// optional context (current hat, task counts, cumulative cost). Skips
    /// silently when no room ID is configured.
    pub fn send_checkin(
        &self,
        iteration: u32,
        elapsed: Duration,
        context: Option<&ralph_proto::CheckinContext>,
    ) -> MatrixResult<i32> {
        if self.room_id.is_empty() {
            debug!(
                loop_id = %self.loop_id,
                "No room ID configured — skipping check-in"
            );
            return Ok(0);
        }

        let elapsed_secs = elapsed.as_secs();
        let minutes = elapsed_secs / 60;
        let seconds = elapsed_secs % 60;
        let elapsed_str = if minutes > 0 {
            format!("{}m {}s", minutes, seconds)
        } else {
            format!("{}s", seconds)
        };

        let msg = match context {
            Some(ctx) => {
                let mut lines = vec![format!(
                    "Still working — iteration **{}**, `{}` elapsed.",
                    iteration, elapsed_str
                )];

                if let Some(hat) = &ctx.current_hat {
                    lines.push(format!("Hat: `{}`", hat));
                }

                if ctx.open_tasks > 0 || ctx.closed_tasks > 0 {
                    lines.push(format!(
                        "Tasks: **{}** open, {} closed",
                        ctx.open_tasks, ctx.closed_tasks
                    ));
                }

                if ctx.cumulative_cost > 0.0 {
                    lines.push(format!("Cost: `${:.4}`", ctx.cumulative_cost));
                }

                lines.join("\n")
            }
            None => format!(
                "Still working — iteration **{}**, `{}` elapsed.",
                iteration, elapsed_str
            ),
        };

        self.send_with_retry(&msg)?;
        Ok(0)
    }

    /// Send a question to the human operator via Matrix.
    ///
    /// The question payload is extracted from the `human.interact` event. A pending
    /// question is stored in state keyed by the returned event ID so that incoming
    /// replies can be routed back to the correct loop.
    ///
    /// Returns a monotonically incrementing `i32` counter (the trait requires `i32`
    /// but Matrix uses string event IDs internally, stored in state).
    ///
    /// Skips silently if no room ID is configured.
    pub fn send_question(&self, payload: &str) -> MatrixResult<i32> {
        let mut state = self.state_manager.load_or_default()?;

        let event_id = if self.room_id.is_empty() {
            warn!(
                loop_id = %self.loop_id,
                "No room ID configured — human.interact question logged but not sent: {}",
                payload
            );
            String::new()
        } else {
            self.send_with_retry(payload)?
        };

        self.state_manager.add_pending_question(
            &mut state,
            &event_id,
            Some(&self.loop_id),
            payload,
        )?;

        let counter = self.message_counter.fetch_add(1, Ordering::SeqCst) + 1;

        debug!(
            loop_id = %self.loop_id,
            event_id = %event_id,
            counter = counter,
            "Stored pending question"
        );

        Ok(counter)
    }

    /// Check the events file for a `human.response` event starting from
    /// `file_pos`. Updates `file_pos` to the new end of file.
    ///
    /// Supports two event formats:
    /// - JSON: `{"topic":"human.response","payload":"..."}`
    /// - Pipe-separated: `... EVENT: human.response | message: "..."`
    pub fn check_for_response(
        events_path: &Path,
        file_pos: &mut u64,
    ) -> MatrixResult<Option<String>> {
        use std::io::{BufRead, BufReader, Seek, SeekFrom};

        if !events_path.exists() {
            return Ok(None);
        }

        let mut file = std::fs::File::open(events_path)?;
        file.seek(SeekFrom::Start(*file_pos))?;

        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line?;
            let line_bytes = line.len() as u64 + 1; // +1 for newline
            *file_pos += line_bytes;

            if line.trim().is_empty() {
                continue;
            }

            // Try JSON event format
            if let Ok(event) = serde_json::from_str::<serde_json::Value>(&line)
                && event.get("topic").and_then(|t| t.as_str()) == Some("human.response")
            {
                let message = event
                    .get("payload")
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string();
                return Ok(Some(message));
            }

            // Pipe-separated format (written by MessageHandler)
            if line.contains("EVENT: human.response") {
                let message = line
                    .split('|')
                    .find(|part| part.trim().starts_with("message:"))
                    .and_then(|part| {
                        let value = part.trim().strip_prefix("message:")?;
                        let trimmed = value.trim().trim_matches('"');
                        Some(trimmed.to_string())
                    })
                    .unwrap_or_default();
                return Ok(Some(message));
            }
        }

        Ok(None)
    }

    /// Block until a `human.response` event appears or timeout/shutdown.
    ///
    /// Polls the events file every 250ms for new `human.response` events
    /// written by the background message handler. On response, timeout, or
    /// shutdown, removes the pending question for this loop from state.
    ///
    /// Returns `Some(response)` on success, `None` on timeout or shutdown.
    pub fn wait_for_response(&self, events_path: &Path) -> MatrixResult<Option<String>> {
        let timeout = Duration::from_secs(self.timeout);
        let poll_interval = Duration::from_millis(250);
        let deadline = Instant::now() + timeout;

        let initial_pos = if events_path.exists() {
            std::fs::metadata(events_path).map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };
        let mut file_pos = initial_pos;

        info!(
            loop_id = %self.loop_id,
            timeout_secs = self.timeout,
            events_path = %events_path.display(),
            "Waiting for human.response"
        );

        loop {
            if Instant::now() >= deadline {
                warn!(
                    loop_id = %self.loop_id,
                    timeout_secs = self.timeout,
                    "Timed out waiting for human.response"
                );
                self.remove_pending_for_loop();
                return Ok(None);
            }

            if self.shutdown.load(Ordering::Relaxed) {
                info!(loop_id = %self.loop_id, "Interrupted while waiting for human.response");
                self.remove_pending_for_loop();
                return Ok(None);
            }

            if let Some(response) = Self::check_for_response(events_path, &mut file_pos)? {
                info!(
                    loop_id = %self.loop_id,
                    "Received human.response: {}",
                    response
                );
                self.remove_pending_for_loop();
                return Ok(Some(response));
            }

            std::thread::sleep(poll_interval);
        }
    }

    /// Start the Matrix service: spawn background message poller and send greeting.
    ///
    /// Acquires the current tokio runtime handle and spawns [`poll_messages`] as
    /// an async background task. Also sends a one-off greeting message to the
    /// configured room.
    pub fn start(&self) -> MatrixResult<()> {
        info!(
            room_id = %self.room_id,
            workspace = %self.workspace_root.display(),
            timeout_secs = self.timeout,
            "Matrix service starting"
        );

        let handle = tokio::runtime::Handle::try_current().map_err(|_| {
            MatrixError::Startup("no tokio runtime available for polling".to_string())
        })?;

        let workspace_root = self.workspace_root.clone();
        let state_path = self.workspace_root.join(".ralph/matrix-state.json");
        let room_id = self.room_id.clone();
        let operator_id = self.operator_id.clone();
        let shutdown = self.shutdown.clone();
        let loop_id = self.loop_id.clone();
        let client = Arc::clone(&self.client);

        handle.spawn(async move {
            Self::poll_messages(
                client,
                workspace_root,
                state_path,
                room_id,
                operator_id,
                shutdown,
                loop_id,
            )
            .await;
        });

        // Send greeting if we have a room_id configured
        if !self.room_id.is_empty() {
            let greeting = format!("🤖 Ralph loop `{}` connected via Matrix", self.loop_id);
            let room_id = self.room_id.clone();
            let client = Arc::clone(&self.client);

            let handle = tokio::runtime::Handle::try_current().map_err(|_| {
                MatrixError::Startup("no tokio runtime available for greeting".to_string())
            })?;
            handle.spawn(async move {
                match client.send_message(&room_id, &greeting, None).await {
                    Ok(_) => info!("Sent greeting to room {}", room_id),
                    Err(e) => warn!(error = %e, "Failed to send greeting"),
                }
            });
        }

        info!("Matrix service started — polling for incoming messages");
        Ok(())
    }

    /// Background polling task that receives incoming Matrix messages.
    ///
    /// Uses `sync_once` to long-poll the homeserver, filters messages by
    /// `operator_id`, and delegates to [`MessageHandler`] to write events
    /// to the correct loop's JSONL file.
    async fn poll_messages(
        client: Arc<dyn MatrixApi>,
        workspace_root: PathBuf,
        state_path: PathBuf,
        room_id: String,
        operator_id: Option<String>,
        shutdown: Arc<AtomicBool>,
        loop_id: String,
    ) {
        use crate::types::filter_by_operator;

        let state_manager = StateManager::new(&state_path);
        let handler_state_manager = StateManager::new(&state_path);
        let handler = MessageHandler::new(handler_state_manager, &workspace_root);

        info!(loop_id = %loop_id, "Matrix polling task started");

        while !shutdown.load(Ordering::Relaxed) {
            let mut state = match state_manager.load_or_default() {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "Failed to load Matrix state");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };

            match client.sync_once(Duration::from_secs(30)).await {
                Ok(sync_result) => {
                    // Filter to only messages in our room
                    let room_messages: Vec<_> = sync_result
                        .messages
                        .into_iter()
                        .filter(|msg| msg.room_id == room_id)
                        .collect();

                    let messages = if let Some(ref op_id) = operator_id {
                        filter_by_operator(&room_messages, op_id)
                    } else {
                        room_messages
                    };

                    for msg in &messages {
                        debug!(
                            event_id = %msg.event_id,
                            sender = %msg.sender_id,
                            text = %msg.body,
                            "Processing Matrix message"
                        );

                        match handler.handle_message(&mut state, msg) {
                            Ok(crate::handler::HandleResult::Event(topic)) => {
                                info!(
                                    topic = %topic,
                                    event_id = %msg.event_id,
                                    "Routed Matrix message as {topic}"
                                );
                            }
                            Ok(crate::handler::HandleResult::CommandResponse(response)) => {
                                info!(
                                    event_id = %msg.event_id,
                                    "Handled bot command"
                                );
                                if let Err(e) = client.send_message(&room_id, &response, None).await
                                {
                                    warn!(error = %e, "Failed to send command response");
                                }
                            }
                            Err(e) => {
                                warn!(
                                    error = %e,
                                    event_id = %msg.event_id,
                                    "Failed to handle Matrix message"
                                );
                            }
                        }
                    }

                    // Persist updated since_token (would be set by sync_once in
                    // production; for now save state to capture pending_question
                    // changes from handler).
                    if let Err(e) = state_manager.save(&state) {
                        warn!(error = %e, "Failed to persist Matrix state");
                    }
                }
                Err(e) => {
                    if !shutdown.load(Ordering::Relaxed) {
                        warn!(error = %e, "Matrix polling error — retrying in 5s");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                }
            }

            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        info!(loop_id = %loop_id, "Matrix polling task stopped");
    }

    /// Stop the Matrix service: send farewell and set the shutdown flag.
    ///
    /// Sends a farewell message to the configured room (if non-empty) and
    /// signals the background polling task to terminate via the shutdown flag.
    /// Takes `self` by value to prevent further use after shutdown.
    pub fn stop(self) {
        if !self.room_id.is_empty() {
            let farewell = format!("👋 Ralph loop `{}` disconnecting.", self.loop_id);
            match self.send_with_retry(&farewell) {
                Ok(_) => info!("Sent farewell to room {}", self.room_id),
                Err(e) => warn!(error = %e, "Failed to send farewell"),
            }
        }

        self.shutdown.store(true, Ordering::Relaxed);
        info!(
            workspace = %self.workspace_root.display(),
            "Matrix service stopped"
        );
    }
}

impl ralph_proto::RobotService for MatrixService {
    fn send_question(&self, payload: &str) -> anyhow::Result<i32> {
        Ok(MatrixService::send_question(self, payload)?)
    }

    fn wait_for_response(&self, events_path: &Path) -> anyhow::Result<Option<String>> {
        Ok(MatrixService::wait_for_response(self, events_path)?)
    }

    fn send_checkin(
        &self,
        iteration: u32,
        elapsed: Duration,
        context: Option<&ralph_proto::CheckinContext>,
    ) -> anyhow::Result<i32> {
        Ok(MatrixService::send_checkin(
            self, iteration, elapsed, context,
        )?)
    }

    fn timeout_secs(&self) -> u64 {
        self.timeout
    }

    fn shutdown_flag(&self) -> Arc<AtomicBool> {
        self.shutdown.clone()
    }

    fn stop(self: Box<Self>) {
        MatrixService::stop(*self);
    }
}

impl std::fmt::Debug for MatrixService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MatrixService")
            .field("workspace_root", &self.workspace_root)
            .field("room_id", &self.room_id)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl MatrixService {
    /// Find and remove the pending question for this loop_id from state.
    ///
    /// Matrix pending questions are keyed by event ID, so we iterate to find
    /// the entry matching our loop_id.
    fn remove_pending_for_loop(&self) {
        if let Ok(mut state) = self.state_manager.load_or_default() {
            let event_id = state
                .pending_questions
                .iter()
                .find(|(_, q)| q.loop_id.as_deref() == Some(&self.loop_id))
                .map(|(id, _)| id.clone());

            if let Some(event_id) = event_id {
                let _ = self
                    .state_manager
                    .remove_pending_question(&mut state, &event_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{MockCall, MockMatrixClient};
    use crate::error::MatrixError;
    use tempfile::TempDir;

    fn make_service(dir: &std::path::Path) -> MatrixService {
        let mock = MockMatrixClient::new();
        MatrixService::new(
            dir.to_path_buf(),
            "!room:example.com".to_string(),
            Some("@operator:example.com".to_string()),
            300,
            "main".to_string(),
            Arc::new(mock),
        )
    }

    #[test]
    fn new_creates_service_with_correct_fields() {
        let dir = TempDir::new().unwrap();
        let svc = make_service(dir.path());

        assert_eq!(svc.workspace_root(), dir.path());
        assert_eq!(svc.room_id(), "!room:example.com");
        assert_eq!(svc.operator_id(), Some("@operator:example.com"));
        assert_eq!(svc.timeout_secs(), 300);
        assert_eq!(svc.loop_id(), "main");
    }

    #[test]
    fn shutdown_flag_initially_false() {
        let dir = TempDir::new().unwrap();
        let svc = make_service(dir.path());

        let flag = svc.shutdown_flag();
        assert!(!flag.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn operator_id_none_when_not_set() {
        let dir = TempDir::new().unwrap();
        let mock = MockMatrixClient::new();
        let svc = MatrixService::new(
            dir.path().to_path_buf(),
            "!room:example.com".to_string(),
            None,
            120,
            "worktree-1".to_string(),
            Arc::new(mock),
        );

        assert_eq!(svc.operator_id(), None);
        assert_eq!(svc.loop_id(), "worktree-1");
        assert_eq!(svc.timeout_secs(), 120);
    }

    #[test]
    fn state_manager_points_to_correct_path() {
        let dir = TempDir::new().unwrap();
        let svc = make_service(dir.path());

        let expected_path = dir.path().join(".ralph/matrix-state.json");
        assert_eq!(svc.state_manager().path(), expected_path);
    }

    #[test]
    fn shutdown_flag_is_shared() {
        let dir = TempDir::new().unwrap();
        let svc = make_service(dir.path());

        let flag1 = svc.shutdown_flag();
        let flag2 = svc.shutdown_flag();

        flag1.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(flag2.load(std::sync::atomic::Ordering::SeqCst));
    }

    fn make_service_with_client(dir: &std::path::Path, client: MockMatrixClient) -> MatrixService {
        MatrixService::new(
            dir.to_path_buf(),
            "!room:example.com".to_string(),
            Some("@operator:example.com".to_string()),
            300,
            "main".to_string(),
            Arc::new(client),
        )
    }

    fn make_service_empty_room(dir: &std::path::Path) -> MatrixService {
        let mock = MockMatrixClient::new();
        MatrixService::new(
            dir.to_path_buf(),
            String::new(),
            None,
            300,
            "main".to_string(),
            Arc::new(mock),
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn send_question_sends_via_mock_and_stores_pending() {
        let dir = TempDir::new().unwrap();
        let mock = MockMatrixClient::new();
        mock.on_send_message(Ok("$evt1:example.com".to_string()));
        let svc = make_service_with_client(dir.path(), mock);

        let counter = svc.send_question("What should I do?").unwrap();
        assert_eq!(counter, 1);

        // Verify pending question was stored keyed by event ID
        let state = svc.state_manager().load_or_default().unwrap();
        let pending = state.pending_questions.get("$evt1:example.com");
        assert!(pending.is_some());
        let pq = pending.unwrap();
        assert_eq!(pq.loop_id.as_deref(), Some("main"));
        assert_eq!(pq.question, "What should I do?");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn send_question_increments_counter() {
        let dir = TempDir::new().unwrap();
        let mock = MockMatrixClient::new();
        mock.on_send_message(Ok("$evt1:example.com".to_string()));
        mock.on_send_message(Ok("$evt2:example.com".to_string()));
        let svc = make_service_with_client(dir.path(), mock);

        let c1 = svc.send_question("First?").unwrap();
        let c2 = svc.send_question("Second?").unwrap();
        assert_eq!(c1, 1);
        assert_eq!(c2, 2);
    }

    #[test]
    fn send_question_empty_room_skips_send() {
        let dir = TempDir::new().unwrap();
        let svc = make_service_empty_room(dir.path());

        let counter = svc.send_question("Should I proceed?").unwrap();
        assert_eq!(counter, 1);

        // Question still stored in state (with empty event ID key)
        let state = svc.state_manager().load_or_default().unwrap();
        assert_eq!(state.pending_questions.len(), 1);
        let pq = state.pending_questions.get("").unwrap();
        assert_eq!(pq.question, "Should I proceed?");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn send_checkin_basic_format() {
        let dir = TempDir::new().unwrap();
        let mock = MockMatrixClient::new();
        mock.on_send_message(Ok("$checkin1:example.com".to_string()));
        let calls_arc = mock.calls_arc();
        let svc = make_service_with_client(dir.path(), mock);

        let result = svc.send_checkin(3, Duration::from_secs(125), None).unwrap();
        assert_eq!(result, 0);

        // Verify the sent message content
        let calls = calls_arc.lock().unwrap();
        let body = calls
            .iter()
            .find_map(|c| match c {
                MockCall::SendMessage { body, .. } => Some(body.clone()),
                _ => None,
            })
            .expect("expected a SendMessage call");
        assert!(body.contains("iteration **3**"), "body: {}", body);
        assert!(body.contains("`2m 5s`"), "body: {}", body);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn send_checkin_with_context() {
        let dir = TempDir::new().unwrap();
        let mock = MockMatrixClient::new();
        mock.on_send_message(Ok("$checkin2:example.com".to_string()));
        let calls_arc = mock.calls_arc();
        let svc = make_service_with_client(dir.path(), mock);

        let ctx = ralph_proto::CheckinContext {
            current_hat: Some("executor".to_string()),
            open_tasks: 3,
            closed_tasks: 1,
            cumulative_cost: 0.1234,
        };

        let result = svc
            .send_checkin(5, Duration::from_secs(42), Some(&ctx))
            .unwrap();
        assert_eq!(result, 0);

        let calls = calls_arc.lock().unwrap();
        let body = calls
            .iter()
            .find_map(|c| match c {
                MockCall::SendMessage { body, .. } => Some(body.clone()),
                _ => None,
            })
            .expect("expected a SendMessage call");
        assert!(body.contains("iteration **5**"), "body: {}", body);
        assert!(body.contains("`42s`"), "body: {}", body);
        assert!(body.contains("Hat: `executor`"), "body: {}", body);
        assert!(
            body.contains("Tasks: **3** open, 1 closed"),
            "body: {}",
            body
        );
        assert!(body.contains("Cost: `$0.1234`"), "body: {}", body);
    }

    #[test]
    fn send_checkin_empty_room_skips() {
        let dir = TempDir::new().unwrap();
        let svc = make_service_empty_room(dir.path());

        let result = svc.send_checkin(1, Duration::from_secs(10), None).unwrap();
        assert_eq!(result, 0);
    }

    #[test]
    fn check_for_response_json_format() {
        let dir = TempDir::new().unwrap();
        let events_path = dir.path().join("events.jsonl");
        std::fs::write(
            &events_path,
            r#"{"topic":"human.response","payload":"yes, proceed"}
"#,
        )
        .unwrap();

        let mut pos = 0;
        let result = MatrixService::check_for_response(&events_path, &mut pos).unwrap();
        assert_eq!(result, Some("yes, proceed".to_string()));
        assert!(pos > 0);
    }

    #[test]
    fn check_for_response_pipe_format() {
        let dir = TempDir::new().unwrap();
        let events_path = dir.path().join("events.jsonl");
        std::fs::write(
            &events_path,
            "2026-03-12 EVENT: human.response | message: \"do it\"\n",
        )
        .unwrap();

        let mut pos = 0;
        let result = MatrixService::check_for_response(&events_path, &mut pos).unwrap();
        assert_eq!(result, Some("do it".to_string()));
    }

    #[test]
    fn check_for_response_missing_file() {
        let dir = TempDir::new().unwrap();
        let events_path = dir.path().join("nonexistent.jsonl");

        let mut pos = 0;
        let result = MatrixService::check_for_response(&events_path, &mut pos).unwrap();
        assert_eq!(result, None);
        assert_eq!(pos, 0);
    }

    #[test]
    fn check_for_response_tracks_position() {
        let dir = TempDir::new().unwrap();
        let events_path = dir.path().join("events.jsonl");

        // Write non-response line first
        std::fs::write(
            &events_path,
            r#"{"topic":"other","payload":"ignore"}
"#,
        )
        .unwrap();

        let mut pos = 0;
        let result = MatrixService::check_for_response(&events_path, &mut pos).unwrap();
        assert_eq!(result, None);
        let pos_after_first = pos;
        assert!(pos_after_first > 0);

        // Append a response event
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&events_path)
            .unwrap();
        writeln!(file, r#"{{"topic":"human.response","payload":"ok"}}"#).unwrap();

        let result = MatrixService::check_for_response(&events_path, &mut pos).unwrap();
        assert_eq!(result, Some("ok".to_string()));
        assert!(pos > pos_after_first);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn wait_for_response_returns_on_response() {
        let dir = TempDir::new().unwrap();
        let ralph_dir = dir.path().join(".ralph");
        std::fs::create_dir_all(&ralph_dir).unwrap();
        let events_path = ralph_dir.join("events.jsonl");

        let mock = MockMatrixClient::new();
        mock.on_send_message(Ok("$q1:example.com".to_string()));
        let svc = make_service_with_client(dir.path(), mock);

        // Send a question to create pending state
        svc.send_question("What next?").unwrap();

        // Write a response event after a tiny delay in a background thread
        let events_path_clone = events_path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            std::fs::write(
                events_path_clone,
                r#"{"topic":"human.response","payload":"do the thing"}
"#,
            )
            .unwrap();
        });

        let result = svc.wait_for_response(&events_path).unwrap();
        assert_eq!(result, Some("do the thing".to_string()));

        // Pending question should be removed
        let state = svc.state_manager().load_or_default().unwrap();
        assert!(state.pending_questions.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn wait_for_response_returns_none_on_timeout() {
        let dir = TempDir::new().unwrap();
        let ralph_dir = dir.path().join(".ralph");
        std::fs::create_dir_all(&ralph_dir).unwrap();
        let events_path = ralph_dir.join("events.jsonl");

        // Create service with 1-second timeout
        let mock = MockMatrixClient::new();
        mock.on_send_message(Ok("$q1:example.com".to_string()));
        let svc = MatrixService::new(
            dir.path().to_path_buf(),
            "!room:example.com".to_string(),
            None,
            1, // 1 second timeout
            "main".to_string(),
            Arc::new(mock),
        );

        // Send a question to create pending state
        svc.send_question("Waiting forever?").unwrap();

        // No response event written → should timeout
        let result = svc.wait_for_response(&events_path).unwrap();
        assert_eq!(result, None);

        // Pending question should be cleaned up
        let state = svc.state_manager().load_or_default().unwrap();
        assert!(state.pending_questions.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn wait_for_response_returns_none_on_shutdown() {
        let dir = TempDir::new().unwrap();
        let ralph_dir = dir.path().join(".ralph");
        std::fs::create_dir_all(&ralph_dir).unwrap();
        let events_path = ralph_dir.join("events.jsonl");

        let mock = MockMatrixClient::new();
        mock.on_send_message(Ok("$q1:example.com".to_string()));
        let svc = make_service_with_client(dir.path(), mock);

        // Send a question to create pending state
        svc.send_question("Should I stop?").unwrap();

        // Set shutdown flag after a tiny delay
        let shutdown = svc.shutdown_flag();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            shutdown.store(true, Ordering::SeqCst);
        });

        let result = svc.wait_for_response(&events_path).unwrap();
        assert_eq!(result, None);

        // Pending question should be cleaned up
        let state = svc.state_manager().load_or_default().unwrap();
        assert!(state.pending_questions.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stop_sets_shutdown_flag() {
        let dir = TempDir::new().unwrap();
        let mock = MockMatrixClient::new();
        let svc = make_service_with_client(dir.path(), mock);

        let flag = svc.shutdown_flag();
        assert!(!flag.load(Ordering::SeqCst));

        svc.stop();

        assert!(
            flag.load(Ordering::SeqCst),
            "shutdown flag should be true after stop()"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stop_sends_farewell_message() {
        let dir = TempDir::new().unwrap();
        let mock = MockMatrixClient::new();
        mock.on_send_message(Ok("$farewell:example.com".to_string()));
        let calls_arc = mock.calls_arc();
        let svc = make_service_with_client(dir.path(), mock);

        svc.stop();

        let calls = calls_arc.lock().unwrap();
        let farewell_sent = calls.iter().any(|call| {
            matches!(call, MockCall::SendMessage { body, .. } if body.contains("disconnecting"))
        });
        assert!(
            farewell_sent,
            "stop() should send a farewell message containing 'disconnecting'"
        );
    }

    #[test]
    fn stop_empty_room_skips_farewell() {
        let dir = TempDir::new().unwrap();
        let svc = make_service_empty_room(dir.path());

        let flag = svc.shutdown_flag();
        svc.stop();

        assert!(
            flag.load(Ordering::SeqCst),
            "shutdown flag should be true even without room_id"
        );
    }

    #[test]
    fn retry_with_backoff_first_attempt() {
        let result = retry_with_backoff(|_| Ok("$event1:example.com".to_string()), |_| {});
        assert_eq!(result.unwrap(), "$event1:example.com");
    }

    #[test]
    fn retry_with_backoff_second_attempt() {
        let mut call_count = 0u32;
        let result = retry_with_backoff(
            |_| {
                call_count += 1;
                if call_count == 1 {
                    Err(MatrixError::SendFailed("transient".to_string()))
                } else {
                    Ok("$event2:example.com".to_string())
                }
            },
            |_| {},
        );
        assert_eq!(result.unwrap(), "$event2:example.com");
        assert_eq!(call_count, 2);
    }

    #[test]
    fn retry_with_backoff_all_fail() {
        let mut call_count = 0u32;
        let result = retry_with_backoff(
            |_| {
                call_count += 1;
                Err(MatrixError::SendFailed("permanent".to_string()))
            },
            |_| {},
        );
        assert!(result.is_err());
        assert_eq!(call_count, 3);
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failed after 3 attempts"));
        assert!(err.contains("permanent"));
    }

    #[test]
    fn robot_service_timeout_secs() {
        let dir = TempDir::new().unwrap();
        let svc = make_service(dir.path());
        let robot: Box<dyn ralph_proto::RobotService> = Box::new(svc);

        assert_eq!(robot.timeout_secs(), 300);
    }

    #[test]
    fn robot_service_shutdown_flag_propagation() {
        let dir = TempDir::new().unwrap();
        let svc = make_service(dir.path());
        let inherent_flag = svc.shutdown_flag();
        let robot: Box<dyn ralph_proto::RobotService> = Box::new(svc);

        let trait_flag = robot.shutdown_flag();
        assert!(!trait_flag.load(Ordering::SeqCst));

        inherent_flag.store(true, Ordering::SeqCst);
        assert!(trait_flag.load(Ordering::SeqCst));
    }

    #[test]
    fn debug_output_masks_fields() {
        let dir = TempDir::new().unwrap();
        let svc = make_service(dir.path());

        let debug_str = format!("{:?}", svc);
        assert!(debug_str.contains("MatrixService"));
        assert!(debug_str.contains("room_id"));
        assert!(debug_str.contains("workspace_root"));
        assert!(debug_str.contains("timeout"));
        // Should NOT expose internal fields like client, handler, shutdown, etc.
        assert!(!debug_str.contains("handler"));
        assert!(!debug_str.contains("shutdown"));
        assert!(!debug_str.contains("message_counter"));
    }
}
