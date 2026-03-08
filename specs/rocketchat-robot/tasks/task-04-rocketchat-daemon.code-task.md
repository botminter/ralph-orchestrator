---
status: pending
created: 2026-03-08
started: null
completed: null
---
# Task: Implement DaemonAdapter for Rocket.Chat

## Description
Implement `ralph_proto::DaemonAdapter` for Rocket.Chat, enabling a persistent bot process that listens for messages in a Rocket.Chat room and spawns orchestration loops on demand. This mirrors the `TelegramDaemon` pattern.

## Background
The daemon mode runs independently of any orchestration loop. It idles in a Rocket.Chat room, waiting for the operator to send a message. When a message arrives (and no loop is currently running), it starts a new loop with that message as the prompt. While a loop is running, the daemon yields message polling to the loop's `RocketChatService` (turn-taking model, same as `TelegramDaemon`).

The `DaemonAdapter` trait is defined in `crates/ralph-proto/src/daemon.rs` with a single method:
```rust
async fn run_daemon(&self, workspace_root: PathBuf, start_loop: StartLoopFn) -> anyhow::Result<()>;
```

## Reference Documentation
**Required:**
- `crates/ralph-proto/src/daemon.rs` — `DaemonAdapter` trait
- `crates/ralph-telegram/src/daemon.rs` — `TelegramDaemon` reference implementation

**Additional References:**
- `crates/ralph-cli/src/bot.rs` — daemon CLI wiring
- Task 2 output — REST API client
- Task 3 output — `RocketChatService` and message handling

## Technical Requirements
1. Create `RocketChatDaemon` struct:
   - `server_url: String`, `auth_token: String`, `bot_user_id: String`
   - `room_id: String`
   - `operator_id: Option<String>`

2. Implement `DaemonAdapter` trait:
   - `run_daemon()` — main loop:
     a. Create REST API client
     b. Send greeting message to room
     c. Poll `chat.syncMessages` while idle
     d. Filter by `operator_id`, skip system messages
     e. Route slash commands through shared command handler
     f. On operator message (not a command) and no active loop: start loop with message as prompt
     g. While loop is running: yield polling (don't poll, let the loop's service handle it)
     h. When loop completes: resume idle polling
     i. Handle SIGINT/SIGTERM gracefully

3. Reuse shared command handling patterns (can extract common command logic from Telegram or duplicate for now)

4. Turn-taking model: daemon polls when idle, loop's `RocketChatService` polls when active. Use an `AtomicBool` or similar to coordinate.

## Dependencies
- Task 2 — REST API client
- Task 3 — `RocketChatService`, message handler, state management

## Implementation Approach
1. Create `daemon.rs` in `crates/ralph-rocketchat/src/`
2. Implement `RocketChatDaemon` struct
3. Implement the idle polling loop with operator filtering
4. Wire turn-taking with the loop's service
5. Handle graceful shutdown
6. Wire into CLI `ralph bot daemon` command in `bot.rs`

## Acceptance Criteria

1. **Idle polling**
   - Given the daemon is running with no active loop
   - When the operator sends a message
   - Then the daemon receives it via `chat.syncMessages`

2. **Loop start on message**
   - Given the daemon is idle
   - When the operator sends "Add a login page"
   - Then a new orchestration loop starts with that prompt

3. **Command handling while idle**
   - Given the daemon is idle
   - When the operator sends "/status"
   - Then the daemon responds with status info (no loop started)

4. **Turn-taking**
   - Given a loop is active
   - When the daemon's polling loop runs
   - Then it yields and does not poll for messages (the loop's service handles polling)

5. **Loop completion**
   - Given a loop completes
   - When the daemon detects loop completion
   - Then it resumes idle polling and sends a completion message

6. **Graceful shutdown**
   - Given the daemon is running
   - When SIGINT is received
   - Then polling stops, farewell message is sent, process exits cleanly

7. **Operator filtering in daemon mode**
   - Given `operator_id` is set
   - When messages arrive from non-operator users
   - Then they are silently dropped (no loop started)

## Metadata
- **Complexity**: Medium
- **Labels**: rocketchat, daemon, robot
- **Required Skills**: Rust, async/tokio, signal handling
