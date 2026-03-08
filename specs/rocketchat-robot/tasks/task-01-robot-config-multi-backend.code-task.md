---
status: pending
created: 2026-03-08
started: null
completed: null
---
# Task: Extend RObot Config for Multi-Backend and Operator Filtering

## Description
Add backend selection and operator filtering to the RObot configuration system. Instead of an explicit `backend` field, detect which backend is configured by checking which config section is present (`telegram` or `rocketchat`). Error if both or neither are configured when RObot is enabled. Add `operator_id` for filtering messages in group chats.

## Background
The current `RobotConfig` in `crates/ralph-core/src/config.rs` has a `telegram` field with `TelegramBotConfig`. The `create_robot_service()` factory in `crates/ralph-cli/src/loop_runner.rs` is hardcoded to create a `TelegramService`. We need to make this generic so it dispatches based on which config section is present, and add operator filtering support for group chat scenarios.

## Reference Documentation
**Required:**
- Design: This task file and the Rocket.Chat API research below

**Additional References:**
- `crates/ralph-core/src/config.rs` — `RobotConfig` (line ~1756), `TelegramBotConfig` (line ~1852)
- `crates/ralph-cli/src/loop_runner.rs` — `create_robot_service()` (line ~4517)
- `crates/ralph-proto/src/robot.rs` — `RobotService` trait
- `crates/ralph-telegram/src/service.rs` — existing `TelegramService` for reference

## Technical Requirements
1. Add `operator_id: Option<String>` field to `RobotConfig` — identifies the human operator whose messages to process (required for group chats, optional for Telegram DMs)
2. Add `rocketchat: Option<RocketChatConfig>` field to `RobotConfig`
3. Create `RocketChatConfig` struct with fields:
   - `server_url: Option<String>` — Rocket.Chat server URL
   - `bot_user_id: Option<String>` — Bot's own RC user `_id` (used for `X-User-Id` header)
   - `auth_token: Option<String>` — Personal access token (used for `X-Auth-Token` header)
   - `room_id: Option<String>` — Primary room ID for communication
4. Add validation in `RobotConfig::validate()`:
   - Error if both `telegram` and `rocketchat` are configured
   - Error if neither is configured when `enabled: true`
   - Validate Rocket.Chat token availability (env var `RALPH_ROCKETCHAT_AUTH_TOKEN` > config > OS keychain)
   - Validate `server_url` and `bot_user_id` are present when `rocketchat` is configured
5. Add `resolve_rocketchat_auth_token()` method following the same pattern as `resolve_bot_token()`
6. Add `resolve_rocketchat_server_url()` method (env var `RALPH_ROCKETCHAT_SERVER_URL` > config)
7. Refactor `create_robot_service()` in `loop_runner.rs` to:
   - Check which config section is present
   - Dispatch to `TelegramService::new()` or (placeholder for now) `RocketChatService::new()`
   - Pass `operator_id` to the service constructor
8. Update YAML config deserialization to handle the new fields

## Dependencies
- None — this is the foundation task

## Implementation Approach
1. Add `RocketChatConfig` struct to `config.rs` with `Serialize`/`Deserialize` derives
2. Add `rocketchat` and `operator_id` fields to `RobotConfig`
3. Extend `validate()` with mutual exclusion check and RC-specific validation
4. Add `resolve_rocketchat_auth_token()` and `resolve_rocketchat_server_url()` methods
5. Update `create_robot_service()` to match on config sections — use a compile-time `cfg` gate or runtime check for the `ralph-rocketchat` dependency (can start with a `todo!()` branch for RC until Task 3 implements it)
6. Add unit tests for all validation scenarios

## Acceptance Criteria

1. **Mutual exclusion validation**
   - Given a config with both `telegram` and `rocketchat` sections
   - When `RobotConfig::validate()` is called
   - Then it returns an error indicating only one backend can be configured

2. **Missing backend validation**
   - Given a config with `RObot.enabled: true` but neither `telegram` nor `rocketchat`
   - When `RobotConfig::validate()` is called
   - Then it returns an error indicating a backend must be configured

3. **Telegram-only still works**
   - Given a config with only `telegram` configured (no `rocketchat`)
   - When `create_robot_service()` is called
   - Then a `TelegramService` is created (existing behavior preserved)

4. **RocketChat config validation**
   - Given a config with `rocketchat` section but missing `server_url`
   - When `RobotConfig::validate()` is called
   - Then it returns an error about the missing required field

5. **Token resolution for RocketChat**
   - Given `RALPH_ROCKETCHAT_AUTH_TOKEN` env var is set
   - When `resolve_rocketchat_auth_token()` is called
   - Then the env var value is returned (takes precedence over config)

6. **Operator ID passthrough**
   - Given a config with `operator_id: "user123"`
   - When the config is deserialized
   - Then `robot_config.operator_id` equals `Some("user123".to_string())`

7. **Existing tests pass**
   - Given all changes are complete
   - When `cargo test` is run
   - Then all existing tests pass without modification

## Metadata
- **Complexity**: Medium
- **Labels**: config, robot, multi-backend
- **Required Skills**: Rust, serde, YAML configuration
