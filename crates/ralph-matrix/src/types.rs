//! Matrix domain types and operator filtering.
//!
//! Defines [`MatrixMessage`], [`SyncResult`], and [`RoomInfo`] for Matrix
//! homeserver interactions, plus the [`filter_by_operator`] helper for
//! group-chat message filtering.

use serde::{Deserialize, Serialize};

/// A Matrix message (room timeline event of type `m.room.message`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MatrixMessage {
    /// The Matrix user ID of the sender (e.g., "@alice:example.com").
    pub sender_id: String,

    /// The message text content.
    pub body: String,

    /// The unique event ID (e.g., "$abc123").
    pub event_id: String,

    /// Timestamp of the event (milliseconds since Unix epoch, from the homeserver).
    pub timestamp: u64,

    /// The room ID this message belongs to (e.g., "!abc:example.com").
    pub room_id: String,

    /// If this message is a reply, the event ID of the message being replied to.
    /// Used for multi-loop routing (analogous to Telegram's reply_to_message_id).
    pub reply_to_event_id: Option<String>,
}

/// Result of a Matrix `/sync` poll for a room's timeline.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SyncResult {
    /// New messages received since the last sync.
    pub messages: Vec<MatrixMessage>,
}

/// Information about a Matrix room.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RoomInfo {
    /// The room's unique ID (e.g., "!abc:example.com").
    pub room_id: String,

    /// The room's display name, if set.
    pub name: Option<String>,

    /// The room's topic, if set.
    pub topic: Option<String>,

    /// Number of joined members in the room.
    pub member_count: u64,
}

/// Filters messages to only those sent by a specific operator.
///
/// Returns messages where `sender_id` matches the given `operator_id`.
pub fn filter_by_operator(messages: &[MatrixMessage], operator_id: &str) -> Vec<MatrixMessage> {
    messages
        .iter()
        .filter(|msg| msg.sender_id == operator_id)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_message(event_id: &str, sender_id: &str) -> MatrixMessage {
        MatrixMessage {
            sender_id: sender_id.to_string(),
            body: format!("msg from {sender_id}"),
            event_id: event_id.to_string(),
            timestamp: 1_700_000_000_000,
            room_id: "!room:example.com".to_string(),
            reply_to_event_id: None,
        }
    }

    // ── Deserialization tests ───────────────────────────────────────────

    #[test]
    fn matrix_message_deserializes() {
        let json = json!({
            "sender_id": "@alice:example.com",
            "body": "hello world",
            "event_id": "$evt-1",
            "timestamp": 1_700_000_000_000_u64,
            "room_id": "!room:example.com",
            "reply_to_event_id": "$parent-evt"
        });
        let msg: MatrixMessage = serde_json::from_value(json).unwrap();
        assert_eq!(msg.sender_id, "@alice:example.com");
        assert_eq!(msg.body, "hello world");
        assert_eq!(msg.event_id, "$evt-1");
        assert_eq!(msg.timestamp, 1_700_000_000_000);
        assert_eq!(msg.room_id, "!room:example.com");
        assert_eq!(msg.reply_to_event_id, Some("$parent-evt".to_string()));
    }

    #[test]
    fn matrix_message_deserializes_without_reply() {
        let json = json!({
            "sender_id": "@bob:example.com",
            "body": "standalone message",
            "event_id": "$evt-2",
            "timestamp": 1_700_000_001_000_u64,
            "room_id": "!room:example.com"
        });
        let msg: MatrixMessage = serde_json::from_value(json).unwrap();
        assert!(msg.reply_to_event_id.is_none());
    }

    #[test]
    fn sync_result_deserializes_with_messages() {
        let json = json!({
            "messages": [
                {
                    "sender_id": "@alice:example.com",
                    "body": "first",
                    "event_id": "$e1",
                    "timestamp": 1_700_000_000_000_u64,
                    "room_id": "!room:example.com"
                },
                {
                    "sender_id": "@bob:example.com",
                    "body": "second",
                    "event_id": "$e2",
                    "timestamp": 1_700_000_001_000_u64,
                    "room_id": "!room:example.com"
                }
            ]
        });
        let result: SyncResult = serde_json::from_value(json).unwrap();
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.messages[0].event_id, "$e1");
        assert_eq!(result.messages[1].event_id, "$e2");
    }

    #[test]
    fn sync_result_deserializes_empty() {
        let json = json!({ "messages": [] });
        let result: SyncResult = serde_json::from_value(json).unwrap();
        assert!(result.messages.is_empty());
    }

    #[test]
    fn room_info_deserializes() {
        let json = json!({
            "room_id": "!abc:example.com",
            "name": "General",
            "topic": "Main discussion",
            "member_count": 42
        });
        let room: RoomInfo = serde_json::from_value(json).unwrap();
        assert_eq!(room.room_id, "!abc:example.com");
        assert_eq!(room.name, Some("General".to_string()));
        assert_eq!(room.topic, Some("Main discussion".to_string()));
        assert_eq!(room.member_count, 42);
    }

    #[test]
    fn room_info_deserializes_without_optional_fields() {
        let json = json!({
            "room_id": "!xyz:example.com",
            "member_count": 2
        });
        let room: RoomInfo = serde_json::from_value(json).unwrap();
        assert_eq!(room.room_id, "!xyz:example.com");
        assert!(room.name.is_none());
        assert!(room.topic.is_none());
        assert_eq!(room.member_count, 2);
    }

    // ── filter_by_operator tests ────────────────────────────────────────

    #[test]
    fn filter_by_operator_returns_only_operator_messages() {
        let messages = vec![
            make_message("$e1", "@operator:example.com"),
            make_message("$e2", "@other:example.com"),
            make_message("$e3", "@operator:example.com"),
        ];
        let filtered = filter_by_operator(&messages, "@operator:example.com");
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].event_id, "$e1");
        assert_eq!(filtered[1].event_id, "$e3");
    }

    #[test]
    fn filter_by_operator_returns_empty_for_no_matches() {
        let messages = vec![
            make_message("$e1", "@other:example.com"),
            make_message("$e2", "@another:example.com"),
        ];
        let filtered = filter_by_operator(&messages, "@operator:example.com");
        assert!(filtered.is_empty());
    }

    #[test]
    fn filter_by_operator_handles_empty_input() {
        let filtered = filter_by_operator(&[], "@operator:example.com");
        assert!(filtered.is_empty());
    }
}
