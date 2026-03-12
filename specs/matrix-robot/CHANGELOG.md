# Matrix RObot Integration — Changelog

## 2026-03-12

### Added
- **`ralph-matrix` crate** — new crate providing Matrix integration for human-in-the-loop orchestration
  - `MatrixApi` trait (7 methods) with `MatrixClient` (matrix-sdk wrapper) and `MockMatrixClient` for testing
  - `MatrixService` implementing `RobotService` trait (send questions, receive responses, background polling)
  - `MatrixDaemon` implementing `DaemonAdapter` trait with bot commands (`!help`, `!status`, `!tasks`, `!memories`, `!tail`, `!model`, `!models`, `!restart`, `!stop`)
  - `MessageHandler` for event routing: reply-to-based loop routing and default-to-primary fallback
  - `StateManager` with atomic file writes for persistent state (`.ralph/matrix-state.json`)
  - `MockMatrixClient` for unit testing with call recording and response queues
  - Operator filtering to restrict message processing to a designated human in group chats
  - `HandleResult` enum for clean command dispatch in daemon/handler
- **`MatrixConfig` in ralph-core** — Matrix backend configuration with credential resolution
  - `homeserver_url`, `access_token`, `room_id`, `bot_user_id` fields
  - `resolve_matrix_access_token()` with 3-tier resolution (config → env → error)
  - `resolve_matrix_homeserver_url()` with 2-tier resolution (config → env)
  - Count-based mutual exclusivity validation (rejects configs with multiple backends)
  - 13 new config validation tests
- **CLI commands** for Matrix
  - `ralph bot onboard --backend matrix` interactive setup wizard
  - `ralph bot status` and `ralph bot test` auto-detect Matrix backend
  - `ralph bot token --backend matrix` for token management
  - Backend-aware `interact.rs` for progress notifications via Matrix
- **102+ tests** in `ralph-matrix` across 7 source files (client, service, handler, state, daemon, commands, types)
- **Documentation** — `crates/ralph-matrix/README.md`, CLAUDE.md updates (4 sections: Architecture, Key Files, Code Locations, RObot config example)

### Changed
- `RobotConfig::validate()` updated for 3-backend mutual exclusion (count-based approach)
- `create_robot_service()` in `loop_runner.rs` dispatches on Matrix config section
- `run_daemon()` in `bot.rs` includes `MatrixDaemon` in adapter chain
- `detect_configured_backend()` extended to detect Matrix
- `bot_token_set()` made backend-aware with `--backend` flag
- `ralph tools interact progress` description updated to include Matrix backend
- AGENTS.md updated with `ralph-matrix` in Architecture table

### Crates Affected
- `ralph-matrix` (new)
- `ralph-core` (config: `MatrixConfig`, validation)
- `ralph-cli` (CLI wiring, bot commands, interact, daemon)
- `ralph-proto` (no changes — reuses existing `RobotService` and `DaemonAdapter` traits)

## Suggested AGENTS.md Updates

1. **Three-backend RObot pattern**: RObot now supports three backends (Telegram, Rocket.Chat, Matrix). When adding future backends, follow the established pattern: implement `RobotService` + `DaemonAdapter` traits, add config section to `RobotConfig`, use count-based mutual exclusion in `validate()`.

2. **Command prefix convention by platform**: Telegram uses `/command` (slash), Rocket.Chat uses `/command` (slash), Matrix uses `!command` (bang). This matches each platform's native bot command convention.

3. **Reply-to loop routing**: Matrix uses reply-to event IDs for multi-loop message routing (same as Telegram's reply-to-message-id pattern). This differs from Rocket.Chat's thread-based `tmid` approach. Fallback chain is consistent: reply-to → `@loop-id` prefix → default to primary.

4. **MockClient pattern for bot backends**: All three bot crates use the same testing pattern — a trait defining the API surface, a real client, and a mock client with call recording and configurable response queues. This enables unit testing without network calls.

5. **`!` prefix for Matrix commands**: Matrix commands use `!` instead of `/` because Matrix reserves `/` for client-side slash commands. This is documented in `crates/ralph-matrix/src/commands.rs`.
