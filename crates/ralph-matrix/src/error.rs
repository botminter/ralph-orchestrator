//! Error types for Matrix bot operations.
//!
//! Provides [`MatrixError`] covering auth failures, network errors,
//! room resolution, message sending, sync, and state persistence.

use thiserror::Error;

/// Result type alias for Matrix operations.
pub type MatrixResult<T> = std::result::Result<T, MatrixError>;

/// Errors that can occur during Matrix bot operations.
#[derive(Debug, Error)]
pub enum MatrixError {
    /// Authentication failed (invalid token, expired credentials, etc.).
    #[error("matrix auth error: {0}")]
    Auth(String),

    /// Network-level failure (connection refused, DNS, timeout).
    #[error("matrix network error: {0}")]
    Network(String),

    /// Requested room not found or inaccessible.
    #[error("matrix room not found: {0}")]
    RoomNotFound(String),

    /// Failed to send a message.
    #[error("matrix send error: {0}")]
    SendFailed(String),

    /// Failed to sync with the homeserver.
    #[error("matrix sync error: {0}")]
    SyncFailed(String),

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
    #[error("matrix startup error: {0}")]
    Startup(String),
}
