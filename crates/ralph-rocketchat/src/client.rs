//! Rocket.Chat REST API client and mock implementation.
//!
//! Provides the [`RocketChatApi`] trait for abstracting bot operations, a production
//! [`RocketChatClient`] using `reqwest`, and a [`MockRocketChatClient`] for testing.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use reqwest::StatusCode;
use serde::Deserialize;

use crate::error::{RocketChatError, RocketChatResult};
use crate::types::{RcMessage, RcRoom, RcUser, SyncResult};

/// Trait abstracting Rocket.Chat bot operations for testability.
///
/// Production code uses [`RocketChatClient`]; tests can provide [`MockRocketChatClient`].
/// Mirrors the `BotApi` pattern from `ralph-telegram`.
#[async_trait]
pub trait RocketChatApi: Send + Sync {
    /// Send a text message to the given room.
    ///
    /// When `thread_id` is `Some`, the message is sent as a reply in that thread
    /// (sets the `tmid` field). This is used for multi-loop routing.
    ///
    /// Returns the sent message with its server-assigned ID.
    async fn send_message(
        &self,
        room_id: &str,
        text: &str,
        thread_id: Option<&str>,
    ) -> RocketChatResult<RcMessage>;

    /// Poll for new and deleted messages since `last_update`.
    ///
    /// Uses the `chat.syncMessages` endpoint. The `last_update` parameter
    /// is an ISO 8601 timestamp string from the previous sync.
    async fn sync_messages(&self, room_id: &str, last_update: &str)
    -> RocketChatResult<SyncResult>;

    /// Get the authenticated bot user's profile.
    ///
    /// Uses the `me` endpoint to retrieve the bot's own user info.
    async fn get_me(&self) -> RocketChatResult<RcUser>;

    /// Get information about a room by its ID.
    ///
    /// Works for channels, private groups, and direct messages.
    async fn get_room_info(&self, room_id: &str) -> RocketChatResult<RcRoom>;
}

// ── API response envelope types ─────────────────────────────────────────

/// Wrapper for `chat.sendMessage` response.
#[derive(Debug, Deserialize)]
struct SendMessageResponse {
    message: RcMessage,
}

/// Wrapper for `chat.syncMessages` response.
#[derive(Debug, Deserialize)]
struct SyncMessagesResponse {
    result: SyncResult,
}

/// Wrapper for `channels.info` response.
#[derive(Debug, Deserialize)]
struct ChannelInfoResponse {
    channel: RcRoom,
}

/// Wrapper for `groups.info` response.
#[derive(Debug, Deserialize)]
struct GroupInfoResponse {
    group: RcRoom,
}

/// Wrapper for `dm.info` (im.info) response.
#[derive(Debug, Deserialize)]
struct DmInfoResponse {
    room: RcRoom,
}

// ── RocketChatClient ────────────────────────────────────────────────────

/// Production Rocket.Chat REST API client.
///
/// Authenticates via Personal Access Token (`X-Auth-Token` + `X-User-Id` headers).
pub struct RocketChatClient {
    http: reqwest::Client,
    server_url: String,
    auth_token: String,
    bot_user_id: String,
}

impl RocketChatClient {
    /// Create a new client for the given Rocket.Chat server.
    ///
    /// - `server_url`: Base URL of the server (e.g., `https://chat.example.com`)
    /// - `auth_token`: Personal Access Token for authentication
    /// - `bot_user_id`: The bot's own Rocket.Chat user ID (used for `X-User-Id` header)
    pub fn new(
        server_url: impl Into<String>,
        auth_token: impl Into<String>,
        bot_user_id: impl Into<String>,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            server_url: server_url.into().trim_end_matches('/').to_string(),
            auth_token: auth_token.into(),
            bot_user_id: bot_user_id.into(),
        }
    }

    /// Build a GET request with auth headers to the given API path.
    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.http
            .get(format!("{}/api/v1/{path}", self.server_url))
            .header("X-Auth-Token", &self.auth_token)
            .header("X-User-Id", &self.bot_user_id)
    }

    /// Build a POST request with auth headers to the given API path.
    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.http
            .post(format!("{}/api/v1/{path}", self.server_url))
            .header("X-Auth-Token", &self.auth_token)
            .header("X-User-Id", &self.bot_user_id)
    }
}

/// Map an HTTP status code to the appropriate `RocketChatError`.
fn map_status_error(status: StatusCode, body: &str) -> RocketChatError {
    match status.as_u16() {
        401 => RocketChatError::Auth(format!("HTTP 401: {body}")),
        404 => RocketChatError::NotFound(format!("HTTP 404: {body}")),
        429 => RocketChatError::RateLimit(format!("HTTP 429: {body}")),
        500..=599 => RocketChatError::Server(format!("HTTP {status}: {body}")),
        _ => RocketChatError::Server(format!("unexpected HTTP {status}: {body}")),
    }
}

#[async_trait]
impl RocketChatApi for RocketChatClient {
    async fn send_message(
        &self,
        room_id: &str,
        text: &str,
        thread_id: Option<&str>,
    ) -> RocketChatResult<RcMessage> {
        let mut message = serde_json::json!({
            "rid": room_id,
            "msg": text,
        });
        if let Some(tmid) = thread_id {
            message["tmid"] = serde_json::Value::String(tmid.to_string());
        }
        let body = serde_json::json!({ "message": message });

        let response = self
            .post("chat.sendMessage")
            .json(&body)
            .send()
            .await
            .map_err(|e| RocketChatError::Send(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(map_status_error(status, &text));
        }

        let envelope: SendMessageResponse = response
            .json()
            .await
            .map_err(|e| RocketChatError::Deserialize(e.to_string()))?;

        Ok(envelope.message)
    }

    async fn sync_messages(
        &self,
        room_id: &str,
        last_update: &str,
    ) -> RocketChatResult<SyncResult> {
        let response = self
            .get("chat.syncMessages")
            .query(&[("roomId", room_id), ("lastUpdate", last_update)])
            .send()
            .await
            .map_err(|e| RocketChatError::Receive(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(map_status_error(status, &text));
        }

        let envelope: SyncMessagesResponse = response
            .json()
            .await
            .map_err(|e| RocketChatError::Deserialize(e.to_string()))?;

        Ok(envelope.result)
    }

    async fn get_me(&self) -> RocketChatResult<RcUser> {
        let response = self
            .get("me")
            .send()
            .await
            .map_err(|e| RocketChatError::Receive(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(map_status_error(status, &text));
        }

        response
            .json::<RcUser>()
            .await
            .map_err(|e| RocketChatError::Deserialize(e.to_string()))
    }

    async fn get_room_info(&self, room_id: &str) -> RocketChatResult<RcRoom> {
        // Try channels.info first, then fall back to groups.info, then dm.info.
        let query = &[("roomId", room_id)];

        // Attempt: channels.info
        let response = self
            .get("channels.info")
            .query(query)
            .send()
            .await
            .map_err(|e| RocketChatError::Receive(e.to_string()))?;

        if response.status().is_success()
            && let Ok(envelope) = response.json::<ChannelInfoResponse>().await
        {
            return Ok(envelope.channel);
        }

        // Attempt: groups.info
        let response = self
            .get("groups.info")
            .query(query)
            .send()
            .await
            .map_err(|e| RocketChatError::Receive(e.to_string()))?;

        if response.status().is_success()
            && let Ok(envelope) = response.json::<GroupInfoResponse>().await
        {
            return Ok(envelope.group);
        }

        // Attempt: im.info (DM)
        let response = self
            .get("im.info")
            .query(query)
            .send()
            .await
            .map_err(|e| RocketChatError::Receive(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(map_status_error(status, &text));
        }

        let envelope: DmInfoResponse = response
            .json()
            .await
            .map_err(|e| RocketChatError::Deserialize(e.to_string()))?;

        Ok(envelope.room)
    }
}

// ── MockRocketChatClient ─────────────────────────────────────────────────

/// Recorded call to [`MockRocketChatClient`].
#[derive(Debug, Clone)]
pub enum MockCall {
    /// A `send_message` call with the room ID, text content, and optional thread ID.
    SendMessage {
        room_id: String,
        text: String,
        thread_id: Option<String>,
    },
    /// A `sync_messages` call with the room ID and last-update timestamp.
    SyncMessages {
        room_id: String,
        last_update: String,
    },
    /// A `get_me` call (no parameters).
    GetMe,
    /// A `get_room_info` call with the room ID.
    GetRoomInfo { room_id: String },
}

/// A mock [`RocketChatApi`] implementation for testing.
///
/// Records all calls and returns responses from configurable queues.
/// Declared `pub` (not `#[cfg(test)]`) so that `ralph-cli`, `ralph-e2e`,
/// and other crates can use it in their tests.
pub struct MockRocketChatClient {
    calls: Arc<Mutex<Vec<MockCall>>>,
    send_message_responses: Arc<Mutex<VecDeque<RocketChatResult<RcMessage>>>>,
    sync_messages_responses: Arc<Mutex<VecDeque<RocketChatResult<SyncResult>>>>,
    get_me_responses: Arc<Mutex<VecDeque<RocketChatResult<RcUser>>>>,
    get_room_info_responses: Arc<Mutex<VecDeque<RocketChatResult<RcRoom>>>>,
}

impl MockRocketChatClient {
    /// Create a new mock client with empty call log and no pre-configured responses.
    ///
    /// Methods return sensible defaults when no responses are enqueued:
    /// - `send_message` → an `RcMessage` with the provided text
    /// - `sync_messages` → an empty `SyncResult`
    /// - `get_me` → an `RcUser` with id "bot-user", username "mock-bot"
    /// - `get_room_info` → an `RcRoom` with the requested room_id
    pub fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            send_message_responses: Arc::new(Mutex::new(VecDeque::new())),
            sync_messages_responses: Arc::new(Mutex::new(VecDeque::new())),
            get_me_responses: Arc::new(Mutex::new(VecDeque::new())),
            get_room_info_responses: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// Enqueue a response for the next `send_message` call.
    pub fn on_send_message(&self, response: RocketChatResult<RcMessage>) {
        self.send_message_responses
            .lock()
            .unwrap()
            .push_back(response);
    }

    /// Enqueue a response for the next `sync_messages` call.
    pub fn on_sync_messages(&self, response: RocketChatResult<SyncResult>) {
        self.sync_messages_responses
            .lock()
            .unwrap()
            .push_back(response);
    }

    /// Enqueue a response for the next `get_me` call.
    pub fn on_get_me(&self, response: RocketChatResult<RcUser>) {
        self.get_me_responses.lock().unwrap().push_back(response);
    }

    /// Enqueue a response for the next `get_room_info` call.
    pub fn on_get_room_info(&self, response: RocketChatResult<RcRoom>) {
        self.get_room_info_responses
            .lock()
            .unwrap()
            .push_back(response);
    }

    /// Return a snapshot of all recorded calls.
    pub fn calls(&self) -> Vec<MockCall> {
        self.calls.lock().unwrap().clone()
    }

    /// Return the number of recorded calls.
    pub fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    /// Return a shared reference to the call log.
    ///
    /// Useful when the mock will be moved into a service and you still need
    /// to inspect calls afterward.
    pub fn calls_arc(&self) -> Arc<Mutex<Vec<MockCall>>> {
        self.calls.clone()
    }
}

impl Default for MockRocketChatClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RocketChatApi for MockRocketChatClient {
    async fn send_message(
        &self,
        room_id: &str,
        text: &str,
        thread_id: Option<&str>,
    ) -> RocketChatResult<RcMessage> {
        self.calls.lock().unwrap().push(MockCall::SendMessage {
            room_id: room_id.to_string(),
            text: text.to_string(),
            thread_id: thread_id.map(String::from),
        });

        if let Some(response) = self.send_message_responses.lock().unwrap().pop_front() {
            return response;
        }

        // Default: return a message echoing the input.
        Ok(RcMessage {
            id: format!("msg-{}", self.call_count()),
            rid: room_id.to_string(),
            msg: text.to_string(),
            u: RcUser {
                id: "bot-user".to_string(),
                username: "mock-bot".to_string(),
                name: Some("Mock Bot".to_string()),
            },
            ts: "2026-01-01T00:00:00.000Z".to_string(),
            tmid: thread_id.map(String::from),
            t: None,
        })
    }

    async fn sync_messages(
        &self,
        room_id: &str,
        last_update: &str,
    ) -> RocketChatResult<SyncResult> {
        self.calls.lock().unwrap().push(MockCall::SyncMessages {
            room_id: room_id.to_string(),
            last_update: last_update.to_string(),
        });

        if let Some(response) = self.sync_messages_responses.lock().unwrap().pop_front() {
            return response;
        }

        // Default: no new messages.
        Ok(SyncResult {
            updated: vec![],
            deleted: vec![],
        })
    }

    async fn get_me(&self) -> RocketChatResult<RcUser> {
        self.calls.lock().unwrap().push(MockCall::GetMe);

        if let Some(response) = self.get_me_responses.lock().unwrap().pop_front() {
            return response;
        }

        Ok(RcUser {
            id: "bot-user".to_string(),
            username: "mock-bot".to_string(),
            name: Some("Mock Bot".to_string()),
        })
    }

    async fn get_room_info(&self, room_id: &str) -> RocketChatResult<RcRoom> {
        self.calls.lock().unwrap().push(MockCall::GetRoomInfo {
            room_id: room_id.to_string(),
        });

        if let Some(response) = self.get_room_info_responses.lock().unwrap().pop_front() {
            return response;
        }

        Ok(RcRoom {
            id: room_id.to_string(),
            t: "c".to_string(),
            name: Some("mock-room".to_string()),
            fname: Some("Mock Room".to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── map_status_error tests ──────────────────────────────────────────

    #[test]
    fn map_status_error_401_returns_auth() {
        let err = map_status_error(StatusCode::UNAUTHORIZED, "bad token");
        assert!(matches!(err, RocketChatError::Auth(_)));
        assert!(err.to_string().contains("401"));
    }

    #[test]
    fn map_status_error_404_returns_not_found() {
        let err = map_status_error(StatusCode::NOT_FOUND, "no such room");
        assert!(matches!(err, RocketChatError::NotFound(_)));
        assert!(err.to_string().contains("404"));
    }

    #[test]
    fn map_status_error_429_returns_rate_limit() {
        let err = map_status_error(StatusCode::TOO_MANY_REQUESTS, "slow down");
        assert!(matches!(err, RocketChatError::RateLimit(_)));
        assert!(err.to_string().contains("429"));
    }

    #[test]
    fn map_status_error_500_returns_server() {
        let err = map_status_error(StatusCode::INTERNAL_SERVER_ERROR, "oops");
        assert!(matches!(err, RocketChatError::Server(_)));
        assert!(err.to_string().contains("500"));
    }

    #[test]
    fn map_status_error_502_returns_server() {
        let err = map_status_error(StatusCode::BAD_GATEWAY, "gateway error");
        assert!(matches!(err, RocketChatError::Server(_)));
        assert!(err.to_string().contains("502"));
    }

    #[test]
    fn map_status_error_other_returns_server() {
        let err = map_status_error(StatusCode::BAD_REQUEST, "bad request");
        assert!(matches!(err, RocketChatError::Server(_)));
        assert!(err.to_string().contains("unexpected"));
    }

    // ── MockRocketChatClient tests ──────────────────────────────────────

    #[tokio::test]
    async fn mock_send_message_default_response() {
        let mock = MockRocketChatClient::new();
        let msg = mock.send_message("room-1", "hello", None).await.unwrap();
        assert_eq!(msg.rid, "room-1");
        assert_eq!(msg.msg, "hello");
        assert!(msg.tmid.is_none());
    }

    #[tokio::test]
    async fn mock_send_message_with_thread_id() {
        let mock = MockRocketChatClient::new();
        let msg = mock
            .send_message("room-1", "reply", Some("thread-42"))
            .await
            .unwrap();
        assert_eq!(msg.tmid, Some("thread-42".to_string()));
    }

    #[tokio::test]
    async fn mock_sync_messages_default_empty() {
        let mock = MockRocketChatClient::new();
        let result = mock.sync_messages("room-1", "2026-01-01").await.unwrap();
        assert!(result.updated.is_empty());
        assert!(result.deleted.is_empty());
    }

    #[tokio::test]
    async fn mock_get_me_default_response() {
        let mock = MockRocketChatClient::new();
        let user = mock.get_me().await.unwrap();
        assert_eq!(user.id, "bot-user");
        assert_eq!(user.username, "mock-bot");
    }

    #[tokio::test]
    async fn mock_get_room_info_default_response() {
        let mock = MockRocketChatClient::new();
        let room = mock.get_room_info("room-abc").await.unwrap();
        assert_eq!(room.id, "room-abc");
        assert_eq!(room.t, "c");
    }

    #[tokio::test]
    async fn mock_enqueued_responses_consumed_fifo() {
        let mock = MockRocketChatClient::new();

        let user1 = RcUser {
            id: "first".to_string(),
            username: "first".to_string(),
            name: None,
        };
        let user2 = RcUser {
            id: "second".to_string(),
            username: "second".to_string(),
            name: None,
        };

        mock.on_get_me(Ok(user1));
        mock.on_get_me(Ok(user2));

        let r1 = mock.get_me().await.unwrap();
        assert_eq!(r1.id, "first");

        let r2 = mock.get_me().await.unwrap();
        assert_eq!(r2.id, "second");

        // Third call falls back to default
        let r3 = mock.get_me().await.unwrap();
        assert_eq!(r3.id, "bot-user");
    }

    #[tokio::test]
    async fn mock_enqueued_error_response() {
        let mock = MockRocketChatClient::new();
        mock.on_send_message(Err(RocketChatError::Auth("forbidden".to_string())));

        let result = mock.send_message("room-1", "text", None).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), RocketChatError::Auth(_)));
    }

    #[tokio::test]
    async fn mock_records_calls() {
        let mock = MockRocketChatClient::new();
        let _ = mock.send_message("room-1", "hi", None).await;
        let _ = mock.get_me().await;
        let _ = mock.sync_messages("room-2", "ts").await;
        let _ = mock.get_room_info("room-3").await;

        let calls = mock.calls();
        assert_eq!(calls.len(), 4);

        assert!(
            matches!(&calls[0], MockCall::SendMessage { room_id, text, thread_id }
            if room_id == "room-1" && text == "hi" && thread_id.is_none())
        );
        assert!(matches!(&calls[1], MockCall::GetMe));
        assert!(
            matches!(&calls[2], MockCall::SyncMessages { room_id, last_update }
            if room_id == "room-2" && last_update == "ts")
        );
        assert!(matches!(&calls[3], MockCall::GetRoomInfo { room_id } if room_id == "room-3"));
    }

    #[tokio::test]
    async fn mock_call_count_tracks_correctly() {
        let mock = MockRocketChatClient::new();
        assert_eq!(mock.call_count(), 0);

        let _ = mock.send_message("r", "t", None).await;
        assert_eq!(mock.call_count(), 1);

        let _ = mock.get_me().await;
        assert_eq!(mock.call_count(), 2);

        let _ = mock.sync_messages("r", "ts").await;
        let _ = mock.get_room_info("r").await;
        assert_eq!(mock.call_count(), 4);
    }
}
