# ralph-matrix

Matrix integration for human-in-the-loop orchestration in Ralph.

Enables bidirectional communication between AI agents and humans during orchestration loops:

- **AI to Human**: Agents emit `human.interact` events; the bot sends questions to Matrix
- **Human to AI**: Humans reply or send proactive `human.guidance` via Matrix messages

## Setup

### 1. Create a Bot Account and Get an Access Token

1. Create a bot user on your Matrix homeserver (e.g., `@ralph-bot:example.com`)
2. Obtain an access token via one of:
   - **API login**: `curl -X POST https://matrix.example.com/_matrix/client/v3/login -d '{"type":"m.login.password","user":"ralph-bot","password":"..."}'`
   - **Admin tools**: Use your homeserver's admin API or web UI to generate a token
   - **Element**: Log in as the bot, go to Settings > Help & About > Access Token
3. Note the bot user's Matrix ID (e.g., `@ralph-bot:example.com`)

### 2. Configure Ralph

**Option A: Environment variables (recommended)**

```bash
export RALPH_MATRIX_ACCESS_TOKEN="syt_your_access_token_here"
export RALPH_MATRIX_HOMESERVER_URL="https://matrix.example.com"
```

**Option B: Config file**

```yaml
# ralph.yml
RObot:
  enabled: true
  timeout_seconds: 300
  operator_id: "@your-user:example.com"  # Required for group chats
  matrix:
    homeserver_url: "https://matrix.example.com"
    room_id: "!abc123:example.com"
    access_token: "syt_your_access_token_here"
```

Environment variables take precedence over config file values. Access tokens are also resolved from the OS keychain as a third tier.

**Option C: Interactive onboarding**

```bash
ralph bot onboard --backend matrix
```

The wizard walks through each configuration step interactively.

### 3. Test the Connection

```bash
ralph bot test --backend matrix
```

### 4. Start a Loop

```bash
ralph run -p "your prompt"
```

The bot sends a greeting to the configured room when it starts.

## Operator Filtering

In group chats, set `operator_id` to restrict which user the bot listens to. Only messages from the designated operator are processed; all other messages are silently dropped. This prevents noise from other room members.

In DM conversations, `operator_id` is optional — all messages are from the operator.

## Bot Commands

Available commands while a loop is running (note the `!` prefix, not `/`):

- `!help` — list available commands
- `!status` — current loop status
- `!tasks` — open tasks
- `!memories` — recent memories
- `!tail` — last 20 events
- `!model` — current backend/model (runtime or config fallback)
- `!models` — configured model options found in `ralph*.yml`
- `!restart` — restart the loop
- `!stop` — stop the loop at the next iteration boundary

Matrix reserves `/` for client-side commands, so the Matrix backend uses `!` as the command prefix.

## How It Works

### human.interact Flow

When an agent emits a `human.interact` event:

1. The bot sends the question to the configured Matrix room
2. The event loop **blocks** waiting for a reply
3. The human replies in Matrix
4. The reply is published as a `human.response` event on the bus
5. The next iteration receives the response in its context

If no response arrives within `timeout_seconds`, the loop continues without a response.

### human.guidance Flow

Humans can send messages at any time (not as replies to questions):

1. Message is written as a `human.guidance` event to `events.jsonl`
2. On the next iteration, guidance events are collected and squashed
3. A `## ROBOT GUIDANCE` section is injected into the agent's prompt

### Parallel Loop Routing

With multiple loops running, messages are routed by:

1. **Reply-to event ID**: Replying to a bot question routes to the loop that asked it
2. **@prefix**: Starting a message with `@loop-id` routes to that loop
3. **Default**: Messages without routing go to the primary loop

## Architecture

```
MatrixService (lifecycle management, RobotService impl)
├── MatrixApi / MatrixClient (matrix-sdk wrapper, send + sync messages)
├── StateManager (since_token, pending questions, state persistence)
├── MessageHandler (incoming messages -> events.jsonl routing)
└── retry_with_backoff (exponential retry for sends)

MatrixDaemon (DaemonAdapter impl, persistent listener)
├── MatrixClient (polls via sync_once while idle)
├── commands (bot command parsing and response formatting)
└── loop_lock (flock-based primary loop detection)
```

### Key Types

| Type | Purpose |
|------|---------|
| `MatrixService` | Lifecycle: start, stop, send questions, wait for responses |
| `MatrixApi` | Trait for Matrix operations; `MatrixClient` is the production impl |
| `MatrixClient` | `matrix-sdk` wrapper with login, send, sync, room info |
| `MockMatrixClient` | In-process mock with call recording and response queues |
| `StateManager` | Persists state to `.ralph/matrix-state.json` |
| `MessageHandler` | Writes `human.response` / `human.guidance` events to JSONL |
| `MatrixDaemon` | `DaemonAdapter` impl: polls while idle, delegates to loop service during runs |
| `MatrixError` | Typed errors: Auth, Network, RoomNotFound, SendFailed, SyncFailed, State, EventWrite, Startup |

### Key Data Types

| Type | Purpose |
|------|---------|
| `MatrixMessage` | Message with `sender_id`, `body`, `event_id`, `timestamp`, `room_id`, optional `reply_to_event_id` |
| `SyncResult` | Result of a `/sync` poll: list of new messages |
| `RoomInfo` | Room metadata: `room_id`, `name`, `topic`, `member_count` |
| `MatrixState` | Persisted state: `since_token`, `room_id`, `pending_questions` |
| `PendingQuestion` | A question awaiting response: `loop_id`, `question`, `asked_at` |
| `filter_by_operator` | Helper function to filter messages by operator ID |

## Config Reference

| Field | Required | Env Var | Description |
|-------|----------|---------|-------------|
| `homeserver_url` | Yes | `RALPH_MATRIX_HOMESERVER_URL` | Matrix homeserver base URL |
| `room_id` | Yes | — | Target room ID (e.g., `!abc:example.com`) |
| `access_token` | Yes | `RALPH_MATRIX_ACCESS_TOKEN` | Bot access token (also resolved from OS keychain) |
| `operator_id` | No | — | Human operator's Matrix ID (for group chat filtering) |

## Error Handling

- **Send failures**: Retried with exponential backoff (3 attempts: 1s, 2s, 4s delays)
- **All retries exhausted**: Logged to diagnostics, treated as timeout (loop continues)
- **Auth errors**: Clear error on invalid token or expired credentials
- **Response timeout**: Configurable via `timeout_seconds`; loop continues without response
- **State persistence**: Atomic writes via temp file + rename to prevent corruption

## Testing

```bash
cargo test -p ralph-matrix         # 111 tests (unit + integration)
cargo test -p ralph-core robot     # Integration tests in ralph-core
```

Tests use `MockMatrixClient` (in-process mock with call recording and response queues) for full isolation from any homeserver.
