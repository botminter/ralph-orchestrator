pub mod client;
pub mod commands;
pub mod daemon;
pub mod error;
pub mod handler;
#[allow(dead_code)]
pub(crate) mod loop_lock;
pub mod service;
pub mod state;
pub mod types;

// Re-export primary types for convenience.
pub use client::{MatrixApi, MatrixClient, MockCall, MockMatrixClient};
pub use daemon::MatrixDaemon;
pub use error::{MatrixError, MatrixResult};
pub use handler::HandleResult;
pub use service::MatrixService;
pub use state::{MatrixState, PendingQuestion, StateManager};
pub use types::{MatrixMessage, RoomInfo, SyncResult, filter_by_operator};
