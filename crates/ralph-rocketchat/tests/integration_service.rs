//! Integration tests for the Rocket.Chat service pipeline.
//!
//! Tests the integration of `MessageHandler` + `StateManager` + `filter_by_operator`
//! using `MockRocketChatClient` as the message source. Covers:
//! - Operator filtering (only operator messages processed, system messages dropped)
//! - Multi-loop message routing (tmid, @loop-id prefix, default to "main")
//! - State persistence (last_sync restored across service instances)

use std::collections::HashMap;

use ralph_rocketchat::client::{MockRocketChatClient, RocketChatApi};
use ralph_rocketchat::handler::MessageHandler;
use ralph_rocketchat::state::{RocketChatState, StateManager};
use ralph_rocketchat::types::{RcMessage, RcUser, SyncResult, filter_by_operator};
use tempfile::TempDir;

// ── Helpers ──────────────────────────────────────────────────────────────

fn make_user(id: &str, username: &str) -> RcUser {
    RcUser {
        id: id.to_string(),
        username: username.to_string(),
        name: Some(username.to_string()),
    }
}

fn make_message(id: &str, user: &RcUser, text: &str) -> RcMessage {
    RcMessage {
        id: id.to_string(),
        rid: "room-1".to_string(),
        msg: text.to_string(),
        u: user.clone(),
        ts: "2026-03-08T12:00:00.000Z".to_string(),
        tmid: None,
        t: None,
    }
}

fn make_system_message(id: &str, user: &RcUser, msg_type: &str) -> RcMessage {
    RcMessage {
        id: id.to_string(),
        rid: "room-1".to_string(),
        msg: String::new(),
        u: user.clone(),
        ts: "2026-03-08T12:00:00.000Z".to_string(),
        tmid: None,
        t: Some(msg_type.to_string()),
    }
}

fn make_threaded_message(id: &str, user: &RcUser, text: &str, tmid: &str) -> RcMessage {
    RcMessage {
        id: id.to_string(),
        rid: "room-1".to_string(),
        msg: text.to_string(),
        u: user.clone(),
        ts: "2026-03-08T12:00:00.000Z".to_string(),
        tmid: Some(tmid.to_string()),
        t: None,
    }
}

/// Set up a workspace with StateManager and MessageHandler sharing the same state path.
fn setup_workspace() -> (TempDir, StateManager, MessageHandler) {
    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join(".ralph/rocketchat-state.json");
    let state_manager = StateManager::new(&state_path);
    let handler_state_manager = StateManager::new(&state_path);
    let handler = MessageHandler::new(handler_state_manager, dir.path());
    (dir, state_manager, handler)
}

/// Simulate one poll cycle: sync → filter → handle, mirroring poll_messages() logic.
fn simulate_poll_cycle(
    messages: &[RcMessage],
    operator_id: Option<&str>,
    state: &mut RocketChatState,
    handler: &MessageHandler,
) -> Vec<String> {
    let filtered = if let Some(op_id) = operator_id {
        filter_by_operator(messages, op_id)
    } else {
        messages
            .iter()
            .filter(|msg| msg.t.is_none())
            .cloned()
            .collect()
    };

    let mut topics = Vec::new();
    for msg in &filtered {
        match handler.handle_message(state, msg) {
            Ok(topic) => topics.push(topic),
            Err(e) => panic!("handle_message failed: {e}"),
        }
    }
    topics
}

/// Read events written to the default events.jsonl for a loop.
fn read_events(dir: &std::path::Path, loop_id: &str) -> Vec<serde_json::Value> {
    let events_path = if loop_id == "main" {
        dir.join(".ralph/events.jsonl")
    } else {
        dir.join(format!(".worktrees/{loop_id}/.ralph/events.jsonl"))
    };

    if !events_path.exists() {
        return vec![];
    }

    std::fs::read_to_string(&events_path)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

// ── Operator Filtering ──────────────────────────────────────────────────

#[tokio::test]
async fn only_operator_messages_are_processed() {
    let (dir, _state_mgr, handler) = setup_workspace();
    let mut state = RocketChatState {
        last_sync: None,
        pending_questions: HashMap::new(),
    };

    let operator = make_user("operator-1", "alice");
    let other = make_user("other-user", "bob");

    let mock = MockRocketChatClient::new();
    mock.on_sync_messages(Ok(SyncResult {
        updated: vec![
            make_message("m1", &operator, "operator says hello"),
            make_message("m2", &other, "bystander chatter"),
            make_message("m3", &operator, "operator says goodbye"),
        ],
        deleted: vec![],
    }));

    let sync = mock.sync_messages("room-1", "ts").await.unwrap();
    let topics = simulate_poll_cycle(&sync.updated, Some("operator-1"), &mut state, &handler);

    assert_eq!(
        topics.len(),
        2,
        "only 2 operator messages should be handled"
    );

    let events = read_events(dir.path(), "main");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["payload"], "operator says hello");
    assert_eq!(events[1]["payload"], "operator says goodbye");
}

#[tokio::test]
async fn system_messages_are_dropped() {
    let (dir, _state_mgr, handler) = setup_workspace();
    let mut state = RocketChatState {
        last_sync: None,
        pending_questions: HashMap::new(),
    };

    let operator = make_user("operator-1", "alice");

    let mock = MockRocketChatClient::new();
    mock.on_sync_messages(Ok(SyncResult {
        updated: vec![
            make_message("m1", &operator, "real message"),
            make_system_message("m2", &operator, "uj"), // user joined
            make_system_message("m3", &operator, "au"), // added user
            make_system_message("m4", &operator, "rl"), // role change
        ],
        deleted: vec![],
    }));

    let sync = mock.sync_messages("room-1", "ts").await.unwrap();
    let topics = simulate_poll_cycle(&sync.updated, Some("operator-1"), &mut state, &handler);

    assert_eq!(topics.len(), 1, "only normal messages should pass filter");

    let events = read_events(dir.path(), "main");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["payload"], "real message");
}

#[tokio::test]
async fn no_operator_id_processes_all_non_system_messages() {
    let (dir, _state_mgr, handler) = setup_workspace();
    let mut state = RocketChatState {
        last_sync: None,
        pending_questions: HashMap::new(),
    };

    let alice = make_user("user-a", "alice");
    let bob = make_user("user-b", "bob");

    let mock = MockRocketChatClient::new();
    mock.on_sync_messages(Ok(SyncResult {
        updated: vec![
            make_message("m1", &alice, "from alice"),
            make_message("m2", &bob, "from bob"),
            make_system_message("m3", &alice, "uj"), // system — should be dropped
        ],
        deleted: vec![],
    }));

    let sync = mock.sync_messages("room-1", "ts").await.unwrap();
    // operator_id = None → all non-system messages processed
    let topics = simulate_poll_cycle(&sync.updated, None, &mut state, &handler);

    assert_eq!(
        topics.len(),
        2,
        "without operator_id, all non-system messages should be processed"
    );

    let events = read_events(dir.path(), "main");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["payload"], "from alice");
    assert_eq!(events[1]["payload"], "from bob");
}

// ── Multi-Loop Message Routing ──────────────────────────────────────────

#[tokio::test]
async fn tmid_thread_reply_routes_to_correct_loop() {
    let (dir, state_mgr, handler) = setup_workspace();
    let mut state = RocketChatState {
        last_sync: None,
        pending_questions: HashMap::new(),
    };

    // Simulate a pending question for "feature-auth" loop with message_id "q-msg-42"
    state_mgr
        .add_pending_question(&mut state, "feature-auth", "q-msg-42")
        .unwrap();

    let operator = make_user("operator-1", "alice");
    let msg = make_threaded_message("reply-1", &operator, "yes, proceed", "q-msg-42");

    let topics = simulate_poll_cycle(&[msg], Some("operator-1"), &mut state, &handler);

    assert_eq!(topics.len(), 1);
    assert_eq!(
        topics[0], "human.response",
        "thread reply should be a response"
    );

    // Event should be routed to the feature-auth loop's events file
    let events = read_events(dir.path(), "feature-auth");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["topic"], "human.response");
    assert_eq!(events[0]["payload"], "yes, proceed");

    // No events should appear in main loop
    let main_events = read_events(dir.path(), "main");
    assert!(main_events.is_empty(), "main loop should have no events");
}

#[tokio::test]
async fn at_loop_id_prefix_routes_correctly() {
    let (dir, _state_mgr, handler) = setup_workspace();
    let mut state = RocketChatState {
        last_sync: None,
        pending_questions: HashMap::new(),
    };

    let operator = make_user("operator-1", "alice");
    let msg = make_message("m1", &operator, "@feature-db use postgres");

    let topics = simulate_poll_cycle(&[msg], Some("operator-1"), &mut state, &handler);

    assert_eq!(topics.len(), 1);
    assert_eq!(topics[0], "human.guidance");

    // Event should be routed to feature-db loop
    let events = read_events(dir.path(), "feature-db");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["topic"], "human.guidance");
    assert_eq!(events[0]["payload"], "@feature-db use postgres");

    // main should be empty
    let main_events = read_events(dir.path(), "main");
    assert!(main_events.is_empty());
}

#[tokio::test]
async fn untagged_message_defaults_to_main() {
    let (dir, _state_mgr, handler) = setup_workspace();
    let mut state = RocketChatState {
        last_sync: None,
        pending_questions: HashMap::new(),
    };

    let operator = make_user("operator-1", "alice");
    let msg = make_message("m1", &operator, "just some guidance");

    let topics = simulate_poll_cycle(&[msg], Some("operator-1"), &mut state, &handler);

    assert_eq!(topics.len(), 1);
    assert_eq!(topics[0], "human.guidance");

    let events = read_events(dir.path(), "main");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["topic"], "human.guidance");
    assert_eq!(events[0]["payload"], "just some guidance");
}

#[tokio::test]
async fn tmid_takes_priority_over_at_prefix() {
    let (dir, state_mgr, handler) = setup_workspace();
    let mut state = RocketChatState {
        last_sync: None,
        pending_questions: HashMap::new(),
    };

    // Pending question for "loop-a" with thread message "tmid-xyz"
    state_mgr
        .add_pending_question(&mut state, "loop-a", "tmid-xyz")
        .unwrap();

    let operator = make_user("operator-1", "alice");
    // Message has @loop-b prefix BUT tmid matches loop-a
    let msg = make_threaded_message("reply-1", &operator, "@loop-b some text", "tmid-xyz");

    let topics = simulate_poll_cycle(&[msg], Some("operator-1"), &mut state, &handler);

    assert_eq!(topics.len(), 1);
    assert_eq!(topics[0], "human.response");

    // Should route to loop-a (tmid wins), NOT loop-b
    let loop_a_events = read_events(dir.path(), "loop-a");
    assert_eq!(loop_a_events.len(), 1, "tmid routing should take priority");

    let loop_b_events = read_events(dir.path(), "loop-b");
    assert!(
        loop_b_events.is_empty(),
        "@prefix should be ignored when tmid matches"
    );
}

#[tokio::test]
async fn pending_question_cleared_after_response() {
    let (_dir, state_mgr, handler) = setup_workspace();
    let mut state = RocketChatState {
        last_sync: None,
        pending_questions: HashMap::new(),
    };

    state_mgr
        .add_pending_question(&mut state, "main", "q-msg-1")
        .unwrap();
    assert!(state.pending_questions.contains_key("main"));

    let operator = make_user("operator-1", "alice");
    let msg = make_threaded_message("reply-1", &operator, "use async", "q-msg-1");

    simulate_poll_cycle(&[msg], Some("operator-1"), &mut state, &handler);

    // Pending question should be cleared after processing the response
    assert!(
        !state.pending_questions.contains_key("main"),
        "pending question should be removed after response"
    );
}

// ── State Persistence ───────────────────────────────────────────────────

#[tokio::test]
async fn state_persists_last_sync_across_service_instances() {
    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join(".ralph/rocketchat-state.json");

    // First "service instance" — process messages and save state
    {
        let state_manager = StateManager::new(&state_path);
        let mut state = state_manager.load_or_default().unwrap();
        assert!(state.last_sync.is_none(), "fresh state has no last_sync");

        // Simulate what poll_messages does after processing
        state.last_sync = Some("2026-03-08T14:30:00.000Z".to_string());
        state_manager.save(&state).unwrap();
    }

    // Second "service instance" — loads persisted state
    {
        let state_manager = StateManager::new(&state_path);
        let state = state_manager.load_or_default().unwrap();
        assert_eq!(
            state.last_sync,
            Some("2026-03-08T14:30:00.000Z".to_string()),
            "last_sync should be restored from disk"
        );
    }
}

#[tokio::test]
async fn state_persists_pending_questions_across_service_instances() {
    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join(".ralph/rocketchat-state.json");

    // First instance adds a pending question
    {
        let state_manager = StateManager::new(&state_path);
        let mut state = state_manager.load_or_default().unwrap();
        state_manager
            .add_pending_question(&mut state, "feature-auth", "msg-q1")
            .unwrap();
    }

    // Second instance sees the pending question and can route replies
    {
        let state_manager = StateManager::new(&state_path);
        let state = state_manager.load_or_default().unwrap();
        assert!(
            state.pending_questions.contains_key("feature-auth"),
            "pending question should survive across instances"
        );
        assert_eq!(state.pending_questions["feature-auth"].message_id, "msg-q1");

        // Verify reply routing works with persisted state
        let loop_id = state_manager.get_loop_for_reply(&state, "msg-q1");
        assert_eq!(loop_id, Some("feature-auth".to_string()));
    }
}

#[tokio::test]
async fn full_pipeline_with_state_persistence() {
    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join(".ralph/rocketchat-state.json");
    let operator = make_user("operator-1", "alice");

    // First poll cycle — processes messages and persists state
    {
        let state_manager = StateManager::new(&state_path);
        let handler_sm = StateManager::new(&state_path);
        let handler = MessageHandler::new(handler_sm, dir.path());
        let mut state = state_manager.load_or_default().unwrap();

        let mock = MockRocketChatClient::new();
        mock.on_sync_messages(Ok(SyncResult {
            updated: vec![make_message("m1", &operator, "initial guidance")],
            deleted: vec![],
        }));
        let sync = mock.sync_messages("room-1", "ts").await.unwrap();
        simulate_poll_cycle(&sync.updated, Some("operator-1"), &mut state, &handler);

        state.last_sync = Some("2026-03-08T15:00:00.000Z".to_string());
        state_manager.save(&state).unwrap();
    }

    // Second poll cycle — new service instance, state restored
    {
        let state_manager = StateManager::new(&state_path);
        let handler_sm = StateManager::new(&state_path);
        let handler = MessageHandler::new(handler_sm, dir.path());
        let mut state = state_manager.load_or_default().unwrap();

        assert_eq!(
            state.last_sync.as_deref(),
            Some("2026-03-08T15:00:00.000Z"),
            "last_sync should be restored"
        );

        let mock = MockRocketChatClient::new();
        mock.on_sync_messages(Ok(SyncResult {
            updated: vec![make_message("m2", &operator, "follow-up guidance")],
            deleted: vec![],
        }));
        let sync = mock
            .sync_messages("room-1", state.last_sync.as_deref().unwrap())
            .await
            .unwrap();
        simulate_poll_cycle(&sync.updated, Some("operator-1"), &mut state, &handler);

        // Both messages should be in the events file (appended across cycles)
        let events = read_events(dir.path(), "main");
        assert_eq!(
            events.len(),
            2,
            "events should accumulate across poll cycles"
        );
        assert_eq!(events[0]["payload"], "initial guidance");
        assert_eq!(events[1]["payload"], "follow-up guidance");
    }
}

// ── Mixed Scenario ──────────────────────────────────────────────────────

#[tokio::test]
async fn mixed_messages_filter_and_route_correctly() {
    let (dir, state_mgr, handler) = setup_workspace();
    let mut state = RocketChatState {
        last_sync: None,
        pending_questions: HashMap::new(),
    };

    // Set up a pending question for main loop
    state_mgr
        .add_pending_question(&mut state, "main", "q-main")
        .unwrap();

    let operator = make_user("operator-1", "alice");
    let bystander = make_user("other-user", "eve");

    let messages = vec![
        make_threaded_message("m1", &operator, "answer to main question", "q-main"),
        make_message("m2", &bystander, "should be filtered out"),
        make_message("m3", &operator, "@feature-x try caching"),
        make_system_message("m4", &operator, "uj"),
        make_message("m5", &operator, "general guidance"),
    ];

    let topics = simulate_poll_cycle(&messages, Some("operator-1"), &mut state, &handler);

    // m1: response to main (thread match), m3: guidance to feature-x, m5: guidance to main
    // m2: filtered (wrong user), m4: filtered (system message)
    assert_eq!(topics.len(), 3);
    assert_eq!(topics[0], "human.response"); // m1 → thread reply to main
    assert_eq!(topics[1], "human.guidance"); // m3 → @feature-x
    assert_eq!(topics[2], "human.guidance"); // m5 → default main

    // Check main loop events (m1 response + m5 guidance)
    let main_events = read_events(dir.path(), "main");
    assert_eq!(main_events.len(), 2);
    assert_eq!(main_events[0]["topic"], "human.response");
    assert_eq!(main_events[0]["payload"], "answer to main question");
    assert_eq!(main_events[1]["topic"], "human.guidance");
    assert_eq!(main_events[1]["payload"], "general guidance");

    // Check feature-x loop events (m3 guidance)
    let fx_events = read_events(dir.path(), "feature-x");
    assert_eq!(fx_events.len(), 1);
    assert_eq!(fx_events[0]["topic"], "human.guidance");
    assert_eq!(fx_events[0]["payload"], "@feature-x try caching");

    // Pending question for main should be cleared
    assert!(!state.pending_questions.contains_key("main"));
}
