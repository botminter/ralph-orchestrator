---
status: pending
created: 2026-03-08
started: null
completed: null
---
# Task: Implement RobotService for Rocket.Chat

## Description
Implement `ralph_proto::RobotService` for Rocket.Chat, enabling human-in-the-loop orchestration via Rocket.Chat rooms. This is the core integration that allows agents to ask questions and receive responses from a human operator in Rocket.Chat, with operator filtering to handle group chat scenarios.

## Background
The `RobotService` trait (`crates/ralph-proto/src/robot.rs`) is the backend-agnostic interface the event loop uses for human communication. The Telegram implementation (`TelegramService`) provides the reference pattern: it spawns a background polling task, sends questions as messages, and polls the events JSONL file for responses. The Rocket.Chat implementation follows the same pattern but uses REST API polling via `chat.syncMessages` instead of Telegram's `getUpdates`.

Key difference from Telegram: Rocket.Chat supports group chats where multiple users post. Messages from non-operator users must be silently dropped. The `operator_id` config field identifies whose messages to process.

## Reference Documentation
**Required:**
- `crates/ralph-proto/src/robot.rs` — `RobotService` trait definition
- `crates/ralph-telegram/src/service.rs` — reference implementation to mirror
- `crates/ralph-telegram/src/handler.rs` — `MessageHandler` for event routing pattern
- `crates/ralph-telegram/src/state.rs` — `StateManager` / `TelegramState` for state pattern

**Additional References:**
- `crates/ralph-core/src/event_loop/mod.rs` — how the event loop uses `RobotService`
- Task 2 output — `ralph-rocketchat` REST API client

## Technical Requirements
1. Create `RocketChatService` struct with fields:
   - `workspace_root: PathBuf`
   - `server_url: String`, `auth_token: String`, `bot_user_id: String`
   - `room_id: String`
   - `operator_id: Option<String>`
   - `timeout_secs: u64`
   - `loop_id: String`
   - `state_manager: StateManager`
   - `handler: MessageHandler`
   - `client: Box<dyn RocketChatApi>`
   - `shutdown: Arc<AtomicBool>`

2. Create `RocketChatState` (persisted to `.ralph/rocketchat-state.json`):
   - `last_sync: Option<String>` — ISO timestamp for `chat.syncMessages` `lastUpdate` param
   - `pending_questions: HashMap<String, PendingQuestion>` — keyed by loop ID
   - `PendingQuestion` — `asked_at: String`, `message_id: String` (for thread routing)

3. Create `StateManager` — load/save with atomic writes (temp file + rename), same pattern as Telegram

4. Create `MessageHandler`:
   - `handle_message(state, message: &RcMessage)` — routes messages to correct loop's events JSONL
   - Operator filtering: drop messages where `message.u._id != operator_id` (when `operator_id` is set)
   - Drop system messages (where `message.t` is `Some`)
   - Loop targeting: thread reply match → `@loop-id` prefix → default "main"
   - Classify as `human.response` (pending question exists) or `human.guidance` (otherwise)

5. Implement `RocketChatService::new()` and `start()`:
   - `new()` — construct service, initialize state manager, create API client
   - `start()` — spawn background polling task on tokio runtime

6. Implement background polling:
   - Poll `chat.syncMessages` every 1 second
   - Filter by operator, exclude system messages
   - Route through `MessageHandler`
   - Update `last_sync` timestamp after each poll
   - Handle slash commands (reuse patterns from Telegram: `/help`, `/status`, `/tasks`, etc.)

7. Implement `RobotService` trait:
   - `send_question(payload)` — send message via `chat.sendMessage`, store as pending question with message ID for thread routing
   - `wait_for_response(events_path)` — synchronously poll events JSONL file every 250ms for `human.response` event (same as Telegram), respect timeout and shutdown flag
   - `send_checkin(iteration, elapsed, context)` — send status message to room with optional enriched context
   - `timeout_secs()` — return configured timeout
   - `shutdown_flag()` — return Arc clone
   - `stop()` — set shutdown flag, send farewell message

8. Wire into `create_robot_service()` in `loop_runner.rs` (replacing the `todo!()` from Task 1)

## Dependencies
- Task 1 — `RocketChatConfig` in `RobotConfig`
- Task 2 — `ralph-rocketchat` REST API client and types

## Implementation Approach
1. Create `service.rs`, `handler.rs`, `state.rs` in `crates/ralph-rocketchat/src/`
2. Implement `StateManager` and `RocketChatState` first (simplest, testable independently)
3. Implement `MessageHandler` with operator filtering and loop targeting
4. Implement `RocketChatService` with background polling
5. Implement `RobotService` trait
6. Wire into `create_robot_service()` in `loop_runner.rs`
7. Add `ralph-rocketchat` as a dependency of `ralph-cli`

## Acceptance Criteria

1. **Send question to room**
   - Given a running `RocketChatService` with a configured room
   - When `send_question("What should I do?")` is called
   - Then a message is sent to the configured room via `chat.sendMessage`
   - And a `PendingQuestion` is stored in state with the message ID

2. **Wait for response**
   - Given a pending question exists for the current loop
   - When a `human.response` event is written to the events JSONL
   - Then `wait_for_response()` returns `Some(response_text)`

3. **Response timeout**
   - Given a pending question with `timeout_secs: 5`
   - When no response arrives within 5 seconds
   - Then `wait_for_response()` returns `None`

4. **Operator filtering**
   - Given `operator_id` is set to "user123"
   - When messages arrive from "user123", "bot-user", and "other-human"
   - Then only messages from "user123" are processed
   - And messages from other users are silently dropped

5. **Operator filtering disabled**
   - Given `operator_id` is `None`
   - When messages arrive from any user
   - Then all non-system messages are processed

6. **System message filtering**
   - Given a system message (e.g., "user joined") arrives
   - When the message handler processes it
   - Then it is dropped regardless of sender

7. **Thread-based loop targeting**
   - Given a pending question for loop "feature-x" with `message_id: "msg123"`
   - When a reply arrives with `tmid: "msg123"`
   - Then the response is routed to loop "feature-x"

8. **Guidance classification**
   - Given no pending question for any loop
   - When the operator sends a message
   - Then it is classified as `human.guidance` and written to events JSONL

9. **State persistence**
   - Given the service processes messages and updates `last_sync`
   - When the state is saved and reloaded
   - Then `last_sync` and `pending_questions` are preserved

10. **Shutdown**
    - Given a running service
    - When `stop()` is called
    - Then the shutdown flag is set, polling stops, and a farewell message is sent

11. **Check-in messages**
    - Given `checkin_interval_seconds` is configured
    - When `send_checkin()` is called with iteration info
    - Then a formatted status message is sent to the room

## Metadata
- **Complexity**: High
- **Labels**: rocketchat, robot-service, core
- **Required Skills**: Rust, async/tokio, trait implementation, event-driven architecture
