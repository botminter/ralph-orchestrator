# Rocket.Chat Integration

Ralph supports human-in-the-loop communication via Rocket.Chat as an alternative to Telegram. Agents can ask questions during orchestration, and humans can send proactive guidance at any time — all through a Rocket.Chat bot.

## Prerequisites

- A running Rocket.Chat server (self-hosted or cloud)
- Admin access to create a bot user and generate a Personal Access Token (PAT)
- The room ID where the bot should operate (channel or DM)

## Setup

### 1. Create a Bot User

1. Log in to Rocket.Chat as an administrator
2. Go to **Administration > Users > New**
3. Create a user for the bot (e.g., username: `ralph-bot`, name: `Ralph`)
4. Set a password and assign appropriate roles (at minimum, the `bot` role)
5. Note the bot user's **User ID** (visible in the user admin panel, or via the `me` API endpoint)

### 2. Generate a Personal Access Token

1. Log in as the bot user
2. Go to **My Account > Personal Access Tokens**
3. Create a new token with a descriptive name (e.g., `ralph-orchestrator`)
4. Save both the **token** and the **user ID** — the token is only shown once

Personal Access Tokens are preferred over password-based auth because they:
- Persist across password changes
- Bypass 2FA requirements
- Can be revoked individually
- Are the recommended auth method for bots

### 3. Get the Room ID

The room ID is needed to tell Ralph where to send messages. You can find it:

- **From the URL**: In most Rocket.Chat clients, the room ID appears in the URL when you open a channel
- **Via API**: `curl -H "X-Auth-Token: TOKEN" -H "X-User-Id: USER_ID" https://your-server/api/v1/channels.list` lists channels with their `_id` fields
- **During onboarding**: The `ralph bot onboard` wizard can help you find it

### 4. Configure Ralph

There are three ways to configure the Rocket.Chat integration:

**Option A: Interactive onboarding (recommended for first-time setup)**

```bash
ralph bot onboard --backend rocketchat
```

The wizard walks through each step interactively — server URL, bot user ID, auth token, room ID, and operator ID.

**Option B: Environment variables + config file**

```bash
export RALPH_ROCKETCHAT_AUTH_TOKEN="your-personal-access-token"
export RALPH_ROCKETCHAT_SERVER_URL="https://your-rocketchat.example.com"
```

```yaml
# ralph.yml
RObot:
  enabled: true
  timeout_seconds: 300
  operator_id: "your-user-id"       # Required for group chats
  rocketchat:
    server_url: "https://your-rocketchat.example.com"
    bot_user_id: "bot-user-id"
    auth_token: "your-personal-access-token"
    room_id: "target-room-id"
```

Environment variables take precedence over config file values.

**Option C: Config file only**

```yaml
# ralph.yml
RObot:
  enabled: true
  timeout_seconds: 300
  operator_id: "your-user-id"
  rocketchat:
    server_url: "https://your-rocketchat.example.com"
    bot_user_id: "bot-user-id"
    auth_token: "your-personal-access-token"
    room_id: "target-room-id"
```

### 5. Start a Loop

```bash
ralph run -p "your prompt"
```

The bot sends a greeting message to the configured room on startup.

## Configuration Reference

```yaml
RObot:
  enabled: true                    # Enable human-in-the-loop (default: false)
  timeout_seconds: 300             # How long to block waiting for a response
  checkin_interval_seconds: 120    # Periodic status updates (optional)
  operator_id: "user-id"          # Filter messages to this user (for group chats)
  rocketchat:
    server_url: "https://chat.example.com"  # Or RALPH_ROCKETCHAT_SERVER_URL env var
    bot_user_id: "bot-user-id"              # Bot's Rocket.Chat user ID
    auth_token: "pat-token"                 # Or RALPH_ROCKETCHAT_AUTH_TOKEN env var
    room_id: "room-id"                      # Target channel or DM room ID
```

| Field | Required | Env Var | Description |
|-------|----------|---------|-------------|
| `enabled` | Yes | — | Must be `true` to activate Rocket.Chat |
| `timeout_seconds` | Yes | — | Seconds to wait for a human reply before continuing |
| `checkin_interval_seconds` | No | — | Send periodic "still working" status updates |
| `operator_id` | No* | — | Human operator's user ID (*required for group chats) |
| `rocketchat.server_url` | Yes | `RALPH_ROCKETCHAT_SERVER_URL` | Rocket.Chat server base URL |
| `rocketchat.bot_user_id` | Yes | — | Bot's user ID for `X-User-Id` API header |
| `rocketchat.auth_token` | Yes | `RALPH_ROCKETCHAT_AUTH_TOKEN` | Personal Access Token |
| `rocketchat.room_id` | Yes | — | Target room/channel ID |

For long-running loops, increase `timeout_seconds` and set `checkin_interval_seconds`:

```yaml
RObot:
  enabled: true
  timeout_seconds: 43200            # 12 hours
  checkin_interval_seconds: 900     # Check in every 15 minutes
  rocketchat:
    # ...
```

## How It Works

### Agent Asks a Question (`human.interact`)

When an agent emits a `human.interact` event during orchestration:

1. The bot formats the question with context (hat name, iteration, loop ID) and sends it to the configured Rocket.Chat room
2. The event loop **blocks**, waiting for a reply
3. You reply in Rocket.Chat
4. Your reply is published as a `human.response` event
5. The next iteration receives your response in its context

If no reply arrives within `timeout_seconds`, the loop continues without a response.

### You Send Proactive Guidance (`human.guidance`)

You can send messages at any time (not as replies to a question):

1. Your message is written as a `human.guidance` event to `events.jsonl`
2. On the next iteration, all guidance events are collected and squashed into a numbered list
3. A `## ROBOT GUIDANCE` section is injected into the agent's prompt

This lets you steer the agent without waiting for it to ask.

### Event Summary

| Event | Direction | Behavior |
|-------|-----------|----------|
| `human.interact` | Agent to Human | Agent asks a question; loop blocks until reply or timeout |
| `human.response` | Human to Agent | Your reply to a `human.interact` question |
| `human.guidance` | Human to Agent | Proactive message injected into agent's next prompt |

## Operator Filtering

In group chats (channels with multiple users), set `operator_id` to restrict which user the bot listens to. Only messages from the designated operator are processed — all other messages are silently dropped. This prevents noise from other channel members from being injected into the agent's context.

```yaml
RObot:
  operator_id: "abc123def456"    # Your Rocket.Chat user ID
  rocketchat:
    room_id: "general-channel"   # A group channel
    # ...
```

In DM (direct message) conversations, `operator_id` is optional since all messages come from the operator by default.

**Finding your user ID**: Check **My Account** in Rocket.Chat, or query the API:

```bash
curl -H "X-Auth-Token: TOKEN" -H "X-User-Id: BOT_USER_ID" \
  https://your-server/api/v1/users.info?username=your-username
```

## Group Chat vs DM Patterns

| Pattern | Config | Behavior |
|---------|--------|----------|
| **DM** | `room_id` = DM room ID, no `operator_id` | All messages processed (only you and the bot) |
| **Group + filter** | `room_id` = channel ID, `operator_id` set | Only operator's messages processed |
| **Group, no filter** | `room_id` = channel ID, no `operator_id` | All messages from all users processed (noisy) |

**Recommendation**: For group channels, always set `operator_id`. For DMs, it's optional.

## Parallel Loop Routing

When running multiple loops in parallel (via worktrees), messages are routed by priority:

1. **Thread reply (`tmid`)**: Replying in a Rocket.Chat thread routes to the loop that asked the question
2. **@prefix**: Starting a message with `@loop-id` routes to that specific loop
3. **Default**: Messages without routing go to the primary loop

Examples:

- Reply in a thread to a bot question -> routed to the loop that asked
- Send `@feature-auth check the edge cases` -> routed to the `feature-auth` loop
- Send `focus on tests` -> routed to the primary (main) loop

Each loop has its own `events.jsonl`:
- Primary loop: `.ralph/events.jsonl`
- Worktree loops: `.worktrees/<loop-id>/.ralph/events.jsonl`

## Bot Commands

When running in daemon mode (`ralph bot`), the following slash commands are available:

| Command | Description |
|---------|-------------|
| `/help` | List available commands |
| `/status` | Current loop status |
| `/tasks` | Open runtime tasks |
| `/memories` | Recent memories |
| `/tail` | Last 20 events |
| `/model` | Current backend and model |
| `/models` | Configured model options from `ralph*.yml` |
| `/restart` | Restart the loop |
| `/stop` | Stop the loop at the next iteration boundary |

## Docker Compose Testing

You can spin up a local Rocket.Chat instance for testing the integration:

```yaml
# docker-compose.yml
services:
  rocketchat:
    image: registry.rocket.chat/rocketchat/rocket.chat:latest
    restart: unless-stopped
    environment:
      MONGO_URL: "mongodb://mongodb:27017/rocketchat?replicaSet=rs0"
      MONGO_OPLOG_URL: "mongodb://mongodb:27017/local?replicaSet=rs0"
      ROOT_URL: "http://localhost:3000"
      PORT: "3000"
    depends_on:
      - mongodb
    ports:
      - "3000:3000"

  mongodb:
    image: docker.io/bitnami/mongodb:5.0
    restart: unless-stopped
    volumes:
      - mongodb_data:/bitnami/mongodb
    environment:
      MONGODB_REPLICA_SET_MODE: primary
      MONGODB_REPLICA_SET_NAME: rs0
      MONGODB_PORT_NUMBER: "27017"
      ALLOW_EMPTY_PASSWORD: "yes"

volumes:
  mongodb_data:
```

```bash
docker compose up -d
# Wait for Rocket.Chat to start (first boot takes a minute)
# Then open http://localhost:3000, complete setup wizard, create bot user + PAT
```

Once the server is running, follow the setup steps above using `http://localhost:3000` as your `server_url`.

## Error Handling

| Scenario | Behavior |
|----------|----------|
| Send failure | Retried with exponential backoff: 1s, 2s, 4s (3 attempts) |
| All retries fail | Logged to diagnostics, treated as timeout (loop continues) |
| Auth failure (401) | Clear error identifying invalid token or expired credentials |
| Missing config field | Validation error listing the missing field and how to set it |
| Response timeout | Configurable via `timeout_seconds`; loop continues without response |
| State file corruption | Atomic writes via temp file + rename prevent partial writes |

## State File

The bot persists its state to `.ralph/rocketchat-state.json`:

```json
{
  "last_sync": "2026-03-08T10:00:00Z",
  "pending_questions": {
    "main": {
      "asked_at": "2026-03-08T10:05:00Z",
      "message_id": "msg-abc123"
    }
  }
}
```

- `last_sync`: Timestamp of the last successful message sync (used by `chat.syncMessages`)
- `pending_questions`: Tracks which loops have outstanding questions, used for reply routing

## Comparison with Telegram

| Feature | Telegram | Rocket.Chat |
|---------|----------|-------------|
| **Auth method** | Bot token (via BotFather) | Personal Access Token |
| **Message receiving** | Long polling (`getUpdates`) | REST polling (`chat.syncMessages`) |
| **Thread routing** | Reply-to message ID | Thread ID (`tmid`) |
| **Chat ID detection** | Auto-detected from first message | Configured via `room_id` |
| **Operator filtering** | Not needed (DM-only) | `operator_id` for group chats |
| **Setup complexity** | Lower (BotFather wizard) | Higher (admin access needed) |
| **Self-hosted** | No (Telegram servers only) | Yes (full control) |
| **Multimedia** | Photos + documents | Text only |
| **Onboarding** | `ralph bot onboard --backend telegram` | `ralph bot onboard --backend rocketchat` |
| **State file** | `.ralph/telegram-state.json` | `.ralph/rocketchat-state.json` |

**When to choose Rocket.Chat over Telegram**:
- Your team already uses Rocket.Chat as their primary chat platform
- You need a self-hosted solution for compliance or security reasons
- You want to use group channels with operator filtering for team visibility
- You need to keep all communication within your infrastructure

**When to choose Telegram**:
- Quick setup with minimal infrastructure
- Mobile-first workflow (Telegram's mobile app is lightweight)
- You don't need self-hosted chat infrastructure

## Troubleshooting

### Auth failures (401 errors)

- Verify your Personal Access Token is still valid — tokens can be revoked from the admin panel
- Check that `bot_user_id` matches the user who generated the token
- Confirm the server URL is correct and accessible: `curl https://your-server/api/v1/info`
- If using env vars, check they're set: `echo $RALPH_ROCKETCHAT_AUTH_TOKEN`

### Room not found

- Verify the `room_id` is correct — it's the internal ID, not the room name
- Check that the bot user has been added to the room/channel
- For private channels, the bot must be explicitly invited
- Query available rooms: `curl -H "X-Auth-Token: TOKEN" -H "X-User-Id: USER_ID" https://your-server/api/v1/channels.list.joined`

### Messages not received

- Check `operator_id` — if set, only messages from that user are processed
- Verify the bot is polling the correct room (check `room_id` in config)
- Look at `.ralph/rocketchat-state.json` for the `last_sync` timestamp
- Check diagnostics logs: `RALPH_DIAGNOSTICS=1 ralph run -p "test"`

### Bot doesn't start

- Ensure `RObot.enabled: true` is set in your config
- Check that only one backend is configured (cannot use both Telegram and Rocket.Chat)
- Verify all required fields are set: `server_url`, `bot_user_id`, `auth_token`, `room_id`
- Run validation: `ralph bot status` will report config errors

### Messages go to the wrong loop

- Use thread replies for routing to the loop that asked the question
- Use `@loop-id` prefix to target a specific loop
- Unrouted messages default to the primary loop

### Timeout before you can respond

- Increase `timeout_seconds` in your config
- For long tasks, set `checkin_interval_seconds` so you know the loop is still active

## Testing

```bash
cargo test -p ralph-rocketchat     # 129 tests (unit + integration, no network)
cargo test -p ralph-core robot     # Integration tests in ralph-core
```

All tests use mock clients and an in-process mock HTTP server — no Rocket.Chat instance is needed for testing.
