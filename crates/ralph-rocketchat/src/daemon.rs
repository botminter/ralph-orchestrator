//! Rocket.Chat daemon adapter.
//!
//! Implements [`DaemonAdapter`] for Rocket.Chat, providing a persistent process
//! that listens for messages and starts orchestration loops on demand.
//!
//! Uses a **turn-taking model**: the daemon polls Rocket.Chat while idle, but
//! stops polling when a loop starts — the loop's own [`RocketChatService`](crate::service::RocketChatService)
//! takes over for the full feature set (commands, guidance, responses,
//! check-ins). When the loop finishes, the daemon resumes.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use tracing::{error, info, warn};

use ralph_proto::daemon::{DaemonAdapter, StartLoopFn};

use crate::client::{RocketChatApi, RocketChatClient};
use crate::loop_lock::{LockState, lock_path, lock_state};
use crate::types::filter_by_operator;

async fn wait_for_shutdown(shutdown: Arc<AtomicBool>) {
    while !shutdown.load(Ordering::Relaxed) {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

/// A Rocket.Chat-based daemon adapter.
///
/// Polls Rocket.Chat for messages while idle and delegates loop execution
/// to the provided [`StartLoopFn`] callback. Supports the shared slash-command
/// set (`/status`, `/tasks`, `/model`, etc.) and graceful shutdown via
/// `SIGINT`/`SIGTERM`.
pub struct RocketChatDaemon {
    pub(crate) server_url: String,
    pub(crate) auth_token: String,
    pub(crate) bot_user_id: String,
    pub(crate) room_id: String,
    pub(crate) operator_id: Option<String>,
}

impl RocketChatDaemon {
    /// Create a new Rocket.Chat daemon.
    ///
    /// * `server_url` — Base URL of the Rocket.Chat server (e.g., `https://chat.example.com`).
    /// * `auth_token` — Personal Access Token for authentication.
    /// * `bot_user_id` — The bot's own Rocket.Chat user ID (used for `X-User-Id` header).
    /// * `room_id` — The room to communicate in.
    /// * `operator_id` — Optional operator user ID for filtering messages in group chats.
    pub fn new(
        server_url: String,
        auth_token: String,
        bot_user_id: String,
        room_id: String,
        operator_id: Option<String>,
    ) -> Self {
        Self {
            server_url,
            auth_token,
            bot_user_id,
            room_id,
            operator_id,
        }
    }
}

#[async_trait]
impl DaemonAdapter for RocketChatDaemon {
    async fn run_daemon(
        &self,
        workspace_root: PathBuf,
        start_loop: StartLoopFn,
    ) -> anyhow::Result<()> {
        let client = RocketChatClient::new(&self.server_url, &self.auth_token, &self.bot_user_id);
        let room_id = &self.room_id;

        // Send greeting
        let _ = client
            .send_message(room_id, "Ralph daemon online 🤖", None)
            .await;

        // Install signal handlers for graceful shutdown
        let shutdown = Arc::new(AtomicBool::new(false));
        {
            let flag = shutdown.clone();
            tokio::spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                flag.store(true, Ordering::Relaxed);
            });
        }
        #[cfg(unix)]
        {
            let flag = shutdown.clone();
            tokio::spawn(async move {
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    Ok(mut sigterm) => {
                        sigterm.recv().await;
                        flag.store(true, Ordering::Relaxed);
                    }
                    Err(e) => {
                        error!(error = %e, "Failed to register SIGTERM handler");
                        flag.store(true, Ordering::Relaxed);
                    }
                }
            });
        }

        // Use current time as the initial sync timestamp
        let mut last_update = chrono::Utc::now().to_rfc3339();

        // Main daemon loop
        'daemon: while !shutdown.load(Ordering::Relaxed) {
            // ── Idle: poll Rocket.Chat for messages ──
            let sync_result = match tokio::select! {
                _ = wait_for_shutdown(shutdown.clone()) => {
                    break 'daemon;
                }
                result = client.sync_messages(room_id, &last_update) => result,
            } {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "Rocket.Chat sync failed, retrying");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
            };

            // Filter messages: exclude bot's own messages and system messages
            let mut messages: Vec<_> = sync_result
                .updated
                .into_iter()
                .filter(|msg| msg.u.id != self.bot_user_id && msg.t.is_none())
                .collect();

            // Apply operator filtering if configured
            if let Some(ref op_id) = self.operator_id {
                messages = filter_by_operator(&messages, op_id);
            }

            // Update sync timestamp if we got any messages at all
            if !messages.is_empty() {
                // Use the latest message timestamp as the new last_update
                if let Some(latest) = messages.iter().map(|m| &m.ts).max() {
                    last_update = latest.clone();
                }
            }

            for message in &messages {
                let text = message.msg.trim();
                if text.is_empty() {
                    continue;
                }

                info!(text = %text, user = %message.u.username, "Daemon received message");

                // Handle slash-commands while idle using the shared command parser.
                if text.starts_with('/') {
                    let response = crate::commands::handle_command(text, &workspace_root)
                        .unwrap_or_else(|| {
                            "Unknown command. Use /help for the supported commands.".to_string()
                        });
                    let _ = client.send_message(room_id, &response, None).await;
                    continue;
                }

                // Regular message → check lock state
                let lp = lock_path(&workspace_root);
                let state = match lock_state(&workspace_root) {
                    Ok(state) => state,
                    Err(e) => {
                        warn!(error = %e, "Failed to check loop lock state");
                        let _ = client
                            .send_message(
                                room_id,
                                "Failed to check loop state; try again in a moment.",
                                None,
                            )
                            .await;
                        continue;
                    }
                };
                if state == LockState::Active {
                    let _ = client
                        .send_message(
                            room_id,
                            "A loop is already running — it will receive your messages directly.",
                            None,
                        )
                        .await;
                    continue;
                }

                if state == LockState::Stale {
                    warn!(
                        lock_path = %lp.display(),
                        "Found stale loop lock; starting new loop"
                    );
                }

                // No loop running — start one with this message as prompt
                let ack = format!("Starting loop: *{}*", text);
                let _ = client.send_message(room_id, &ack, None).await;

                // ── Loop Running: hand off to the loop ──
                // The loop's RocketChatService polls sync_messages, handles commands,
                // guidance, responses, check-ins. We just await completion.
                let prompt = text.to_string();
                let mut loop_handle = tokio::spawn(start_loop(prompt));
                let result = tokio::select! {
                    _ = wait_for_shutdown(shutdown.clone()) => {
                        loop_handle.abort();
                        let _ = loop_handle.await;
                        break 'daemon;
                    }
                    result = &mut loop_handle => result,
                };

                // Loop finished — daemon resumes polling.
                match result {
                    Ok(Ok(description)) => {
                        let notification = format!("Loop complete ({}).", description);
                        let _ = client.send_message(room_id, &notification, None).await;
                    }
                    Ok(Err(e)) => {
                        let notification = format!("Loop failed: {}", e);
                        let _ = client.send_message(room_id, &notification, None).await;
                    }
                    Err(e) => {
                        let notification = format!("Loop failed: {}", e);
                        let _ = client.send_message(room_id, &notification, None).await;
                    }
                }

                // Reset sync timestamp to now so we don't re-process old messages
                last_update = chrono::Utc::now().to_rfc3339();
            }

            // Brief sleep between polls when no messages (sync_messages doesn't long-poll)
            if messages.is_empty() {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }

        // Farewell
        let _ = client
            .send_message(room_id, "Ralph daemon offline 👋", None)
            .await;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rocketchat_daemon_creation() {
        let daemon = RocketChatDaemon::new(
            "https://chat.example.com".to_string(),
            "secret-token".to_string(),
            "bot-user-id".to_string(),
            "room-123".to_string(),
            None,
        );
        assert_eq!(daemon.server_url, "https://chat.example.com");
        assert_eq!(daemon.auth_token, "secret-token");
        assert_eq!(daemon.bot_user_id, "bot-user-id");
        assert_eq!(daemon.room_id, "room-123");
        assert_eq!(daemon.operator_id, None);
    }

    #[test]
    fn test_rocketchat_daemon_with_operator_id() {
        let daemon = RocketChatDaemon::new(
            "https://chat.example.com".to_string(),
            "secret-token".to_string(),
            "bot-user-id".to_string(),
            "room-123".to_string(),
            Some("operator-456".to_string()),
        );
        assert_eq!(daemon.server_url, "https://chat.example.com");
        assert_eq!(daemon.auth_token, "secret-token");
        assert_eq!(daemon.bot_user_id, "bot-user-id");
        assert_eq!(daemon.room_id, "room-123");
        assert_eq!(daemon.operator_id, Some("operator-456".to_string()));
    }
}
