---
status: pending
created: 2026-03-08
started: null
completed: null
---
# Task: Add CLI Commands for Rocket.Chat

## Description
Extend the `ralph bot` CLI commands to support Rocket.Chat as a backend. This includes an onboarding wizard, status checking, message sending, and daemon mode for Rocket.Chat. Backend is auto-detected from existing configuration.

## Background
The current `ralph bot` commands in `crates/ralph-cli/src/bot.rs` are Telegram-specific: `onboard`, `status`, `send`, and `daemon`. These need to either auto-detect the configured backend or accept a backend-specific subcommand. The onboarding flow for Rocket.Chat differs from Telegram — instead of creating a bot via BotFather and waiting for a DM, it validates credentials against the RC API and lets the user pick a room.

## Reference Documentation
**Required:**
- `crates/ralph-cli/src/bot.rs` — existing Telegram CLI commands
- `crates/ralph-core/src/config.rs` — `RobotConfig` with backend detection

**Additional References:**
- Task 1 output — `RocketChatConfig` struct
- Task 2 output — REST API client (`get_me()`, `get_room_info()`)

## Technical Requirements
1. **`ralph bot onboard` for Rocket.Chat:**
   - Interactive wizard flow:
     a. Ask for Rocket.Chat server URL
     b. Ask for authentication method (personal access token recommended)
     c. Ask for `bot_user_id` and `auth_token` (or guide through token generation)
     d. Validate credentials by calling `GET /api/v1/me`
     e. List rooms the bot user is a member of (channels, groups, DMs)
     f. Let user select a room for communication
     g. Ask for operator user ID (the human to listen to)
     h. Store `auth_token` in OS keychain (fallback to config), save config to `ralph.yml`
   - Support `--server-url`, `--bot-user-id`, `--auth-token`, `--room-id`, `--operator-id` flags for non-interactive use

2. **`ralph bot status` — backend auto-detection:**
   - Read `ralph.yml` config
   - If `rocketchat` section present: validate RC credentials, show connection status, room info, operator info
   - If `telegram` section present: existing Telegram behavior
   - If neither: show "No RObot backend configured"

3. **`ralph bot send` — backend auto-detection:**
   - Detect backend from config
   - If `rocketchat`: send message via RC REST API to configured room
   - If `telegram`: existing behavior

4. **`ralph bot daemon` — backend auto-detection:**
   - Detect backend from config
   - If `rocketchat`: create `RocketChatDaemon` and run
   - If `telegram`: existing behavior

5. Add `--backend` override flag to `ralph bot onboard` to explicitly choose (e.g., `ralph bot onboard --backend rocketchat`)

## Dependencies
- Task 1 — `RocketChatConfig` in config
- Task 2 — REST API client for validation and room listing
- Task 3 — `RocketChatService` for send command
- Task 4 — `RocketChatDaemon` for daemon command

## Implementation Approach
1. Add Rocket.Chat onboarding function `onboard_rocketchat()` alongside existing `onboard_telegram()`
2. Modify `BotCommands::Onboard` to accept a `--backend` flag, defaulting to Telegram for backwards compatibility
3. Add helper functions: `detect_configured_backend()`, `resolve_chat_id_for_rocketchat()`
4. Update `status`, `send`, `daemon` subcommands to auto-detect backend
5. Add unit tests for backend detection and onboarding validation

## Acceptance Criteria

1. **Onboard Rocket.Chat**
   - Given the user runs `ralph bot onboard --backend rocketchat --server-url https://chat.example.com --bot-user-id bot123 --auth-token tok123 --room-id GENERAL --operator-id human456`
   - When the command completes
   - Then `ralph.yml` is updated with `RObot.rocketchat` config and `operator_id`

2. **Onboard validates credentials**
   - Given invalid Rocket.Chat credentials
   - When `ralph bot onboard --backend rocketchat` runs
   - Then it reports the validation failure with troubleshooting steps

3. **Status auto-detects backend**
   - Given `ralph.yml` has a `rocketchat` config section
   - When `ralph bot status` is run
   - Then it shows Rocket.Chat connection status, room info, and operator ID

4. **Send auto-detects backend**
   - Given `ralph.yml` has a `rocketchat` config section
   - When `ralph bot send "Hello from Ralph"` is run
   - Then the message is sent to the configured Rocket.Chat room

5. **Daemon auto-detects backend**
   - Given `ralph.yml` has a `rocketchat` config section
   - When `ralph bot daemon` is run
   - Then a `RocketChatDaemon` is started

6. **Backward compatibility**
   - Given `ralph.yml` has only a `telegram` config section
   - When any `ralph bot` command is run
   - Then existing Telegram behavior is preserved

7. **No backend configured**
   - Given `ralph.yml` has no `telegram` or `rocketchat` section
   - When `ralph bot status` is run
   - Then it shows "No RObot backend configured" with setup instructions

## Metadata
- **Complexity**: Medium
- **Labels**: cli, rocketchat, onboarding
- **Required Skills**: Rust, clap, interactive CLI, async
