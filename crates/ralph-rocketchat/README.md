# ralph-rocketchat

Rocket.Chat integration for human-in-the-loop orchestration in Ralph.

Enables bidirectional communication between AI agents and humans during orchestration loops:

- **AI to Human**: Agents emit `human.interact` events; the bot sends questions to Rocket.Chat
- **Human to AI**: Humans reply or send proactive `human.guidance` via Rocket.Chat messages

## Setup

### 1. Create a Bot User and Personal Access Token

1. In Rocket.Chat **Administration > Users**, create a bot user (e.g., `ralph-bot`)
2. Log in as the bot user
3. Go to **My Account > Personal Access Tokens**
4. Generate a new token — save both the **token** and the **user ID**
5. Note the bot user's **User ID** (visible in user admin or via `me` API endpoint)

### 2. Configure Ralph

**Option A: Environment variables (recommended)**

```bash
export RALPH_ROCKETCHAT_AUTH_TOKEN="your-personal-access-token"
export RALPH_ROCKETCHAT_SERVER_URL="https://your-rocketchat.example.com"
```

**Option B: Config file**

```yaml
# ralph.yml
RObot:
  enabled: true
  timeout_seconds: 300
  operator_id: "your-rocketchat-user-id"  # Required for group chats
  rocketchat:
    server_url: "https://your-rocketchat.example.com"
    bot_user_id: "bot-user-id"
    auth_token: "your-personal-access-token"
    room_id: "room-id"  # Channel or DM room ID
```

Environment variables take precedence over config file values.

**Option C: Interactive onboarding**

```bash
ralph bot onboard --backend rocketchat
```

The wizard walks through each configuration step interactively.

### 3. Start a Loop

```bash
ralph run -p "your prompt"
```

The bot sends a greeting to the configured room when it starts.

## Operator Filtering

In group chats, set `operator_id` to restrict which user the bot listens to. Only messages from the designated operator (and system messages) are processed; all other messages are silently dropped. This prevents noise from other channel members.

In DM conversations, `operator_id` is optional — all messages are from the operator.

## Bot Commands

Available commands while a loop is running:

- `/help` — list available commands
- `/status` — current loop status
- `/tasks` — open tasks
- `/memories` — recent memories
- `/tail` — last 20 events
- `/model` — current backend/model (runtime or config fallback)
- `/models` — configured model options found in `ralph*.yml`
- `/restart` — restart the loop
- `/stop` — stop the loop at the next iteration boundary

## How It Works

### human.interact Flow

When an agent emits a `human.interact` event:

1. The bot sends the question to the configured Rocket.Chat room
2. The event loop **blocks** waiting for a reply
3. The human replies in Rocket.Chat
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

1. **Thread reply (`tmid`)**: Replying in a thread routes to the loop that asked the question
2. **@prefix**: Starting a message with `@loop-id` routes to that loop
3. **Default**: Messages without routing go to the primary loop

## Architecture

```
RocketChatService (lifecycle management, RobotService impl)
├── RocketChatApi / RocketChatClient (REST client, send + sync messages)
├── StateManager (last_sync, pending questions, state persistence)
├── MessageHandler (incoming messages -> events.jsonl routing)
└── retry_with_backoff (exponential retry for sends)

RocketChatDaemon (DaemonAdapter impl, persistent listener)
├── RocketChatClient (polls for messages while idle)
├── commands (slash command parsing and response formatting)
└── loop_lock (flock-based primary loop detection)
```

### Key Types

| Type | Purpose |
|------|---------|
| `RocketChatService` | Lifecycle: start, stop, send questions, wait for responses |
| `RocketChatApi` | Trait for REST operations; `RocketChatClient` is the production impl |
| `RocketChatClient` | REST client using `reqwest` with `X-Auth-Token`/`X-User-Id` headers |
| `StateManager` | Persists state to `.ralph/rocketchat-state.json` |
| `MessageHandler` | Writes `human.response` / `human.guidance` events to JSONL |
| `RocketChatDaemon` | `DaemonAdapter` impl: polls while idle, delegates to loop service during runs |
| `RocketChatError` | Typed errors: Auth, Send, Receive, NotFound, RateLimit, Server, Deserialize, State, Startup |

### Key Data Types

| Type | Purpose |
|------|---------|
| `RcUser` | Rocket.Chat user with `id`, `username`, `name` |
| `RcMessage` | Message with `id`, `rid`, `msg`, user, optional `tmid` (thread) |
| `RcRoom` | Room metadata |
| `SyncResult` | Result of `chat.syncMessages`: updated + deleted message lists |
| `filter_by_operator` | Helper function to filter messages by operator ID |

## Config Reference

| Field | Required | Env Var | Description |
|-------|----------|---------|-------------|
| `server_url` | Yes | `RALPH_ROCKETCHAT_SERVER_URL` | Rocket.Chat server base URL |
| `bot_user_id` | Yes | — | Bot's user ID for `X-User-Id` header |
| `auth_token` | Yes | `RALPH_ROCKETCHAT_AUTH_TOKEN` | Personal Access Token |
| `room_id` | Yes | — | Target room/channel ID |
| `operator_id` | No | — | Human operator's user ID (for group chat filtering) |

## Error Handling

- **Send failures**: Retried with exponential backoff (3 attempts: 1s, 2s, 4s delays)
- **All retries exhausted**: Logged to diagnostics, treated as timeout (loop continues)
- **Auth errors**: Clear error on invalid token or expired credentials
- **Response timeout**: Configurable via `timeout_seconds`; loop continues without response
- **State persistence**: Atomic writes via temp file + rename to prevent corruption

## Testing

```bash
cargo test -p ralph-rocketchat     # 129 tests (unit + integration)
cargo test -p ralph-core robot     # Integration tests in ralph-core
```

Integration tests use `MockRocketChatClient` (in-process mock with call recording and response queues) and `MockRocketChatServer` (axum-based HTTP server for full REST API testing).

For a comprehensive setup and troubleshooting guide, see [docs/guide/rocketchat.md](../../docs/guide/rocketchat.md).
