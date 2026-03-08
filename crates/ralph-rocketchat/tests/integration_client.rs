//! Integration tests for [`RocketChatClient`] against [`MockRocketChatServer`].
//!
//! These tests exercise the production HTTP client through real HTTP requests
//! to an axum-based mock server, validating serialization, auth headers, and
//! response parsing end-to-end.

mod common;

use common::MockRocketChatServer;
use ralph_rocketchat::client::{RocketChatApi, RocketChatClient};
use ralph_rocketchat::error::RocketChatError;
use ralph_rocketchat::types::{RcMessage, RcUser};

/// Helper: create a `RocketChatClient` connected to the mock server with valid credentials.
fn client_for(server: &MockRocketChatServer) -> RocketChatClient {
    RocketChatClient::new(server.url(), server.auth_token(), server.user_id())
}

// ── send_message ────────────────────────────────────────────────────────

#[tokio::test]
async fn send_message_success() {
    let server = MockRocketChatServer::start().await;
    let client = client_for(&server);

    let msg = client
        .send_message("room-1", "hello world", None)
        .await
        .expect("send_message should succeed");

    assert_eq!(msg.rid, "room-1");
    assert_eq!(msg.msg, "hello world");
    assert!(msg.tmid.is_none());

    // Verify the server recorded the sent message.
    let sent = server.sent_messages();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].rid, "room-1");
    assert_eq!(sent[0].msg, "hello world");
}

#[tokio::test]
async fn send_message_with_thread_id() {
    let server = MockRocketChatServer::start().await;
    let client = client_for(&server);

    let msg = client
        .send_message("room-1", "threaded reply", Some("thread-42"))
        .await
        .expect("send_message with thread should succeed");

    assert_eq!(msg.rid, "room-1");
    assert_eq!(msg.msg, "threaded reply");
    assert_eq!(msg.tmid, Some("thread-42".to_string()));

    let sent = server.sent_messages();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].tmid, Some("thread-42".to_string()));
}

// ── sync_messages ───────────────────────────────────────────────────────

#[tokio::test]
async fn sync_messages_returns_injected_messages() {
    let server = MockRocketChatServer::start().await;
    let client = client_for(&server);

    // Inject a message into the mock server.
    server.inject_message(RcMessage {
        id: "injected-1".to_string(),
        rid: "room-1".to_string(),
        msg: "hello from operator".to_string(),
        u: RcUser {
            id: "operator-1".to_string(),
            username: "alice".to_string(),
            name: Some("Alice".to_string()),
        },
        ts: "2026-03-08T12:00:00.000Z".to_string(),
        tmid: None,
        t: None,
    });

    let result = client
        .sync_messages("room-1", "2026-03-08T00:00:00.000Z")
        .await
        .expect("sync_messages should succeed");

    assert_eq!(result.updated.len(), 1);
    assert_eq!(result.updated[0].id, "injected-1");
    assert_eq!(result.updated[0].msg, "hello from operator");
    assert!(result.deleted.is_empty());

    // Second call should return empty — injected messages are drained.
    let result2 = client
        .sync_messages("room-1", "2026-03-08T12:00:00.000Z")
        .await
        .expect("second sync should succeed");

    assert!(result2.updated.is_empty());
    assert!(result2.deleted.is_empty());
}

// ── get_me ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn get_me_returns_bot_info() {
    let server = MockRocketChatServer::start().await;
    let client = client_for(&server);

    let user = client.get_me().await.expect("get_me should succeed");

    assert_eq!(user.id, server.user_id());
    assert_eq!(user.username, "ralph-bot");
}

// ── get_room_info ───────────────────────────────────────────────────────

#[tokio::test]
async fn get_room_info_returns_room() {
    let server = MockRocketChatServer::start().await;
    let client = client_for(&server);

    let room = client
        .get_room_info("room-abc")
        .await
        .expect("get_room_info should succeed");

    assert_eq!(room.id, "room-abc");
    assert_eq!(room.t, "c");
    assert_eq!(room.name, Some("general".to_string()));
}

// ── auth errors ─────────────────────────────────────────────────────────

#[tokio::test]
async fn invalid_auth_token_returns_auth_error() {
    let server = MockRocketChatServer::start().await;
    let client = RocketChatClient::new(server.url(), "wrong-token", server.user_id());

    let result = client.send_message("room-1", "should fail", None).await;
    assert!(result.is_err());
    assert!(
        matches!(result.unwrap_err(), RocketChatError::Auth(_)),
        "expected Auth error for invalid token"
    );
}

#[tokio::test]
async fn invalid_user_id_returns_auth_error() {
    let server = MockRocketChatServer::start().await;
    let client = RocketChatClient::new(server.url(), server.auth_token(), "wrong-user-id");

    let result = client.get_me().await;
    assert!(result.is_err());
    assert!(
        matches!(result.unwrap_err(), RocketChatError::Auth(_)),
        "expected Auth error for invalid user ID"
    );
}
