//! Rocket.Chat service lifecycle management.
//!
//! Implements [`RobotService`](ralph_proto::RobotService) for Rocket.Chat, coordinating
//! question sending, response waiting, check-ins, and background message polling
//! within the Ralph event loop.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::Utc;
use tracing::{debug, info, warn};

use crate::client::RocketChatApi;
use crate::error::{RocketChatError, RocketChatResult};
use crate::handler::MessageHandler;
use crate::state::StateManager;
use crate::types::filter_by_operator;

/// Maximum number of retry attempts for sending messages.
pub const MAX_SEND_RETRIES: u32 = 3;

/// Base delay for exponential backoff (1 second).
pub const BASE_RETRY_DELAY: Duration = Duration::from_secs(1);

/// Execute a fallible send operation with exponential backoff retry.
///
/// Retries up to [`MAX_SEND_RETRIES`] times with delays of 1s, 2s, 4s.
/// Returns the result on success, or `RocketChatError::Send` after all
/// retries are exhausted.
///
/// The `sleep_fn` parameter allows tests to substitute a no-op sleep.
pub fn retry_with_backoff<F, S>(mut send_fn: F, mut sleep_fn: S) -> RocketChatResult<String>
where
    F: FnMut(u32) -> RocketChatResult<String>,
    S: FnMut(Duration),
{
    let mut last_error = String::new();

    for attempt in 1..=MAX_SEND_RETRIES {
        match send_fn(attempt) {
            Ok(msg_id) => return Ok(msg_id),
            Err(e) => {
                last_error = e.to_string();
                warn!(
                    attempt = attempt,
                    max_retries = MAX_SEND_RETRIES,
                    error = %last_error,
                    "Rocket.Chat send failed, {}",
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

    Err(RocketChatError::Send(format!(
        "failed after {} attempts: {}",
        MAX_SEND_RETRIES, last_error
    )))
}

/// Coordinates the Rocket.Chat bot lifecycle with the Ralph event loop.
///
/// Manages startup, shutdown, message sending, and response waiting.
/// Mirrors [`TelegramService`](../../ralph-telegram/src/service.rs) but uses
/// REST polling via `chat.syncMessages` instead of Telegram's `getUpdates`.
pub struct RocketChatService {
    workspace_root: PathBuf,
    server_url: String,
    auth_token: String,
    bot_user_id: String,
    room_id: String,
    operator_id: Option<String>,
    timeout_secs: u64,
    loop_id: String,
    state_manager: StateManager,
    handler: MessageHandler,
    client: Box<dyn RocketChatApi>,
    shutdown: Arc<AtomicBool>,
}

impl RocketChatService {
    /// Create a new RocketChatService.
    ///
    /// All authentication parameters must be pre-resolved by the config layer.
    /// The `operator_id` filters messages in group chats to only process the
    /// designated human operator; when `None`, all non-system messages are
    /// processed.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        workspace_root: PathBuf,
        server_url: String,
        auth_token: String,
        bot_user_id: String,
        room_id: String,
        operator_id: Option<String>,
        timeout_secs: u64,
        loop_id: String,
        client: Box<dyn RocketChatApi>,
    ) -> Self {
        let state_path = workspace_root.join(".ralph/rocketchat-state.json");
        let state_manager = StateManager::new(&state_path);
        let handler_state_manager = StateManager::new(&state_path);
        let handler = MessageHandler::new(handler_state_manager, &workspace_root);
        let shutdown = Arc::new(AtomicBool::new(false));

        Self {
            workspace_root,
            server_url,
            auth_token,
            bot_user_id,
            room_id,
            operator_id,
            timeout_secs,
            loop_id,
            state_manager,
            handler,
            client,
            shutdown,
        }
    }

    /// Get a reference to the workspace root.
    pub fn workspace_root(&self) -> &PathBuf {
        &self.workspace_root
    }

    /// Get the configured timeout in seconds.
    pub fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }

    /// Get the auth token masked for logging.
    pub fn auth_token_masked(&self) -> String {
        if self.auth_token.len() > 8 {
            format!(
                "{}...{}",
                &self.auth_token[..4],
                &self.auth_token[self.auth_token.len() - 4..]
            )
        } else {
            "****".to_string()
        }
    }

    /// Get a reference to the state manager.
    pub fn state_manager(&self) -> &StateManager {
        &self.state_manager
    }

    /// Get a mutable reference to the message handler.
    pub fn handler(&mut self) -> &mut MessageHandler {
        &mut self.handler
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

    /// Get a reference to the API client.
    pub fn client(&self) -> &dyn RocketChatApi {
        &*self.client
    }

    /// Returns a clone of the shutdown flag.
    ///
    /// Signal handlers can set this flag to interrupt `wait_for_response()`
    /// without waiting for the full timeout.
    pub fn shutdown_flag(&self) -> Arc<AtomicBool> {
        self.shutdown.clone()
    }

    /// Start the Rocket.Chat service.
    ///
    /// Spawns a background polling task on the host tokio runtime to receive
    /// incoming messages via `chat.syncMessages`. Must be called from within
    /// a tokio runtime context.
    pub fn start(&self) -> RocketChatResult<()> {
        info!(
            server_url = %self.server_url,
            auth_token = %self.auth_token_masked(),
            workspace = %self.workspace_root.display(),
            timeout_secs = self.timeout_secs,
            "Rocket.Chat service starting"
        );

        let handle = tokio::runtime::Handle::try_current().map_err(|_| {
            RocketChatError::Startup("no tokio runtime available for polling".to_string())
        })?;

        let workspace_root = self.workspace_root.clone();
        let state_path = self.workspace_root.join(".ralph/rocketchat-state.json");
        let room_id = self.room_id.clone();
        let operator_id = self.operator_id.clone();
        let shutdown = self.shutdown.clone();
        let loop_id = self.loop_id.clone();
        let server_url = self.server_url.clone();
        let auth_token = self.auth_token.clone();
        let bot_user_id = self.bot_user_id.clone();

        handle.spawn(async move {
            let client =
                crate::client::RocketChatClient::new(&server_url, &auth_token, &bot_user_id);
            Self::poll_messages(
                Box::new(client),
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
            let greeting = format!("🤖 Ralph loop `{}` connected via Rocket.Chat", self.loop_id);
            let room_id = self.room_id.clone();
            let client_server_url = self.server_url.clone();
            let client_auth_token = self.auth_token.clone();
            let client_bot_user_id = self.bot_user_id.clone();

            let handle = tokio::runtime::Handle::try_current().map_err(|_| {
                RocketChatError::Startup("no tokio runtime available for greeting".to_string())
            })?;
            handle.spawn(async move {
                let client = crate::client::RocketChatClient::new(
                    &client_server_url,
                    &client_auth_token,
                    &client_bot_user_id,
                );
                match client.send_message(&room_id, &greeting, None).await {
                    Ok(_) => info!("Sent greeting to room {}", room_id),
                    Err(e) => warn!(error = %e, "Failed to send greeting"),
                }
            });
        }

        info!("Rocket.Chat service started — polling for incoming messages");
        Ok(())
    }

    /// Background polling task that receives incoming Rocket.Chat messages.
    ///
    /// Uses `chat.syncMessages` to receive messages, then routes them
    /// through `MessageHandler` to write events to the correct loop's JSONL.
    async fn poll_messages(
        client: Box<dyn RocketChatApi>,
        workspace_root: PathBuf,
        state_path: PathBuf,
        room_id: String,
        operator_id: Option<String>,
        shutdown: Arc<AtomicBool>,
        loop_id: String,
    ) {
        let state_manager = StateManager::new(&state_path);
        let handler_state_manager = StateManager::new(&state_path);
        let handler = MessageHandler::new(handler_state_manager, &workspace_root);

        info!(loop_id = %loop_id, "Rocket.Chat polling task started");

        while !shutdown.load(Ordering::Relaxed) {
            let mut state = match state_manager.load_or_default() {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "Failed to load Rocket.Chat state");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };

            let last_sync = state
                .last_sync
                .clone()
                .unwrap_or_else(|| Utc::now().to_rfc3339());

            match client.sync_messages(&room_id, &last_sync).await {
                Ok(sync_result) => {
                    let messages = if let Some(ref op_id) = operator_id {
                        filter_by_operator(&sync_result.updated, op_id)
                    } else {
                        // Without operator filter, still exclude system messages
                        sync_result
                            .updated
                            .into_iter()
                            .filter(|msg| msg.t.is_none())
                            .collect()
                    };

                    for msg in &messages {
                        debug!(
                            msg_id = %msg.id,
                            user = %msg.u.username,
                            text = %msg.msg,
                            "Processing Rocket.Chat message"
                        );

                        match handler.handle_message(&mut state, msg) {
                            Ok(topic) => {
                                info!(
                                    topic = %topic,
                                    msg_id = %msg.id,
                                    "Routed Rocket.Chat message as {topic}"
                                );
                            }
                            Err(e) => {
                                warn!(
                                    error = %e,
                                    msg_id = %msg.id,
                                    "Failed to handle Rocket.Chat message"
                                );
                            }
                        }
                    }

                    // Update last_sync timestamp
                    state.last_sync = Some(Utc::now().to_rfc3339());
                    if let Err(e) = state_manager.save(&state) {
                        warn!(error = %e, "Failed to persist Rocket.Chat state");
                    }
                }
                Err(e) => {
                    if !shutdown.load(Ordering::Relaxed) {
                        warn!(error = %e, "Rocket.Chat polling error — retrying in 5s");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                }
            }

            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        info!(loop_id = %loop_id, "Rocket.Chat polling task stopped");
    }

    /// Stop the Rocket.Chat service gracefully.
    ///
    /// Sends a farewell message and signals the background polling task to shut down.
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
            "Rocket.Chat service stopped"
        );
    }

    /// Send a question to the human via Rocket.Chat and store it as a pending question.
    ///
    /// The question payload is extracted from the `human.interact` event. A pending
    /// question is stored in the state manager so that incoming thread replies can be
    /// routed back to the correct loop.
    ///
    /// On send failure, retries up to 3 times with exponential backoff (1s, 2s, 4s).
    /// Returns 0 (the trait requires `i32` but RC message IDs are strings, stored
    /// in pending question state instead).
    pub fn send_question(&self, payload: &str) -> RocketChatResult<i32> {
        let mut state = self.state_manager.load_or_default()?;

        let message_id = if self.room_id.is_empty() {
            warn!(
                loop_id = %self.loop_id,
                "No room ID configured — human.interact question logged but not sent: {}",
                payload
            );
            String::new()
        } else {
            self.send_with_retry(payload)?
        };

        self.state_manager
            .add_pending_question(&mut state, &self.loop_id, &message_id)?;

        debug!(
            loop_id = %self.loop_id,
            message_id = %message_id,
            "Stored pending question"
        );

        Ok(0)
    }

    /// Send a periodic check-in message via Rocket.Chat.
    ///
    /// Sends a short status update so the human knows the loop is still running.
    /// Skips silently if no room ID is configured.
    pub fn send_checkin(
        &self,
        iteration: u32,
        elapsed: Duration,
        context: Option<&ralph_proto::CheckinContext>,
    ) -> RocketChatResult<i32> {
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

    /// Attempt to send a message with exponential backoff retries.
    ///
    /// Uses the host tokio runtime via `block_in_place` + `Handle::block_on`
    /// to bridge the sync event loop to the async REST API.
    ///
    /// Returns the server-assigned message ID string on success.
    fn send_with_retry(&self, payload: &str) -> RocketChatResult<String> {
        let handle = tokio::runtime::Handle::try_current().map_err(|_| {
            RocketChatError::Send("no tokio runtime available for sending".to_string())
        })?;

        let room_id = self.room_id.clone();
        retry_with_backoff(
            |_attempt| {
                tokio::task::block_in_place(|| {
                    let msg = handle.block_on(self.client.send_message(&room_id, payload, None))?;
                    Ok(msg.id)
                })
            },
            |delay| std::thread::sleep(delay),
        )
    }

    /// Poll the events file for a `human.response` event, blocking until one
    /// arrives or the configured timeout expires.
    ///
    /// Polls the given `events_path` every 250ms for new lines containing
    /// `"human.response"`. On response, removes the pending question and
    /// returns the response message. On timeout, removes the pending question
    /// and returns `None`.
    pub fn wait_for_response(&self, events_path: &Path) -> RocketChatResult<Option<String>> {
        let timeout = Duration::from_secs(self.timeout_secs);
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
            timeout_secs = self.timeout_secs,
            events_path = %events_path.display(),
            "Waiting for human.response"
        );

        loop {
            if Instant::now() >= deadline {
                warn!(
                    loop_id = %self.loop_id,
                    timeout_secs = self.timeout_secs,
                    "Timed out waiting for human.response"
                );

                if let Ok(mut state) = self.state_manager.load_or_default() {
                    let _ = self
                        .state_manager
                        .remove_pending_question(&mut state, &self.loop_id);
                }

                return Ok(None);
            }

            if self.shutdown.load(Ordering::Relaxed) {
                info!(loop_id = %self.loop_id, "Interrupted while waiting for human.response");
                if let Ok(mut state) = self.state_manager.load_or_default() {
                    let _ = self
                        .state_manager
                        .remove_pending_question(&mut state, &self.loop_id);
                }
                return Ok(None);
            }

            if let Some(response) = Self::check_for_response(events_path, &mut file_pos)? {
                info!(
                    loop_id = %self.loop_id,
                    "Received human.response: {}",
                    response
                );

                if let Ok(mut state) = self.state_manager.load_or_default() {
                    let _ = self
                        .state_manager
                        .remove_pending_question(&mut state, &self.loop_id);
                }

                return Ok(Some(response));
            }

            std::thread::sleep(poll_interval);
        }
    }

    /// Check the events file for a `human.response` event starting from
    /// `file_pos`. Updates `file_pos` to the new end of file.
    fn check_for_response(
        events_path: &Path,
        file_pos: &mut u64,
    ) -> RocketChatResult<Option<String>> {
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

            // Try to parse as JSON event
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

            // Also check pipe-separated format (written by MessageHandler)
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
}

impl ralph_proto::RobotService for RocketChatService {
    fn send_question(&self, payload: &str) -> anyhow::Result<i32> {
        Ok(RocketChatService::send_question(self, payload)?)
    }

    fn wait_for_response(&self, events_path: &Path) -> anyhow::Result<Option<String>> {
        Ok(RocketChatService::wait_for_response(self, events_path)?)
    }

    fn send_checkin(
        &self,
        iteration: u32,
        elapsed: Duration,
        context: Option<&ralph_proto::CheckinContext>,
    ) -> anyhow::Result<i32> {
        Ok(RocketChatService::send_checkin(
            self, iteration, elapsed, context,
        )?)
    }

    fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }

    fn shutdown_flag(&self) -> Arc<AtomicBool> {
        self.shutdown.clone()
    }

    fn stop(self: Box<Self>) {
        RocketChatService::stop(*self);
    }
}

impl fmt::Debug for RocketChatService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RocketChatService")
            .field("workspace_root", &self.workspace_root)
            .field("server_url", &self.server_url)
            .field("auth_token", &self.auth_token_masked())
            .field("timeout_secs", &self.timeout_secs)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{MockCall, MockRocketChatClient};
    use std::io::Write;
    use tempfile::TempDir;

    fn make_service(dir: &std::path::Path) -> RocketChatService {
        let mock = MockRocketChatClient::new();
        RocketChatService::new(
            dir.to_path_buf(),
            "https://chat.example.com".to_string(),
            "test-auth-token-12345".to_string(),
            "bot-user-id".to_string(),
            "room-abc".to_string(),
            Some("operator-1".to_string()),
            300,
            "main".to_string(),
            Box::new(mock),
        )
    }

    fn make_service_with_mock(
        dir: &std::path::Path,
        mock: MockRocketChatClient,
    ) -> RocketChatService {
        RocketChatService::new(
            dir.to_path_buf(),
            "https://chat.example.com".to_string(),
            "test-auth-token-12345".to_string(),
            "bot-user-id".to_string(),
            "room-abc".to_string(),
            Some("operator-1".to_string()),
            300,
            "main".to_string(),
            Box::new(mock),
        )
    }

    #[test]
    fn new_creates_service_with_correct_fields() {
        let dir = TempDir::new().unwrap();
        let svc = make_service(dir.path());

        assert_eq!(svc.workspace_root(), dir.path());
        assert_eq!(svc.timeout_secs(), 300);
        assert_eq!(svc.loop_id(), "main");
        assert_eq!(svc.room_id(), "room-abc");
        assert_eq!(svc.operator_id(), Some("operator-1"));
    }

    #[test]
    fn auth_token_masked_hides_middle() {
        let dir = TempDir::new().unwrap();
        let svc = make_service(dir.path());
        let masked = svc.auth_token_masked();
        assert_eq!(masked, "test...2345");
    }

    #[test]
    fn auth_token_masked_short_token() {
        let dir = TempDir::new().unwrap();
        let mock = MockRocketChatClient::new();
        let svc = RocketChatService::new(
            dir.path().to_path_buf(),
            "https://chat.example.com".to_string(),
            "short".to_string(),
            "bot".to_string(),
            "room".to_string(),
            None,
            60,
            "main".to_string(),
            Box::new(mock),
        );
        assert_eq!(svc.auth_token_masked(), "****");
    }

    #[test]
    fn shutdown_flag_is_initially_false() {
        let dir = TempDir::new().unwrap();
        let svc = make_service(dir.path());
        let flag = svc.shutdown_flag();
        assert!(!flag.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn operator_id_none_when_not_set() {
        let dir = TempDir::new().unwrap();
        let mock = MockRocketChatClient::new();
        let svc = RocketChatService::new(
            dir.path().to_path_buf(),
            "https://chat.example.com".to_string(),
            "token-abcdef123456".to_string(),
            "bot".to_string(),
            "room".to_string(),
            None,
            60,
            "main".to_string(),
            Box::new(mock),
        );
        assert_eq!(svc.operator_id(), None);
    }

    // ── send_question tests ──────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_question_sends_message_via_client() {
        let dir = TempDir::new().unwrap();
        let mock = MockRocketChatClient::new();
        let calls_ref = mock.calls_arc();
        let svc = make_service_with_mock(dir.path(), mock);

        let result = svc.send_question("Which DB should we use?");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);

        let calls = calls_ref.lock().unwrap();
        let sent = calls.iter().any(
            |call| matches!(call, MockCall::SendMessage { text, .. } if text.contains("Which DB")),
        );
        assert!(
            sent,
            "send_question should have sent a message via the client"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_question_stores_pending_question_in_state() {
        let dir = TempDir::new().unwrap();
        let mock = MockRocketChatClient::new();
        let svc = make_service_with_mock(dir.path(), mock);

        svc.send_question("Async or sync?").unwrap();

        let state = svc.state_manager().load_or_default().unwrap();
        assert!(
            state.pending_questions.contains_key("main"),
            "pending question should be stored for loop_id 'main'"
        );
        assert!(
            !state.pending_questions["main"].message_id.is_empty(),
            "pending question should have a message_id from the mock"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_question_empty_room_id_skips_send() {
        let dir = TempDir::new().unwrap();
        let mock = MockRocketChatClient::new();
        let svc = RocketChatService::new(
            dir.path().to_path_buf(),
            "https://chat.example.com".to_string(),
            "test-auth-token-12345".to_string(),
            "bot-user-id".to_string(),
            String::new(), // empty room_id
            None,
            300,
            "main".to_string(),
            Box::new(mock),
        );

        let result = svc.send_question("Will this be sent?");
        assert!(result.is_ok());

        // Still stores a pending question (with empty message_id)
        let state = svc.state_manager().load_or_default().unwrap();
        assert!(state.pending_questions.contains_key("main"));
        assert!(state.pending_questions["main"].message_id.is_empty());
    }

    // ── send_checkin tests ───────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_checkin_formats_basic_message() {
        let dir = TempDir::new().unwrap();
        let mock = MockRocketChatClient::new();
        let svc = make_service_with_mock(dir.path(), mock);

        let result = svc.send_checkin(5, Duration::from_secs(90), None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_checkin_with_context_includes_details() {
        let dir = TempDir::new().unwrap();
        let mock = MockRocketChatClient::new();
        let svc = make_service_with_mock(dir.path(), mock);

        let ctx = ralph_proto::CheckinContext {
            current_hat: Some("executor".to_string()),
            open_tasks: 3,
            closed_tasks: 5,
            cumulative_cost: 1.2345,
        };

        let result = svc.send_checkin(10, Duration::from_secs(125), Some(&ctx));
        assert!(result.is_ok());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_checkin_empty_room_id_skips_send() {
        let dir = TempDir::new().unwrap();
        let mock = MockRocketChatClient::new();
        let svc = RocketChatService::new(
            dir.path().to_path_buf(),
            "https://chat.example.com".to_string(),
            "test-auth-token-12345".to_string(),
            "bot-user-id".to_string(),
            String::new(), // empty room_id
            None,
            300,
            "main".to_string(),
            Box::new(mock),
        );

        let result = svc.send_checkin(1, Duration::from_secs(30), None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
    }

    // ── shutdown / stop tests ────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_sets_shutdown_flag() {
        let dir = TempDir::new().unwrap();
        let mock = MockRocketChatClient::new();
        let svc = make_service_with_mock(dir.path(), mock);

        let flag = svc.shutdown_flag();
        assert!(!flag.load(Ordering::SeqCst));

        svc.stop();

        assert!(
            flag.load(Ordering::SeqCst),
            "shutdown flag should be true after stop()"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_sends_farewell_message() {
        let dir = TempDir::new().unwrap();
        let mock = MockRocketChatClient::new();
        // Get a handle to check calls after stop
        let calls_ref = mock.calls_arc();
        let svc = make_service_with_mock(dir.path(), mock);

        svc.stop();

        let calls = calls_ref.lock().unwrap();
        let farewell_sent = calls.iter().any(|call| {
            matches!(call, MockCall::SendMessage { text, .. } if text.contains("disconnecting"))
        });
        assert!(
            farewell_sent,
            "stop() should send a farewell message containing 'disconnecting'"
        );
    }

    // ── timeout_secs via RobotService trait ──────────────────────────────

    #[test]
    fn robot_service_timeout_secs_returns_configured_value() {
        let dir = TempDir::new().unwrap();
        let mock = MockRocketChatClient::new();
        let svc = RocketChatService::new(
            dir.path().to_path_buf(),
            "https://chat.example.com".to_string(),
            "test-auth-token-12345".to_string(),
            "bot-user-id".to_string(),
            "room-abc".to_string(),
            None,
            42,
            "main".to_string(),
            Box::new(mock),
        );

        assert_eq!(ralph_proto::RobotService::timeout_secs(&svc), 42);
    }

    #[test]
    fn robot_service_shutdown_flag_propagation() {
        let dir = TempDir::new().unwrap();
        let mock = MockRocketChatClient::new();
        let svc = RocketChatService::new(
            dir.path().to_path_buf(),
            "https://chat.example.com".to_string(),
            "test-auth-token-12345".to_string(),
            "bot-user-id".to_string(),
            "room-abc".to_string(),
            None,
            60,
            "main".to_string(),
            Box::new(mock),
        );

        let flag = ralph_proto::RobotService::shutdown_flag(&svc);
        assert!(!flag.load(Ordering::SeqCst));

        flag.store(true, Ordering::SeqCst);
        let flag2 = ralph_proto::RobotService::shutdown_flag(&svc);
        assert!(
            flag2.load(Ordering::SeqCst),
            "shutdown flag should be shared across clones"
        );
    }

    // ── retry_with_backoff tests ─────────────────────────────────────────

    #[test]
    fn retry_with_backoff_succeeds_on_first_attempt() {
        let attempts = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let attempts_clone = attempts.clone();

        let result = retry_with_backoff(
            |attempt| {
                attempts_clone.lock().unwrap().push(attempt);
                Ok("msg-1".to_string())
            },
            |_delay| {},
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "msg-1");
        assert_eq!(*attempts.lock().unwrap(), vec![1]);
    }

    #[test]
    fn retry_with_backoff_succeeds_on_second_attempt() {
        let attempts = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let attempts_clone = attempts.clone();
        let delays = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let delays_clone = delays.clone();

        let result = retry_with_backoff(
            |attempt| {
                attempts_clone.lock().unwrap().push(attempt);
                if attempt < 2 {
                    Err(RocketChatError::Send("transient".to_string()))
                } else {
                    Ok("msg-2".to_string())
                }
            },
            |delay| {
                delays_clone.lock().unwrap().push(delay);
            },
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "msg-2");
        assert_eq!(*attempts.lock().unwrap(), vec![1, 2]);
        assert_eq!(*delays.lock().unwrap(), vec![Duration::from_secs(1)]);
    }

    #[test]
    fn retry_with_backoff_fails_after_all_retries() {
        let attempts = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let attempts_clone = attempts.clone();
        let delays = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let delays_clone = delays.clone();

        let result = retry_with_backoff(
            |attempt| {
                attempts_clone.lock().unwrap().push(attempt);
                Err(RocketChatError::Send(format!("fail-{}", attempt)))
            },
            |delay| {
                delays_clone.lock().unwrap().push(delay);
            },
        );

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, RocketChatError::Send(_)));
        assert!(err.to_string().contains("fail-3"));
        assert_eq!(*attempts.lock().unwrap(), vec![1, 2, 3]);
        assert_eq!(
            *delays.lock().unwrap(),
            vec![Duration::from_secs(1), Duration::from_secs(2)]
        );
    }

    // ── check_for_response tests ─────────────────────────────────────────

    #[test]
    fn check_for_response_json_format() {
        let dir = TempDir::new().unwrap();
        let events_path = dir.path().join("events.jsonl");

        let mut file = std::fs::File::create(&events_path).unwrap();
        writeln!(
            file,
            r#"{{"topic":"build.done","payload":"done","ts":"2026-01-30T00:00:00Z"}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"topic":"human.response","payload":"Use async","ts":"2026-01-30T00:01:00Z"}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let mut pos = 0;
        let result = RocketChatService::check_for_response(&events_path, &mut pos).unwrap();
        assert_eq!(result, Some("Use async".to_string()));
    }

    #[test]
    fn check_for_response_pipe_format() {
        let dir = TempDir::new().unwrap();
        let events_path = dir.path().join("events.jsonl");

        let mut file = std::fs::File::create(&events_path).unwrap();
        writeln!(
            file,
            r#"EVENT: human.response | message: "Use sync" | timestamp: "2026-01-30T00:01:00Z""#
        )
        .unwrap();
        file.flush().unwrap();

        let mut pos = 0;
        let result = RocketChatService::check_for_response(&events_path, &mut pos).unwrap();
        assert_eq!(result, Some("Use sync".to_string()));
    }

    #[test]
    fn check_for_response_missing_file() {
        let dir = TempDir::new().unwrap();
        let events_path = dir.path().join("does-not-exist.jsonl");

        let mut pos = 0;
        let result = RocketChatService::check_for_response(&events_path, &mut pos).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn check_for_response_tracks_position() {
        let dir = TempDir::new().unwrap();
        let events_path = dir.path().join("events.jsonl");

        let mut file = std::fs::File::create(&events_path).unwrap();
        writeln!(
            file,
            r#"{{"topic":"build.done","payload":"done","ts":"2026-01-30T00:00:00Z"}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let mut pos = 0;
        let result = RocketChatService::check_for_response(&events_path, &mut pos).unwrap();
        assert_eq!(result, None);
        assert!(pos > 0, "position should advance after reading");

        let pos_after_first = pos;

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&events_path)
            .unwrap();
        writeln!(
            file,
            r#"{{"topic":"human.response","payload":"yes","ts":"2026-01-30T00:02:00Z"}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let result = RocketChatService::check_for_response(&events_path, &mut pos).unwrap();
        assert_eq!(result, Some("yes".to_string()));
        assert!(pos > pos_after_first, "position should advance further");
    }

    // ── wait_for_response tests ──────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wait_for_response_returns_on_response() {
        let dir = TempDir::new().unwrap();
        let mock = MockRocketChatClient::new();
        let svc = RocketChatService::new(
            dir.path().to_path_buf(),
            "https://chat.example.com".to_string(),
            "test-auth-token-12345".to_string(),
            "bot-user-id".to_string(),
            "room-abc".to_string(),
            None,
            5,
            "main".to_string(),
            Box::new(mock),
        );

        let events_path = dir.path().join("events.jsonl");
        std::fs::File::create(&events_path).unwrap();

        svc.send_question("Which plan?").unwrap();

        let writer_path = events_path.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&writer_path)
                .unwrap();
            writeln!(
                file,
                r#"{{"topic":"human.response","payload":"Go with plan A","ts":"2026-01-30T00:00:00Z"}}"#
            )
            .unwrap();
            file.flush().unwrap();
        });

        let result = svc.wait_for_response(&events_path).unwrap();
        writer.join().unwrap();

        assert_eq!(result, Some("Go with plan A".to_string()));

        let state = svc.state_manager().load_or_default().unwrap();
        assert!(
            !state.pending_questions.contains_key("main"),
            "pending question should be removed after response"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wait_for_response_returns_none_on_timeout() {
        let dir = TempDir::new().unwrap();
        let mock = MockRocketChatClient::new();
        let svc = RocketChatService::new(
            dir.path().to_path_buf(),
            "https://chat.example.com".to_string(),
            "test-auth-token-12345".to_string(),
            "bot-user-id".to_string(),
            "room-abc".to_string(),
            None,
            1, // 1 second timeout
            "main".to_string(),
            Box::new(mock),
        );

        let events_path = dir.path().join("events.jsonl");
        std::fs::File::create(&events_path).unwrap();

        svc.send_question("Will this timeout?").unwrap();

        let result = svc.wait_for_response(&events_path).unwrap();
        assert_eq!(result, None, "should return None on timeout");

        let state = svc.state_manager().load_or_default().unwrap();
        assert!(
            !state.pending_questions.contains_key("main"),
            "pending question should be removed on timeout"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wait_for_response_returns_none_on_shutdown() {
        let dir = TempDir::new().unwrap();
        let mock = MockRocketChatClient::new();
        let svc = RocketChatService::new(
            dir.path().to_path_buf(),
            "https://chat.example.com".to_string(),
            "test-auth-token-12345".to_string(),
            "bot-user-id".to_string(),
            "room-abc".to_string(),
            None,
            60, // long timeout — shutdown flag should preempt
            "main".to_string(),
            Box::new(mock),
        );

        let events_path = dir.path().join("events.jsonl");
        std::fs::File::create(&events_path).unwrap();

        svc.shutdown_flag().store(true, Ordering::Relaxed);

        let start = Instant::now();
        let result = svc.wait_for_response(&events_path).unwrap();
        let elapsed = start.elapsed();

        assert_eq!(result, None, "should return None when shutdown flag is set");
        assert!(
            elapsed < Duration::from_secs(2),
            "should return quickly, not wait for timeout (elapsed: {:?})",
            elapsed
        );
    }

    // ── Debug impl test ──────────────────────────────────────────────────

    #[test]
    fn debug_output_masks_token() {
        let dir = TempDir::new().unwrap();
        let svc = make_service(dir.path());
        let debug = format!("{:?}", svc);
        assert!(debug.contains("test...2345"));
        assert!(!debug.contains("test-auth-token-12345"));
    }
}
