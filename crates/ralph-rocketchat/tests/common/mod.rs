//! Mock Rocket.Chat HTTP server for integration testing.
//!
//! Provides an axum-based server that simulates the Rocket.Chat REST API,
//! allowing integration tests to exercise `RocketChatClient` against real HTTP
//! without needing a live Rocket.Chat instance.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use ralph_rocketchat::types::{RcMessage, RcRoom, RcUser, SyncResult};
use serde::Deserialize;
use tokio::net::TcpListener;

// ── Constants ────────────────────────────────────────────────────────────

const VALID_AUTH_TOKEN: &str = "test-auth-token";
const VALID_USER_ID: &str = "bot-user-id";

// ── Shared server state ─────────────────────────────────────────────────

#[derive(Debug, Default)]
struct ServerState {
    /// Messages injected via [`MockRocketChatServer::inject_message`] that will
    /// be returned by the `chat.syncMessages` endpoint.
    injected_messages: Vec<RcMessage>,

    /// Messages received via `chat.sendMessage`, available for inspection.
    sent_messages: Vec<RcMessage>,

    /// Auto-incrementing counter for generating unique message IDs.
    next_msg_id: u64,
}

// ── MockRocketChatServer ─────────────────────────────────────────────────

/// An axum-based mock HTTP server that simulates Rocket.Chat REST API endpoints.
///
/// # Usage
///
/// ```ignore
/// let server = MockRocketChatServer::start().await;
/// let client = RocketChatClient::new(server.url(), VALID_AUTH_TOKEN, VALID_USER_ID);
/// // ... use client normally ...
/// let sent = server.sent_messages();
/// ```
pub struct MockRocketChatServer {
    state: Arc<Mutex<ServerState>>,
    addr: SocketAddr,
}

impl MockRocketChatServer {
    /// Start the mock server on a random available port.
    ///
    /// The server runs in a background tokio task and will be dropped when all
    /// references to it are released (the task detects the server handle is gone).
    pub async fn start() -> Self {
        let state = Arc::new(Mutex::new(ServerState::default()));
        let app = build_router(state.clone());

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("failed to bind mock server");
        let addr = listener.local_addr().expect("failed to get local address");

        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock server crashed");
        });

        Self { state, addr }
    }

    /// The base URL of the mock server (e.g., `http://127.0.0.1:12345`).
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// The valid auth token that the mock server accepts.
    pub fn auth_token(&self) -> &'static str {
        VALID_AUTH_TOKEN
    }

    /// The valid user ID that the mock server accepts.
    pub fn user_id(&self) -> &'static str {
        VALID_USER_ID
    }

    /// Inject a message that will be returned by `chat.syncMessages`.
    pub fn inject_message(&self, message: RcMessage) {
        self.state.lock().unwrap().injected_messages.push(message);
    }

    /// Return a snapshot of all messages sent via `chat.sendMessage`.
    pub fn sent_messages(&self) -> Vec<RcMessage> {
        self.state.lock().unwrap().sent_messages.clone()
    }
}

// ── Router ───────────────────────────────────────────────────────────────

fn build_router(state: Arc<Mutex<ServerState>>) -> Router {
    Router::new()
        .route("/api/v1/chat.sendMessage", post(handle_send_message))
        .route("/api/v1/chat.syncMessages", get(handle_sync_messages))
        .route("/api/v1/me", get(handle_me))
        .route("/api/v1/channels.info", get(handle_channels_info))
        .with_state(state)
}

// ── Auth validation ──────────────────────────────────────────────────────

/// Validate `X-Auth-Token` and `X-User-Id` headers.
/// Returns `Err(401)` if invalid.
fn validate_auth(headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
    let token = headers
        .get("X-Auth-Token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let user_id = headers
        .get("X-User-Id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if token != VALID_AUTH_TOKEN || user_id != VALID_USER_ID {
        return Err((
            StatusCode::UNAUTHORIZED,
            serde_json::json!({
                "status": "error",
                "message": "You must be logged in to do this."
            })
            .to_string(),
        ));
    }

    Ok(())
}

// ── Handlers ─────────────────────────────────────────────────────────────

/// POST /api/v1/chat.sendMessage
///
/// Expects: `{ "message": { "rid": "...", "msg": "...", "tmid"?: "..." } }`
/// Returns: `{ "message": { "_id": "...", ... } }`
async fn handle_send_message(
    State(state): State<Arc<Mutex<ServerState>>>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Err(e) = validate_auth(&headers) {
        return e.into_response();
    }

    let message_obj = match body.get("message") {
        Some(m) => m,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                r#"{"status":"error","message":"missing message field"}"#.to_string(),
            )
                .into_response();
        }
    };

    let rid = message_obj
        .get("rid")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown-room");
    let msg = message_obj
        .get("msg")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tmid = message_obj
        .get("tmid")
        .and_then(|v| v.as_str())
        .map(String::from);

    let mut st = state.lock().unwrap();
    st.next_msg_id += 1;
    let msg_id = format!("mock-msg-{}", st.next_msg_id);

    let sent_msg = RcMessage {
        id: msg_id,
        rid: rid.to_string(),
        msg: msg.to_string(),
        u: RcUser {
            id: VALID_USER_ID.to_string(),
            username: "ralph-bot".to_string(),
            name: Some("Ralph Bot".to_string()),
        },
        ts: "2026-03-08T12:00:00.000Z".to_string(),
        tmid,
        t: None,
    };

    st.sent_messages.push(sent_msg.clone());

    // Wrap in `{ "message": ... }` envelope as Rocket.Chat does.
    let response = serde_json::json!({ "message": sent_msg });
    Json(response).into_response()
}

/// Query parameters for `chat.syncMessages`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncQuery {
    #[allow(dead_code)]
    room_id: String,
    #[allow(dead_code)]
    last_update: String,
}

/// GET /api/v1/chat.syncMessages?roomId=...&lastUpdate=...
///
/// Returns all injected messages and drains them.
async fn handle_sync_messages(
    State(state): State<Arc<Mutex<ServerState>>>,
    headers: HeaderMap,
    Query(_query): Query<SyncQuery>,
) -> impl IntoResponse {
    if let Err(e) = validate_auth(&headers) {
        return e.into_response();
    }

    let mut st = state.lock().unwrap();
    let messages = std::mem::take(&mut st.injected_messages);

    let result = SyncResult {
        updated: messages,
        deleted: vec![],
    };

    let response = serde_json::json!({ "result": result });
    Json(response).into_response()
}

/// GET /api/v1/me
///
/// Returns the bot user info (not wrapped in an envelope).
async fn handle_me(
    State(_state): State<Arc<Mutex<ServerState>>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = validate_auth(&headers) {
        return e.into_response();
    }

    let user = RcUser {
        id: VALID_USER_ID.to_string(),
        username: "ralph-bot".to_string(),
        name: Some("Ralph Bot".to_string()),
    };

    Json(user).into_response()
}

/// Query parameters for `channels.info`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChannelInfoQuery {
    room_id: String,
}

/// GET /api/v1/channels.info?roomId=...
///
/// Returns: `{ "channel": { "_id": "...", ... } }`
async fn handle_channels_info(
    State(_state): State<Arc<Mutex<ServerState>>>,
    headers: HeaderMap,
    Query(query): Query<ChannelInfoQuery>,
) -> impl IntoResponse {
    if let Err(e) = validate_auth(&headers) {
        return e.into_response();
    }

    let room = RcRoom {
        id: query.room_id,
        t: "c".to_string(),
        name: Some("general".to_string()),
        fname: Some("General".to_string()),
    };

    let response = serde_json::json!({ "channel": room });
    Json(response).into_response()
}
