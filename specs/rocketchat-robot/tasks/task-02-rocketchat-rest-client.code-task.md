---
status: pending
created: 2026-03-08
started: null
completed: null
---
# Task: Create ralph-rocketchat Crate — REST API Client

## Description
Create a new `ralph-rocketchat` crate with a Rust HTTP client for the Rocket.Chat REST API. This provides the low-level API communication layer that the `RobotService` implementation (Task 3) will build on. Mirrors the `BotApi` trait pattern from `ralph-telegram`.

## Background
Rocket.Chat uses a REST API at `/api/v1/` with authentication via `X-Auth-Token` and `X-User-Id` headers. Personal Access Tokens are the recommended auth method for bots. Key endpoints:
- `POST /api/v1/chat.sendMessage` — send a message to a room (supports threading via `tmid`)
- `GET /api/v1/chat.syncMessages` — incremental message sync since a timestamp (returns `updated` + `deleted` arrays)
- `GET /api/v1/me` — get current user info (validates credentials)
- `GET /api/v1/channels.info` / `groups.info` / `dm.info` — get room info

Message structure includes `u: { _id, username, name }` for sender identification, `rid` for room ID, `tmid` for thread parent, and `ts` for timestamp.

## Reference Documentation
**Required:**
- Rocket.Chat source: `/opt/workspace/rocket-chat/packages/rest-typings/src/v1/chat.ts` — endpoint type definitions
- Rocket.Chat source: `/opt/workspace/rocket-chat/packages/core-typings/src/IMessage/IMessage.ts` — message model
- Rocket.Chat source: `/opt/workspace/rocket-chat/packages/core-typings/src/IRoom.ts` — room model

**Additional References:**
- `crates/ralph-telegram/src/bot.rs` — `BotApi` trait and `MockBot` pattern to mirror
- `crates/ralph-telegram/src/error.rs` — error type pattern to follow
- `crates/ralph-telegram/Cargo.toml` — dependency patterns

## Technical Requirements
1. Create new crate at `crates/ralph-rocketchat/` with `Cargo.toml`
   - Dependencies: `reqwest` (with `json` feature), `serde`, `serde_json`, `tokio`, `tracing`, `anyhow`, `chrono`
2. Define a `RocketChatApi` trait (mirrors `BotApi` pattern) with async methods:
   - `send_message(room_id: &str, text: &str, thread_id: Option<&str>) -> Result<RcMessage>`
   - `sync_messages(room_id: &str, last_update: &str) -> Result<SyncResult>`
   - `get_me() -> Result<RcUser>`
   - `get_room_info(room_id: &str) -> Result<RcRoom>`
3. Define response types (only fields we need):
   - `RcMessage` — `_id`, `rid`, `msg`, `u` (sender), `ts`, `tmid` (thread parent), `t` (system message type)
   - `RcUser` — `_id`, `username`, `name`
   - `RcRoom` — `_id`, `t` (room type), `name`, `fname`
   - `SyncResult` — `updated: Vec<RcMessage>`, `deleted: Vec<RcMessage>`
4. Implement `RocketChatClient` struct:
   - Holds `reqwest::Client`, `server_url`, `auth_token`, `bot_user_id`
   - Implements `RocketChatApi`
   - All requests include `X-Auth-Token` and `X-User-Id` headers (using `bot_user_id` for the latter)
   - Handles HTTP error responses with meaningful error messages
5. Define `RocketChatError` enum following `ralph-telegram`'s pattern:
   - `Auth` — authentication failure (401)
   - `Send` — message send failure
   - `Receive` — message fetch failure
   - `NotFound` — room/message not found (404)
   - `RateLimit` — rate limited (429)
   - `Server` — server error (5xx)
   - `Deserialize` — response parsing failure
6. Create `MockRocketChatClient` struct for testing:
   - Records all API calls
   - Configurable responses (success/failure)
   - Implements `RocketChatApi`
7. Add helper function `filter_by_operator(messages: &[RcMessage], operator_id: &str) -> Vec<&RcMessage>` — filters messages where `message.u._id == operator_id`, excluding system messages (where `t` is `Some`)

## Dependencies
- Task 1 (config) should be done first for `RocketChatConfig`, but this crate can be developed independently

## Implementation Approach
1. Create crate directory structure: `crates/ralph-rocketchat/src/{lib.rs, client.rs, types.rs, error.rs}`
2. Define types in `types.rs` with `Deserialize` — use `#[serde(rename_all = "camelCase")]` where needed
3. Define `RocketChatApi` trait in `client.rs`
4. Implement `RocketChatClient` with reqwest
5. Implement `MockRocketChatClient` in `client.rs` under `#[cfg(test)]` (and also as a public test utility)
6. Add `filter_by_operator` helper
7. Add to workspace `Cargo.toml`
8. Write unit tests for response parsing, error handling, operator filtering

## Acceptance Criteria

1. **Credential validation**
   - Given valid `auth_token` and `user_id`
   - When `get_me()` is called
   - Then the bot's user info is returned with matching `_id`

2. **Send message to room**
   - Given a valid `room_id` and message text
   - When `send_message(room_id, text, None)` is called
   - Then the message is sent via `POST /api/v1/chat.sendMessage` with correct headers and body

3. **Send threaded reply**
   - Given a valid `room_id`, text, and `thread_id`
   - When `send_message(room_id, text, Some(thread_id))` is called
   - Then the request body includes `tmid` field for threading

4. **Sync messages**
   - Given a room with messages since a timestamp
   - When `sync_messages(room_id, last_update)` is called
   - Then a `SyncResult` with `updated` and `deleted` message arrays is returned

5. **Operator filtering**
   - Given messages from users "operator1", "bot-user", and "other-human"
   - When `filter_by_operator(messages, "operator1")` is called
   - Then only messages from "operator1" are returned

6. **System message filtering**
   - Given messages including system messages (where `t` is `Some`, e.g. "uj" for user joined)
   - When `filter_by_operator` is called
   - Then system messages are excluded regardless of sender

7. **Auth error handling**
   - Given invalid credentials
   - When any API call is made
   - Then `RocketChatError::Auth` is returned with the server's error message

8. **Mock client records calls**
   - Given a `MockRocketChatClient`
   - When `send_message()` is called
   - Then the call is recorded and can be inspected in tests

9. **Crate compiles in workspace**
   - Given the new crate is added to the workspace
   - When `cargo build -p ralph-rocketchat` is run
   - Then it compiles without errors

## Metadata
- **Complexity**: Medium
- **Labels**: rocketchat, api-client, crate
- **Required Skills**: Rust, reqwest, serde, REST APIs, async
