# Rocket.Chat RObot Integration — Changelog

## 2026-03-08

### Added
- **`ralph-rocketchat` crate** — new crate providing Rocket.Chat integration for human-in-the-loop orchestration
  - `RocketChatClient` REST API client with Personal Access Token authentication
  - `RocketChatService` implementing `RobotService` trait (send questions, receive responses, background polling)
  - `RocketChatDaemon` implementing `DaemonAdapter` trait with slash commands (`/help`, `/status`, `/tasks`, `/memories`, `/tail`, `/model`, `/models`, `/restart`, `/stop`)
  - `MessageHandler` for event routing: thread-based (`tmid`), `@loop-id` prefix, and default-to-primary
  - `StateManager` with atomic file writes for persistent state across restarts
  - `MockRocketChatClient` for unit testing with call recording and response queues
  - `MockRocketChatServer` (axum-based) for integration testing against real HTTP
  - Operator filtering to restrict message processing to a designated human in group chats
- **Multi-backend RObot config** — `RobotConfig` now supports `telegram` and `rocketchat` sections with mutual exclusion validation
  - `RocketChatConfig` struct with `server_url`, `bot_user_id`, `auth_token`, `room_id`
  - `operator_id` field at `RobotConfig` level (shared across backends)
  - Environment variable fallbacks: `RALPH_ROCKETCHAT_SERVER_URL`, `RALPH_ROCKETCHAT_AUTH_TOKEN`
- **CLI commands** for Rocket.Chat
  - `ralph bot onboard --backend rocketchat` interactive setup wizard
  - `ralph bot status` and `ralph bot test` auto-detect configured backend
  - `detect_configured_backend()` helper for multi-backend routing
- **Integration tests** — 129 tests in `ralph-rocketchat` covering client, service, handler, state, and daemon
- **Documentation** — `crates/ralph-rocketchat/README.md`, `docs/guide/rocketchat.md` user guide, CLAUDE.md updates

### Changed
- `RobotConfig::validate()` refactored for multi-backend: rejects configs with both backends, validates per-backend fields
- `create_robot_service()` in `loop_runner.rs` dispatches on config section present
- `ralph tools interact progress` description updated to mention both Telegram and Rocket.Chat backends

### Crates Affected
- `ralph-rocketchat` (new)
- `ralph-core` (config changes)
- `ralph-cli` (CLI wiring, bot commands)
- `ralph-telegram` (minor: doc comment updates)

## Suggested AGENTS.md Updates

1. **Multi-backend RObot pattern**: When adding new RObot backends, implement `RobotService` (for orchestration loop) and `DaemonAdapter` (for standalone bot mode) traits from `ralph-proto`. Add mutual exclusion validation in `RobotConfig::validate()`.

2. **REST polling over WebSocket**: For human-in-the-loop use cases, REST polling (`chat.syncMessages`) is preferred over WebSocket/DDP — simpler, sufficient latency for human response times, and easier to test with mock HTTP servers.

3. **Thread-based loop routing**: Multi-loop message routing uses platform-native threading (Telegram reply-to-message-id, Rocket.Chat `tmid`). Fallback: `@loop-id` prefix in message text, then default to primary loop.

4. **Atomic state persistence**: State files (`.ralph/rocketchat-state.json`, `.ralph/telegram-state.json`) use write-to-temp-then-rename for crash safety. Follow this pattern for any new persistent state.

5. **Integration test pattern**: Use axum-based mock servers for HTTP API integration tests. See `crates/ralph-rocketchat/tests/common/mod.rs` for the reusable `MockRocketChatServer` pattern.
