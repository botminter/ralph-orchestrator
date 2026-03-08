---
status: pending
created: 2026-03-08
started: null
completed: null
---
# Task: Testing Infrastructure and Integration Tests

## Description
Build testing infrastructure for the Rocket.Chat integration including a mock HTTP server, integration tests covering the full `RobotService` lifecycle, operator filtering, and multi-loop message routing. Ensures the Rocket.Chat backend is as well-tested as the Telegram backend.

## Background
The `ralph-telegram` crate uses inline `#[cfg(test)]` modules with a `MockBot` struct for unit tests. For integration-level testing, Telegram uses `telegram-test-api` (a mock Telegram server). We need an equivalent mock Rocket.Chat HTTP server that responds to the REST API endpoints used by our client, plus integration tests that exercise the full service lifecycle.

Unit tests for individual components (client, handler, state) are included in Tasks 2-3. This task focuses on integration tests and the mock server infrastructure that enables them.

## Reference Documentation
**Required:**
- `crates/ralph-telegram/src/bot.rs` — `MockBot` pattern (line ~362)
- `crates/ralph-telegram/src/service.rs` — service unit tests (line ~814)
- `crates/ralph-e2e/` — E2E test framework for reference

**Additional References:**
- Task 2 output — `RocketChatApi` trait and `MockRocketChatClient`
- Task 3 output — `RocketChatService`, `MessageHandler`, `StateManager`
- Rocket.Chat API endpoints used: `chat.sendMessage`, `chat.syncMessages`, `me`, `channels.info`

## Technical Requirements
1. **Mock Rocket.Chat HTTP Server:**
   - Lightweight HTTP server (using `axum` or `warp`) that simulates Rocket.Chat REST API
   - Endpoints to implement:
     - `POST /api/v1/chat.sendMessage` — accepts messages, stores in memory, returns success
     - `GET /api/v1/chat.syncMessages` — returns messages since `lastUpdate` timestamp
     - `GET /api/v1/me` — returns bot user info
     - `GET /api/v1/channels.info` — returns room info
   - Auth validation: check `X-Auth-Token` and `X-User-Id` headers, reject invalid credentials
   - API to inject messages (for simulating operator responses)
   - API to inspect sent messages (for asserting bot output)
   - Binds to random available port for test isolation

2. **Integration Tests — Service Lifecycle:**
   - Test: Start `RocketChatService` → send question → inject operator response → verify `wait_for_response()` returns it → stop service
   - Test: Start service → send question → no response → verify timeout
   - Test: Start service → send checkin → verify message sent to room
   - Test: Start service → stop → verify farewell message sent

3. **Integration Tests — Operator Filtering:**
   - Test: Inject messages from operator and non-operator → verify only operator messages are processed
   - Test: Inject system messages → verify they are dropped
   - Test: Set `operator_id` to `None` → verify all non-system messages are processed

4. **Integration Tests — Multi-Loop Message Routing:**
   - Test: Two pending questions for different loops → thread reply routes to correct loop
   - Test: `@loop-id` prefix routes to correct loop
   - Test: Untagged message defaults to "main" loop

5. **Integration Tests — State Persistence:**
   - Test: Service processes messages → state saved → service restarted → state restored → continues from last sync timestamp

6. **CI Considerations:**
   - All tests must be CI-safe (no external dependencies)
   - Mock server starts/stops within each test
   - Use `tempdir` for workspace roots in tests

## Dependencies
- Task 2 — REST API client and types
- Task 3 — `RocketChatService`, `MessageHandler`, `StateManager`

## Implementation Approach
1. Create `crates/ralph-rocketchat/tests/` directory for integration tests
2. Create `crates/ralph-rocketchat/src/mock_server.rs` (or `tests/mock_server.rs`) with the mock HTTP server
3. Write integration tests using the mock server
4. Ensure all tests are `#[tokio::test]` where async is needed
5. Add test utilities for common setup (create temp workspace, start mock server, create service)

## Acceptance Criteria

1. **Mock server responds to all endpoints**
   - Given the mock server is running
   - When `chat.sendMessage`, `chat.syncMessages`, `me`, and `channels.info` are called
   - Then valid Rocket.Chat-format JSON responses are returned

2. **Mock server validates auth**
   - Given the mock server is running with expected credentials
   - When a request arrives with wrong `X-Auth-Token`
   - Then a 401 response is returned

3. **Full lifecycle integration test**
   - Given a `RocketChatService` connected to the mock server
   - When a question is sent and a response is injected
   - Then `wait_for_response()` returns the response text

4. **Operator filtering integration test**
   - Given `operator_id` is set to "human1"
   - When messages from "human1", "bot-user", and "human2" are injected
   - Then only "human1"'s messages appear as events

5. **Thread routing integration test**
   - Given pending questions for loops "main" and "feature-x"
   - When a thread reply to "feature-x"'s question message arrives
   - Then the response is routed to "feature-x"'s events file

6. **State persistence integration test**
   - Given a service that has processed messages
   - When the service is stopped and a new one is created with the same workspace
   - Then `last_sync` is restored and polling resumes from that timestamp

7. **All tests pass in CI**
   - Given no external Rocket.Chat server
   - When `cargo test -p ralph-rocketchat` is run
   - Then all tests pass using the mock server

## Metadata
- **Complexity**: High
- **Labels**: testing, rocketchat, integration, mock-server
- **Required Skills**: Rust, axum/warp, tokio, test infrastructure
