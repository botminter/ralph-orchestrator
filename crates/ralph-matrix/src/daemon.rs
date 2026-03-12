//! Matrix daemon adapter.
//!
//! Implements [`DaemonAdapter`] for Matrix, providing a persistent process
//! that listens for messages and starts orchestration loops on demand.
//!
//! Uses a **turn-taking model**: the daemon polls Matrix via `sync_once` while
//! idle, but stops polling when a loop starts — the loop's own
//! [`MatrixService`](crate::service::MatrixService) takes over for the full
//! feature set (commands, guidance, responses, check-ins). When the loop
//! finishes, the daemon resumes.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tracing::{error, info, warn};

use ralph_proto::daemon::{DaemonAdapter, StartLoopFn};

use crate::client::{MatrixApi, MatrixClient};
use crate::loop_lock::{LockState, lock_path, lock_state};
use crate::types::filter_by_operator;

async fn wait_for_shutdown(shutdown: Arc<AtomicBool>) {
    while !shutdown.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// A Matrix-based daemon adapter.
///
/// Polls Matrix via `sync_once` for messages while idle and delegates loop
/// execution to the provided [`StartLoopFn`] callback. Supports graceful
/// shutdown via `SIGINT`/`SIGTERM`.
pub struct MatrixDaemon {
    pub(crate) homeserver_url: String,
    pub(crate) access_token: String,
    pub(crate) room_id: String,
    pub(crate) operator_id: Option<String>,
}

impl MatrixDaemon {
    /// Create a new Matrix daemon.
    ///
    /// * `homeserver_url` — Base URL of the Matrix homeserver (e.g., `https://matrix.example.com`).
    /// * `access_token` — Access token for authentication.
    /// * `room_id` — The room to communicate in (e.g., `!abc:example.com`).
    /// * `operator_id` — Optional operator user ID for filtering messages in group chats.
    pub fn new(
        homeserver_url: String,
        access_token: String,
        room_id: String,
        operator_id: Option<String>,
    ) -> Self {
        Self {
            homeserver_url,
            access_token,
            room_id,
            operator_id,
        }
    }
}

#[async_trait]
impl DaemonAdapter for MatrixDaemon {
    async fn run_daemon(
        &self,
        workspace_root: PathBuf,
        start_loop: StartLoopFn,
    ) -> anyhow::Result<()> {
        let client = MatrixClient::new();
        client
            .login_with_token(&self.homeserver_url, &self.access_token)
            .await
            .map_err(|e| anyhow::anyhow!("Matrix login failed: {e}"))?;

        let room_id = &self.room_id;

        // Initial sync + join so the SDK knows about the room before we
        // attempt to send messages or look up room state.
        client
            .ensure_room(room_id)
            .await
            .map_err(|e| anyhow::anyhow!("Matrix initial sync/join failed: {e}"))?;

        // Get the bot's display name for filtering out its own messages
        let bot_display_name = client
            .get_display_name()
            .await
            .unwrap_or_else(|_| "Ralph Bot".to_string());

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

        // Main daemon loop
        'daemon: while !shutdown.load(Ordering::Relaxed) {
            // ── Idle: poll Matrix for messages via long-poll sync ──
            let sync_result = match tokio::select! {
                _ = wait_for_shutdown(shutdown.clone()) => {
                    break 'daemon;
                }
                result = client.sync_once(Duration::from_secs(30)) => result,
            } {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "Matrix sync failed, retrying");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };

            // Filter messages: exclude bot's own messages
            let mut messages = sync_result
                .messages
                .into_iter()
                .filter(|msg| !msg.sender_id.contains(&bot_display_name))
                .collect::<Vec<_>>();

            // Apply operator filtering if configured
            if let Some(ref op_id) = self.operator_id {
                messages = filter_by_operator(&messages, op_id);
            }

            for message in &messages {
                let text = message.body.trim();
                if text.is_empty() {
                    continue;
                }

                info!(text = %text, sender = %message.sender_id, "Daemon received message");

                // Handle bot commands while idle.
                if text.starts_with('!') {
                    let response = crate::commands::handle_command(text, &workspace_root)
                        .unwrap_or_else(|| {
                            "Unknown command. Use !help for the supported commands.".to_string()
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

                // Do a fresh sync to skip messages received during the loop
                let _ = client.sync_once(Duration::from_secs(1)).await;
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
    fn test_matrix_daemon_creation() {
        let daemon = MatrixDaemon::new(
            "https://matrix.example.com".to_string(),
            "syt_secret_token".to_string(),
            "!room-123:example.com".to_string(),
            None,
        );
        assert_eq!(daemon.homeserver_url, "https://matrix.example.com");
        assert_eq!(daemon.access_token, "syt_secret_token");
        assert_eq!(daemon.room_id, "!room-123:example.com");
        assert_eq!(daemon.operator_id, None);
    }

    #[test]
    fn test_matrix_daemon_with_operator_id() {
        let daemon = MatrixDaemon::new(
            "https://matrix.example.com".to_string(),
            "syt_secret_token".to_string(),
            "!room-123:example.com".to_string(),
            Some("@operator:example.com".to_string()),
        );
        assert_eq!(daemon.homeserver_url, "https://matrix.example.com");
        assert_eq!(daemon.access_token, "syt_secret_token");
        assert_eq!(daemon.room_id, "!room-123:example.com");
        assert_eq!(
            daemon.operator_id,
            Some("@operator:example.com".to_string())
        );
    }
}
