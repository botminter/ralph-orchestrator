//! Matrix API client abstraction and mock implementation.
//!
//! Provides the [`MatrixApi`] trait for abstracting bot operations, a production
//! [`MatrixClient`] using `matrix-sdk`, and a [`MockMatrixClient`] for testing.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
use matrix_sdk::ruma::{OwnedRoomId, RoomId};
use tracing::{debug, instrument};

use crate::error::{MatrixError, MatrixResult};
use crate::types::{MatrixMessage, RoomInfo, SyncResult};

/// Trait abstracting Matrix bot operations for testability.
///
/// Production code uses [`MatrixClient`]; tests can provide [`MockMatrixClient`].
/// Mirrors the `RocketChatApi` / `BotApi` pattern from other RObot backends.
#[async_trait]
pub trait MatrixApi: Send + Sync {
    /// Authenticate with a homeserver using username and password.
    ///
    /// Creates a new device session. Typically used during onboarding.
    async fn login(&self, homeserver_url: &str, username: &str, password: &str)
    -> MatrixResult<()>;

    /// Authenticate with a homeserver using a pre-obtained access token.
    ///
    /// Restores a session without password. Used for stored credentials.
    async fn login_with_token(&self, homeserver_url: &str, access_token: &str) -> MatrixResult<()>;

    /// Send a text message to a room.
    ///
    /// When `reply_to_event_id` is `Some`, the message is sent as a reply
    /// (sets `m.relates_to` in the event). This is used for multi-loop routing.
    ///
    /// Returns the event ID of the sent message.
    async fn send_message(
        &self,
        room_id: &str,
        body: &str,
        reply_to_event_id: Option<&str>,
    ) -> MatrixResult<String>;

    /// Perform a single incremental sync with the homeserver.
    ///
    /// Returns new messages received since the last sync. The `timeout`
    /// controls how long the server should wait before returning if no
    /// new events are available (long-polling).
    async fn sync_once(&self, timeout: Duration) -> MatrixResult<SyncResult>;

    /// Get the bot's display name from the homeserver.
    ///
    /// Used for identity verification (analogous to Telegram's `getMe`
    /// or Rocket.Chat's `me` endpoint).
    async fn get_display_name(&self) -> MatrixResult<String>;

    /// Get information about a room.
    ///
    /// Returns room metadata including name, topic, and member count.
    async fn get_room_info(&self, room_id: &str) -> MatrixResult<RoomInfo>;

    /// Join a room by ID or alias.
    ///
    /// Returns the canonical room ID of the joined room.
    async fn join_room(&self, room_id_or_alias: &str) -> MatrixResult<OwnedRoomId>;
}

// ── MatrixClient ────────────────────────────────────────────────────────

/// Production Matrix API client wrapping `matrix-sdk::Client`.
///
/// Uses `matrix-sdk`'s built-in sync, login, and typed event handling.
/// Constructed via [`MatrixClient::new`]; authentication must be performed
/// before calling other methods.
pub struct MatrixClient {
    /// The inner matrix-sdk client, set after login.
    client: tokio::sync::RwLock<Option<matrix_sdk::Client>>,
}

impl MatrixClient {
    /// Create a new, unauthenticated Matrix client.
    ///
    /// Call [`login`](MatrixApi::login) or [`login_with_token`](MatrixApi::login_with_token)
    /// before using other methods.
    pub fn new() -> Self {
        Self {
            client: tokio::sync::RwLock::new(None),
        }
    }

    /// Get a reference to the inner client, returning an error if not logged in.
    async fn inner(&self) -> MatrixResult<matrix_sdk::Client> {
        self.client
            .read()
            .await
            .clone()
            .ok_or_else(|| MatrixError::Auth("not logged in".to_string()))
    }

    /// Perform an initial sync and ensure the bot has joined the given room.
    ///
    /// Must be called after [`login`](MatrixApi::login) or
    /// [`login_with_token`](MatrixApi::login_with_token) and before any
    /// operations that require room state (`get_room_info`, `send_message`).
    ///
    /// The SDK's local room cache is empty after login — `sync_once` populates
    /// it. `join_room` is then called as a safety net: it's a no-op if already
    /// joined, but accepts a pending invite if the bot was only invited.
    pub async fn ensure_room(&self, room_id: &str) -> MatrixResult<()> {
        self.sync_once(Duration::from_secs(5)).await?;
        self.join_room(room_id).await?;
        Ok(())
    }
}

impl Default for MatrixClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MatrixApi for MatrixClient {
    #[instrument(skip(self, password))]
    async fn login(
        &self,
        homeserver_url: &str,
        username: &str,
        password: &str,
    ) -> MatrixResult<()> {
        let client = matrix_sdk::Client::builder()
            .homeserver_url(homeserver_url)
            .build()
            .await
            .map_err(|e| MatrixError::Network(e.to_string()))?;

        client
            .matrix_auth()
            .login_username(username, password)
            .initial_device_display_name("Ralph Bot")
            .await
            .map_err(|e| MatrixError::Auth(e.to_string()))?;

        debug!("logged in as {username}");
        *self.client.write().await = Some(client);
        Ok(())
    }

    #[instrument(skip(self, access_token))]
    async fn login_with_token(&self, homeserver_url: &str, access_token: &str) -> MatrixResult<()> {
        let client = matrix_sdk::Client::builder()
            .homeserver_url(homeserver_url)
            .build()
            .await
            .map_err(|e| MatrixError::Network(e.to_string()))?;

        // Restore session using the access token.
        // We need user_id and device_id from a whoami call, but the SDK
        // requires a session to be set first. Use the low-level auth API.
        let session = matrix_sdk::authentication::matrix::MatrixSession {
            meta: matrix_sdk::SessionMeta {
                user_id: matrix_sdk::ruma::OwnedUserId::try_from("@placeholder:localhost").unwrap(),
                device_id: matrix_sdk::ruma::OwnedDeviceId::from("RALPH"),
            },
            tokens: matrix_sdk::SessionTokens {
                access_token: access_token.to_string(),
                refresh_token: None,
            },
        };

        client
            .matrix_auth()
            .restore_session(session, matrix_sdk::store::RoomLoadSettings::default())
            .await
            .map_err(|e| MatrixError::Auth(e.to_string()))?;

        // Verify the token by calling whoami
        let whoami = client
            .whoami()
            .await
            .map_err(|e| MatrixError::Auth(format!("token validation failed: {e}")))?;

        debug!(user_id = %whoami.user_id, "authenticated with token");

        // Re-restore with the correct user_id from whoami
        let correct_session = matrix_sdk::authentication::matrix::MatrixSession {
            meta: matrix_sdk::SessionMeta {
                user_id: whoami.user_id,
                device_id: whoami
                    .device_id
                    .unwrap_or_else(|| matrix_sdk::ruma::OwnedDeviceId::from("RALPH")),
            },
            tokens: matrix_sdk::SessionTokens {
                access_token: access_token.to_string(),
                refresh_token: None,
            },
        };

        // Build a fresh client with correct session
        let client = matrix_sdk::Client::builder()
            .homeserver_url(homeserver_url)
            .build()
            .await
            .map_err(|e| MatrixError::Network(e.to_string()))?;

        client
            .matrix_auth()
            .restore_session(
                correct_session,
                matrix_sdk::store::RoomLoadSettings::default(),
            )
            .await
            .map_err(|e| MatrixError::Auth(e.to_string()))?;

        *self.client.write().await = Some(client);
        Ok(())
    }

    #[instrument(skip(self, body))]
    async fn send_message(
        &self,
        room_id: &str,
        body: &str,
        reply_to_event_id: Option<&str>,
    ) -> MatrixResult<String> {
        let client = self.inner().await?;
        let room_id = <&RoomId>::try_from(room_id)
            .map_err(|e| MatrixError::RoomNotFound(format!("invalid room ID: {e}")))?;

        let room = client
            .get_room(room_id)
            .ok_or_else(|| MatrixError::RoomNotFound(format!("room {room_id} not found")))?;

        let content = RoomMessageEventContent::text_plain(body);

        // Note: For reply threading, we use the basic text content.
        // Full reply formatting (with quoted fallback) would require fetching
        // the original event. For bot messages, plain send with m.relates_to
        // is sufficient for routing purposes.
        let response = if let Some(reply_to) = reply_to_event_id {
            // Send as raw JSON to include m.relates_to for reply routing
            let event_id = matrix_sdk::ruma::OwnedEventId::try_from(reply_to)
                .map_err(|e| MatrixError::SendFailed(format!("invalid reply event ID: {e}")))?;
            let mut json_content = serde_json::to_value(&content)
                .map_err(|e| MatrixError::SendFailed(e.to_string()))?;
            json_content["m.relates_to"] = serde_json::json!({
                "m.in_reply_to": {
                    "event_id": event_id.as_str()
                }
            });
            room.send_raw("m.room.message", json_content)
                .await
                .map_err(|e| MatrixError::SendFailed(e.to_string()))?
        } else {
            room.send(content)
                .await
                .map_err(|e| MatrixError::SendFailed(e.to_string()))?
        };

        Ok(response.event_id.to_string())
    }

    #[instrument(skip(self))]
    async fn sync_once(&self, timeout: Duration) -> MatrixResult<SyncResult> {
        let client = self.inner().await?;

        let settings = SyncSettings::default().timeout(timeout);
        let response = client
            .sync_once(settings)
            .await
            .map_err(|e| MatrixError::SyncFailed(e.to_string()))?;

        let mut messages = Vec::new();

        for (room_id, room_update) in &response.rooms.joined {
            for event in &room_update.timeline.events {
                // Try to deserialize as a room message event
                if let Ok(any_event) = event.raw().deserialize() {
                    use matrix_sdk::ruma::events::AnySyncTimelineEvent;
                    if let AnySyncTimelineEvent::MessageLike(
                        matrix_sdk::ruma::events::AnySyncMessageLikeEvent::RoomMessage(msg_event),
                    ) = any_event
                    {
                        let original = match msg_event {
                            matrix_sdk::ruma::events::SyncMessageLikeEvent::Original(orig) => orig,
                            matrix_sdk::ruma::events::SyncMessageLikeEvent::Redacted(_) => {
                                continue;
                            }
                        };

                        let body = match &original.content.msgtype {
                            matrix_sdk::ruma::events::room::message::MessageType::Text(text) => {
                                text.body.clone()
                            }
                            _ => continue,
                        };

                        // Extract reply_to event ID if present
                        let reply_to_event_id =
                            original.content.relates_to.as_ref().and_then(|rel| {
                                if let matrix_sdk::ruma::events::room::message::Relation::Reply {
                                    in_reply_to,
                                } = rel
                                {
                                    Some(in_reply_to.event_id.to_string())
                                } else {
                                    None
                                }
                            });

                        let timestamp = original.origin_server_ts.0.into();

                        messages.push(MatrixMessage {
                            sender_id: original.sender.to_string(),
                            body,
                            event_id: original.event_id.to_string(),
                            timestamp,
                            room_id: room_id.to_string(),
                            reply_to_event_id,
                        });
                    }
                }
            }
        }

        Ok(SyncResult { messages })
    }

    async fn get_display_name(&self) -> MatrixResult<String> {
        let client = self.inner().await?;
        let name = client
            .account()
            .get_display_name()
            .await
            .map_err(|e| MatrixError::Network(e.to_string()))?;

        Ok(name.unwrap_or_else(|| {
            client
                .user_id()
                .map(|id| id.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        }))
    }

    async fn get_room_info(&self, room_id: &str) -> MatrixResult<RoomInfo> {
        let client = self.inner().await?;
        let room_id = <&RoomId>::try_from(room_id)
            .map_err(|e| MatrixError::RoomNotFound(format!("invalid room ID: {e}")))?;

        let room = client
            .get_room(room_id)
            .ok_or_else(|| MatrixError::RoomNotFound(format!("room {room_id} not found")))?;

        let name = room.cached_display_name().map(|n| n.to_string());
        let topic = room.topic();
        let member_count = room.joined_members_count();

        Ok(RoomInfo {
            room_id: room_id.to_string(),
            name,
            topic,
            member_count,
        })
    }

    #[instrument(skip(self))]
    async fn join_room(&self, room_id_or_alias: &str) -> MatrixResult<OwnedRoomId> {
        let client = self.inner().await?;

        // Try parsing as a room ID first, then as an alias
        if let Ok(room_id) = <&RoomId>::try_from(room_id_or_alias) {
            let room = client
                .join_room_by_id(room_id)
                .await
                .map_err(|e| MatrixError::RoomNotFound(format!("failed to join room: {e}")))?;
            Ok(room.room_id().to_owned())
        } else {
            // Try as alias
            let alias =
                matrix_sdk::ruma::OwnedRoomOrAliasId::try_from(room_id_or_alias.to_string())
                    .map_err(|e| {
                        MatrixError::RoomNotFound(format!("invalid room ID or alias: {e}"))
                    })?;
            let room = client
                .join_room_by_id_or_alias(alias.as_ref(), &[])
                .await
                .map_err(|e| MatrixError::RoomNotFound(format!("failed to join room: {e}")))?;
            Ok(room.room_id().to_owned())
        }
    }
}

// ── MockMatrixClient ────────────────────────────────────────────────────

/// Recorded call to [`MockMatrixClient`].
#[derive(Debug, Clone)]
pub enum MockCall {
    /// A `login` call with homeserver URL and username.
    Login {
        homeserver_url: String,
        username: String,
    },
    /// A `login_with_token` call with homeserver URL.
    LoginWithToken { homeserver_url: String },
    /// A `send_message` call with room ID, text, and optional reply-to event ID.
    SendMessage {
        room_id: String,
        body: String,
        reply_to_event_id: Option<String>,
    },
    /// A `sync_once` call with the timeout duration.
    SyncOnce { timeout: Duration },
    /// A `get_display_name` call (no parameters).
    GetDisplayName,
    /// A `get_room_info` call with the room ID.
    GetRoomInfo { room_id: String },
    /// A `join_room` call with the room ID or alias.
    JoinRoom { room_id_or_alias: String },
}

/// A mock [`MatrixApi`] implementation for testing.
///
/// Records all calls and returns responses from configurable queues.
/// Declared `pub` (not `#[cfg(test)]`) so that `ralph-cli`, `ralph-e2e`,
/// and other crates can use it in their tests.
pub struct MockMatrixClient {
    calls: Arc<Mutex<Vec<MockCall>>>,
    login_responses: Arc<Mutex<VecDeque<MatrixResult<()>>>>,
    login_with_token_responses: Arc<Mutex<VecDeque<MatrixResult<()>>>>,
    send_message_responses: Arc<Mutex<VecDeque<MatrixResult<String>>>>,
    sync_once_responses: Arc<Mutex<VecDeque<MatrixResult<SyncResult>>>>,
    get_display_name_responses: Arc<Mutex<VecDeque<MatrixResult<String>>>>,
    get_room_info_responses: Arc<Mutex<VecDeque<MatrixResult<RoomInfo>>>>,
    join_room_responses: Arc<Mutex<VecDeque<MatrixResult<OwnedRoomId>>>>,
}

impl MockMatrixClient {
    /// Create a new mock client with empty call log and no pre-configured responses.
    ///
    /// Methods return sensible defaults when no responses are enqueued:
    /// - `login` / `login_with_token` → `Ok(())`
    /// - `send_message` → `Ok("$mock-event-id")` with incrementing counter
    /// - `sync_once` → `Ok(SyncResult { messages: vec![] })`
    /// - `get_display_name` → `Ok("Mock Bot")`
    /// - `get_room_info` → `Ok(RoomInfo { ... })` with the requested room_id
    /// - `join_room` → `Ok(OwnedRoomId)` with a default room ID
    pub fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            login_responses: Arc::new(Mutex::new(VecDeque::new())),
            login_with_token_responses: Arc::new(Mutex::new(VecDeque::new())),
            send_message_responses: Arc::new(Mutex::new(VecDeque::new())),
            sync_once_responses: Arc::new(Mutex::new(VecDeque::new())),
            get_display_name_responses: Arc::new(Mutex::new(VecDeque::new())),
            get_room_info_responses: Arc::new(Mutex::new(VecDeque::new())),
            join_room_responses: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// Enqueue a response for the next `login` call.
    pub fn on_login(&self, response: MatrixResult<()>) {
        self.login_responses.lock().unwrap().push_back(response);
    }

    /// Enqueue a response for the next `login_with_token` call.
    pub fn on_login_with_token(&self, response: MatrixResult<()>) {
        self.login_with_token_responses
            .lock()
            .unwrap()
            .push_back(response);
    }

    /// Enqueue a response for the next `send_message` call.
    pub fn on_send_message(&self, response: MatrixResult<String>) {
        self.send_message_responses
            .lock()
            .unwrap()
            .push_back(response);
    }

    /// Enqueue a response for the next `sync_once` call.
    pub fn on_sync_once(&self, response: MatrixResult<SyncResult>) {
        self.sync_once_responses.lock().unwrap().push_back(response);
    }

    /// Enqueue a response for the next `get_display_name` call.
    pub fn on_get_display_name(&self, response: MatrixResult<String>) {
        self.get_display_name_responses
            .lock()
            .unwrap()
            .push_back(response);
    }

    /// Enqueue a response for the next `get_room_info` call.
    pub fn on_get_room_info(&self, response: MatrixResult<RoomInfo>) {
        self.get_room_info_responses
            .lock()
            .unwrap()
            .push_back(response);
    }

    /// Enqueue a response for the next `join_room` call.
    pub fn on_join_room(&self, response: MatrixResult<OwnedRoomId>) {
        self.join_room_responses.lock().unwrap().push_back(response);
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

impl Default for MockMatrixClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MatrixApi for MockMatrixClient {
    async fn login(
        &self,
        homeserver_url: &str,
        username: &str,
        _password: &str,
    ) -> MatrixResult<()> {
        self.calls.lock().unwrap().push(MockCall::Login {
            homeserver_url: homeserver_url.to_string(),
            username: username.to_string(),
        });

        if let Some(response) = self.login_responses.lock().unwrap().pop_front() {
            return response;
        }

        Ok(())
    }

    async fn login_with_token(
        &self,
        homeserver_url: &str,
        _access_token: &str,
    ) -> MatrixResult<()> {
        self.calls.lock().unwrap().push(MockCall::LoginWithToken {
            homeserver_url: homeserver_url.to_string(),
        });

        if let Some(response) = self.login_with_token_responses.lock().unwrap().pop_front() {
            return response;
        }

        Ok(())
    }

    async fn send_message(
        &self,
        room_id: &str,
        body: &str,
        reply_to_event_id: Option<&str>,
    ) -> MatrixResult<String> {
        self.calls.lock().unwrap().push(MockCall::SendMessage {
            room_id: room_id.to_string(),
            body: body.to_string(),
            reply_to_event_id: reply_to_event_id.map(String::from),
        });

        if let Some(response) = self.send_message_responses.lock().unwrap().pop_front() {
            return response;
        }

        // Default: return a mock event ID with counter
        Ok(format!("$mock-event-{}", self.call_count()))
    }

    async fn sync_once(&self, timeout: Duration) -> MatrixResult<SyncResult> {
        self.calls
            .lock()
            .unwrap()
            .push(MockCall::SyncOnce { timeout });

        if let Some(response) = self.sync_once_responses.lock().unwrap().pop_front() {
            return response;
        }

        // Default: no new messages
        Ok(SyncResult { messages: vec![] })
    }

    async fn get_display_name(&self) -> MatrixResult<String> {
        self.calls.lock().unwrap().push(MockCall::GetDisplayName);

        if let Some(response) = self.get_display_name_responses.lock().unwrap().pop_front() {
            return response;
        }

        Ok("Mock Bot".to_string())
    }

    async fn get_room_info(&self, room_id: &str) -> MatrixResult<RoomInfo> {
        self.calls.lock().unwrap().push(MockCall::GetRoomInfo {
            room_id: room_id.to_string(),
        });

        if let Some(response) = self.get_room_info_responses.lock().unwrap().pop_front() {
            return response;
        }

        Ok(RoomInfo {
            room_id: room_id.to_string(),
            name: Some("mock-room".to_string()),
            topic: None,
            member_count: 2,
        })
    }

    async fn join_room(&self, room_id_or_alias: &str) -> MatrixResult<OwnedRoomId> {
        self.calls.lock().unwrap().push(MockCall::JoinRoom {
            room_id_or_alias: room_id_or_alias.to_string(),
        });

        if let Some(response) = self.join_room_responses.lock().unwrap().pop_front() {
            return response;
        }

        Ok(OwnedRoomId::try_from("!mock-room:localhost").unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── MockMatrixClient tests ──────────────────────────────────────────

    #[tokio::test]
    async fn mock_login_default_success() {
        let mock = MockMatrixClient::new();
        let result = mock
            .login("https://matrix.example.com", "bot", "pass")
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn mock_login_with_token_default_success() {
        let mock = MockMatrixClient::new();
        let result = mock
            .login_with_token("https://matrix.example.com", "syt_token")
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn mock_send_message_default_returns_event_id() {
        let mock = MockMatrixClient::new();
        let event_id = mock
            .send_message("!room:example.com", "hello", None)
            .await
            .unwrap();
        assert!(event_id.starts_with("$mock-event-"));
    }

    #[tokio::test]
    async fn mock_send_message_with_reply_to() {
        let mock = MockMatrixClient::new();
        let event_id = mock
            .send_message("!room:example.com", "reply", Some("$original-event"))
            .await
            .unwrap();
        assert!(event_id.starts_with("$mock-event-"));

        let calls = mock.calls();
        assert!(
            matches!(&calls[0], MockCall::SendMessage { reply_to_event_id, .. }
            if *reply_to_event_id == Some("$original-event".to_string()))
        );
    }

    #[tokio::test]
    async fn mock_sync_once_default_empty() {
        let mock = MockMatrixClient::new();
        let result = mock.sync_once(Duration::from_secs(30)).await.unwrap();
        assert!(result.messages.is_empty());
    }

    #[tokio::test]
    async fn mock_get_display_name_default() {
        let mock = MockMatrixClient::new();
        let name = mock.get_display_name().await.unwrap();
        assert_eq!(name, "Mock Bot");
    }

    #[tokio::test]
    async fn mock_get_room_info_default() {
        let mock = MockMatrixClient::new();
        let info = mock.get_room_info("!room:example.com").await.unwrap();
        assert_eq!(info.room_id, "!room:example.com");
        assert_eq!(info.name, Some("mock-room".to_string()));
        assert_eq!(info.member_count, 2);
    }

    #[tokio::test]
    async fn mock_join_room_default() {
        let mock = MockMatrixClient::new();
        let room_id = mock.join_room("!room:example.com").await.unwrap();
        assert_eq!(room_id.as_str(), "!mock-room:localhost");
    }

    #[tokio::test]
    async fn mock_enqueued_responses_consumed_fifo() {
        let mock = MockMatrixClient::new();

        mock.on_get_display_name(Ok("First Name".to_string()));
        mock.on_get_display_name(Ok("Second Name".to_string()));

        let r1 = mock.get_display_name().await.unwrap();
        assert_eq!(r1, "First Name");

        let r2 = mock.get_display_name().await.unwrap();
        assert_eq!(r2, "Second Name");

        // Third call falls back to default
        let r3 = mock.get_display_name().await.unwrap();
        assert_eq!(r3, "Mock Bot");
    }

    #[tokio::test]
    async fn mock_enqueued_error_response() {
        let mock = MockMatrixClient::new();
        mock.on_send_message(Err(MatrixError::SendFailed("test error".to_string())));

        let result = mock.send_message("!room:example.com", "text", None).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), MatrixError::SendFailed(_)));
    }

    #[tokio::test]
    async fn mock_enqueued_sync_with_messages() {
        let mock = MockMatrixClient::new();
        let sync_result = SyncResult {
            messages: vec![MatrixMessage {
                sender_id: "@alice:example.com".to_string(),
                body: "hello from alice".to_string(),
                event_id: "$evt-1".to_string(),
                timestamp: 1_700_000_000_000,
                room_id: "!room:example.com".to_string(),
                reply_to_event_id: None,
            }],
        };
        mock.on_sync_once(Ok(sync_result));

        let result = mock.sync_once(Duration::from_secs(10)).await.unwrap();
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0].sender_id, "@alice:example.com");
        assert_eq!(result.messages[0].body, "hello from alice");
    }

    #[tokio::test]
    async fn mock_records_calls() {
        let mock = MockMatrixClient::new();
        let _ = mock.login("https://hs.example.com", "bot", "pass").await;
        let _ = mock.send_message("!room:example.com", "hi", None).await;
        let _ = mock.get_display_name().await;
        let _ = mock.sync_once(Duration::from_secs(30)).await;
        let _ = mock.get_room_info("!room:example.com").await;
        let _ = mock.join_room("#general:example.com").await;

        let calls = mock.calls();
        assert_eq!(calls.len(), 6);

        assert!(matches!(
            &calls[0],
            MockCall::Login {
                homeserver_url,
                username
            } if homeserver_url == "https://hs.example.com" && username == "bot"
        ));
        assert!(matches!(
            &calls[1],
            MockCall::SendMessage {
                room_id,
                body,
                reply_to_event_id,
            } if room_id == "!room:example.com" && body == "hi" && reply_to_event_id.is_none()
        ));
        assert!(matches!(&calls[2], MockCall::GetDisplayName));
        assert!(matches!(
            &calls[3],
            MockCall::SyncOnce { timeout } if *timeout == Duration::from_secs(30)
        ));
        assert!(matches!(
            &calls[4],
            MockCall::GetRoomInfo { room_id } if room_id == "!room:example.com"
        ));
        assert!(matches!(
            &calls[5],
            MockCall::JoinRoom { room_id_or_alias } if room_id_or_alias == "#general:example.com"
        ));
    }

    #[tokio::test]
    async fn mock_call_count_tracks_correctly() {
        let mock = MockMatrixClient::new();
        assert_eq!(mock.call_count(), 0);

        let _ = mock.send_message("!r:example.com", "t", None).await;
        assert_eq!(mock.call_count(), 1);

        let _ = mock.get_display_name().await;
        assert_eq!(mock.call_count(), 2);

        let _ = mock.sync_once(Duration::from_secs(5)).await;
        let _ = mock.get_room_info("!r:example.com").await;
        assert_eq!(mock.call_count(), 4);
    }

    #[tokio::test]
    async fn mock_calls_arc_shared_access() {
        let mock = MockMatrixClient::new();
        let calls_arc = mock.calls_arc();

        let _ = mock.login("https://hs.example.com", "bot", "pw").await;

        let calls = calls_arc.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(matches!(&calls[0], MockCall::Login { .. }));
    }

    #[tokio::test]
    async fn mock_login_enqueued_error() {
        let mock = MockMatrixClient::new();
        mock.on_login(Err(MatrixError::Auth("bad creds".to_string())));

        let result = mock.login("https://hs.example.com", "bot", "pass").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), MatrixError::Auth(_)));
    }

    #[tokio::test]
    async fn mock_as_dyn_trait() {
        // Verify MockMatrixClient can be used as Box<dyn MatrixApi>
        let mock = MockMatrixClient::new();
        let api: Box<dyn MatrixApi> = Box::new(mock);
        let name = api.get_display_name().await.unwrap();
        assert_eq!(name, "Mock Bot");
    }
}
