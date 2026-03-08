//! Error types for Rocket.Chat bot operations.
//!
//! Provides [`RocketChatError`] covering auth failures, network errors,
//! rate limiting, deserialization, and state persistence.

use thiserror::Error;

/// Result type alias for Rocket.Chat operations.
pub type RocketChatResult<T> = std::result::Result<T, RocketChatError>;

/// Errors that can occur during Rocket.Chat bot operations.
#[derive(Debug, Error)]
pub enum RocketChatError {
    /// Authentication failed (invalid token, expired credentials, etc.).
    #[error("rocket.chat auth error: {0}")]
    Auth(String),

    /// Failed to send a message.
    #[error("rocket.chat send error: {0}")]
    Send(String),

    /// Failed to receive or poll messages.
    #[error("rocket.chat receive error: {0}")]
    Receive(String),

    /// Requested resource not found (room, user, message).
    #[error("rocket.chat not found: {0}")]
    NotFound(String),

    /// Rate limited by the Rocket.Chat server.
    #[error("rocket.chat rate limited: {0}")]
    RateLimit(String),

    /// Server error (5xx responses, connection failures).
    #[error("rocket.chat server error: {0}")]
    Server(String),

    /// Failed to deserialize a response.
    #[error("rocket.chat deserialization error: {0}")]
    Deserialize(String),

    /// Failed to read or write state file.
    #[error("state persistence error: {0}")]
    State(#[from] std::io::Error),

    /// Failed to parse state JSON.
    #[error("state parse error: {0}")]
    StateParse(#[from] serde_json::Error),

    /// Failed to write event to JSONL.
    #[error("event write error: {0}")]
    EventWrite(String),

    /// Failed to start the service (e.g., no tokio runtime available).
    #[error("rocket.chat startup error: {0}")]
    Startup(String),
}
