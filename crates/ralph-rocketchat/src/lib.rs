//! # ralph-rocketchat
//!
//! Rocket.Chat integration for human-in-the-loop orchestration in Ralph.
//!
//! This crate provides bidirectional communication between AI agents and humans
//! during orchestration loops via Rocket.Chat's REST API:
//!
//! - **AI → Human**: Agents emit `human.interact` events; the service sends questions to Rocket.Chat
//! - **Human → AI**: Humans reply or send proactive guidance via Rocket.Chat messages
//!
//! Uses REST polling via `chat.syncMessages` (not DDP/WebSocket) and authenticates
//! with Personal Access Tokens (`X-Auth-Token` + `X-User-Id`).
//!
//! ## Key Components
//!
//! - [`RocketChatClient`] — Production REST API client (implements [`RocketChatApi`])
//! - [`RocketChatService`] — Lifecycle management for the bot within the event loop
//! - [`RocketChatDaemon`] — Persistent daemon that polls while idle and delegates to loops
//! - [`MessageHandler`] — Processes incoming messages and writes events to JSONL
//! - [`StateManager`] — Persists sync timestamps, pending questions, and reply routing
//! - [`error`] — Error types for auth, send, receive, and state failures

pub mod client;
pub mod commands;
pub mod daemon;
pub mod error;
pub mod handler;
pub(crate) mod loop_lock;
pub mod service;
pub mod state;
pub mod types;

pub use client::{MockRocketChatClient, RocketChatApi, RocketChatClient};
pub use daemon::RocketChatDaemon;
pub use error::{RocketChatError, RocketChatResult};
pub use handler::MessageHandler;
pub use service::{BASE_RETRY_DELAY, MAX_SEND_RETRIES, RocketChatService, retry_with_backoff};
pub use state::{PendingQuestion, RocketChatState, StateManager};
pub use types::{RcMessage, RcRoom, RcUser, SyncResult, filter_by_operator};
