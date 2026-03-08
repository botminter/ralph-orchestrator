//! Rocket.Chat API response types and operator filtering.
//!
//! Defines the domain types ([`RcUser`], [`RcMessage`], [`RcRoom`], [`SyncResult`])
//! deserialized from Rocket.Chat REST API responses, plus the [`filter_by_operator`]
//! helper for group-chat message filtering.

use serde::{Deserialize, Serialize};

/// A Rocket.Chat user, as returned by the REST API.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RcUser {
    /// The user's unique ID (MongoDB `_id`).
    #[serde(rename = "_id")]
    pub id: String,

    /// The user's username (e.g., "ralph-bot").
    pub username: String,

    /// The user's display name.
    pub name: Option<String>,
}

/// A Rocket.Chat message, as returned by the REST API.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RcMessage {
    /// The message's unique ID (MongoDB `_id`).
    #[serde(rename = "_id")]
    pub id: String,

    /// The room ID this message belongs to.
    pub rid: String,

    /// The message text content.
    pub msg: String,

    /// The user who sent this message.
    pub u: RcUser,

    /// Timestamp of the message (ISO 8601 string from the server).
    pub ts: String,

    /// Thread message ID — if set, this message is a reply in a thread.
    /// Used for multi-loop routing (analogous to Telegram's reply_to_message_id).
    pub tmid: Option<String>,

    /// Message type — `None` for normal user messages, `Some("uj")` for "user joined", etc.
    /// System messages (where `t` is `Some`) should be filtered out when processing operator input.
    pub t: Option<String>,
}

/// A Rocket.Chat room (channel, group, or DM).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RcRoom {
    /// The room's unique ID (MongoDB `_id`).
    #[serde(rename = "_id")]
    pub id: String,

    /// Room type: "c" (channel), "p" (private group), "d" (direct message).
    pub t: String,

    /// The room's name (not present for DMs).
    pub name: Option<String>,

    /// The room's display name (not present for DMs).
    pub fname: Option<String>,
}

/// Result of a `chat.syncMessages` API call.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SyncResult {
    /// Messages that were created or updated since the last sync.
    pub updated: Vec<RcMessage>,

    /// Messages that were deleted since the last sync.
    pub deleted: Vec<RcMessage>,
}

/// Filters messages to only those sent by a specific operator, excluding system messages.
///
/// System messages have `t` set to `Some(...)` (e.g., "uj" = user joined, "au" = added user).
/// Only normal user messages (`t` is `None`) from the given operator ID are returned.
pub fn filter_by_operator(messages: &[RcMessage], operator_id: &str) -> Vec<RcMessage> {
    messages
        .iter()
        .filter(|msg| msg.u.id == operator_id && msg.t.is_none())
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_user(id: &str) -> RcUser {
        RcUser {
            id: id.to_string(),
            username: "user".to_string(),
            name: Some("User".to_string()),
        }
    }

    fn make_message(id: &str, user_id: &str, t: Option<&str>) -> RcMessage {
        RcMessage {
            id: id.to_string(),
            rid: "room1".to_string(),
            msg: format!("msg from {user_id}"),
            u: make_user(user_id),
            ts: "2026-01-01T00:00:00.000Z".to_string(),
            tmid: None,
            t: t.map(String::from),
        }
    }

    // ── Deserialization tests ───────────────────────────────────────────

    #[test]
    fn rc_user_deserializes_with_id_rename() {
        let json = json!({
            "_id": "user-42",
            "username": "alice",
            "name": "Alice"
        });
        let user: RcUser = serde_json::from_value(json).unwrap();
        assert_eq!(user.id, "user-42");
        assert_eq!(user.username, "alice");
        assert_eq!(user.name, Some("Alice".to_string()));
    }

    #[test]
    fn rc_user_deserializes_without_optional_name() {
        let json = json!({
            "_id": "user-43",
            "username": "bob"
        });
        let user: RcUser = serde_json::from_value(json).unwrap();
        assert_eq!(user.id, "user-43");
        assert!(user.name.is_none());
    }

    #[test]
    fn rc_message_deserializes_with_id_rename() {
        let json = json!({
            "_id": "msg-1",
            "rid": "room-abc",
            "msg": "hello world",
            "u": { "_id": "user-1", "username": "alice", "name": "Alice" },
            "ts": "2026-03-08T12:00:00.000Z",
            "tmid": "thread-99",
            "t": null
        });
        let msg: RcMessage = serde_json::from_value(json).unwrap();
        assert_eq!(msg.id, "msg-1");
        assert_eq!(msg.rid, "room-abc");
        assert_eq!(msg.msg, "hello world");
        assert_eq!(msg.u.id, "user-1");
        assert_eq!(msg.tmid, Some("thread-99".to_string()));
        assert!(msg.t.is_none());
    }

    #[test]
    fn rc_message_system_message_has_type() {
        let json = json!({
            "_id": "msg-2",
            "rid": "room-abc",
            "msg": "",
            "u": { "_id": "user-1", "username": "alice" },
            "ts": "2026-03-08T12:00:00.000Z",
            "t": "uj"
        });
        let msg: RcMessage = serde_json::from_value(json).unwrap();
        assert_eq!(msg.t, Some("uj".to_string()));
    }

    #[test]
    fn rc_room_deserializes_with_id_rename() {
        let json = json!({
            "_id": "room-xyz",
            "t": "c",
            "name": "general",
            "fname": "General"
        });
        let room: RcRoom = serde_json::from_value(json).unwrap();
        assert_eq!(room.id, "room-xyz");
        assert_eq!(room.t, "c");
        assert_eq!(room.name, Some("general".to_string()));
        assert_eq!(room.fname, Some("General".to_string()));
    }

    #[test]
    fn rc_room_deserializes_dm_without_name() {
        let json = json!({
            "_id": "dm-123",
            "t": "d"
        });
        let room: RcRoom = serde_json::from_value(json).unwrap();
        assert_eq!(room.id, "dm-123");
        assert_eq!(room.t, "d");
        assert!(room.name.is_none());
        assert!(room.fname.is_none());
    }

    #[test]
    fn sync_result_deserializes_with_messages() {
        let json = json!({
            "updated": [
                {
                    "_id": "msg-10",
                    "rid": "room-1",
                    "msg": "new message",
                    "u": { "_id": "u1", "username": "alice" },
                    "ts": "2026-03-08T12:00:00.000Z"
                }
            ],
            "deleted": [
                {
                    "_id": "msg-5",
                    "rid": "room-1",
                    "msg": "deleted",
                    "u": { "_id": "u2", "username": "bob" },
                    "ts": "2026-03-08T11:00:00.000Z"
                }
            ]
        });
        let result: SyncResult = serde_json::from_value(json).unwrap();
        assert_eq!(result.updated.len(), 1);
        assert_eq!(result.updated[0].id, "msg-10");
        assert_eq!(result.deleted.len(), 1);
        assert_eq!(result.deleted[0].id, "msg-5");
    }

    #[test]
    fn sync_result_deserializes_empty() {
        let json = json!({
            "updated": [],
            "deleted": []
        });
        let result: SyncResult = serde_json::from_value(json).unwrap();
        assert!(result.updated.is_empty());
        assert!(result.deleted.is_empty());
    }

    // ── filter_by_operator tests ────────────────────────────────────────

    #[test]
    fn filter_by_operator_returns_only_operator_messages() {
        let messages = vec![
            make_message("m1", "operator-1", None),
            make_message("m2", "other-user", None),
            make_message("m3", "operator-1", None),
        ];
        let filtered = filter_by_operator(&messages, "operator-1");
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].id, "m1");
        assert_eq!(filtered[1].id, "m3");
    }

    #[test]
    fn filter_by_operator_excludes_system_messages() {
        let messages = vec![
            make_message("m1", "operator-1", None),
            make_message("m2", "operator-1", Some("uj")), // system: user joined
            make_message("m3", "operator-1", Some("au")), // system: added user
        ];
        let filtered = filter_by_operator(&messages, "operator-1");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "m1");
    }

    #[test]
    fn filter_by_operator_returns_empty_for_no_matches() {
        let messages = vec![
            make_message("m1", "other-user", None),
            make_message("m2", "another-user", None),
        ];
        let filtered = filter_by_operator(&messages, "operator-1");
        assert!(filtered.is_empty());
    }

    #[test]
    fn filter_by_operator_handles_empty_input() {
        let filtered = filter_by_operator(&[], "operator-1");
        assert!(filtered.is_empty());
    }
}
