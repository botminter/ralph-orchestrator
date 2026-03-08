---
status: pending
created: 2026-03-08
started: null
completed: null
---
# Task: Documentation

## Description
Create comprehensive documentation for the Rocket.Chat RObot integration, including crate README, user guide, and updates to existing project documentation. Mirrors the documentation structure of the Telegram integration.

## Background
The Telegram integration has:
- `crates/ralph-telegram/README.md` — crate-level setup guide
- `docs/guide/telegram.md` — full user guide with setup, configuration, testing
- `CLAUDE.md` — project-level architecture docs referencing Telegram
- Inline doc comments on all public types

The Rocket.Chat integration needs equivalent documentation. Additionally, the RObot section of `CLAUDE.md` needs updating to reflect multi-backend support.

## Reference Documentation
**Required:**
- `crates/ralph-telegram/README.md` — template for crate README
- `docs/guide/telegram.md` — template for user guide
- `CLAUDE.md` — sections to update (Architecture, RObot, Configuration)

**Additional References:**
- All task outputs (Tasks 1-6) for accurate documentation of features

## Technical Requirements

1. **`crates/ralph-rocketchat/README.md`:**
   - Overview: Rocket.Chat integration for human-in-the-loop orchestration
   - Setup steps:
     a. Create a Rocket.Chat bot user (or use existing user with personal access token)
     b. Generate personal access token in RC admin
     c. Configure Ralph (env vars vs config file)
   - Configuration reference (all config fields with descriptions)
   - Quick start example
   - Operator filtering explanation
   - Link to full guide

2. **`docs/guide/rocketchat.md`:**
   - Prerequisites (Rocket.Chat server, bot user, personal access token)
   - Step-by-step setup:
     a. Creating a bot user in Rocket.Chat
     b. Generating a personal access token (with screenshots path if applicable)
     c. Running `ralph bot onboard --backend rocketchat`
     d. Manual configuration via `ralph.yml`
   - Configuration reference:
     - `RObot.operator_id` — what it does, when required
     - `RObot.rocketchat.server_url` — server URL
     - `RObot.rocketchat.bot_user_id` — bot's RC user ID (for `X-User-Id` header)
     - `RObot.rocketchat.auth_token` — personal access token
     - `RObot.rocketchat.room_id` — room for communication
   - Environment variables: `RALPH_ROCKETCHAT_AUTH_TOKEN`, `RALPH_ROCKETCHAT_SERVER_URL`
   - Token resolution order: env var > config > OS keychain
   - Operator filtering:
     - Why it's needed (group chats have multiple users)
     - How to find your Rocket.Chat user ID
     - What happens to non-operator messages (silently dropped)
   - Group chat vs DM usage patterns
   - Testing with a local Rocket.Chat instance (Docker Compose example)
   - Troubleshooting section (auth failures, room not found, messages not received)
   - Comparison with Telegram backend (when to use which)

3. **Update `CLAUDE.md`:**
   - Add `ralph-rocketchat` to Architecture table
   - Update RObot section to mention backend selection
   - Add Rocket.Chat config example alongside Telegram example
   - Update Key Files table with `.ralph/rocketchat-state.json`
   - Update Code Locations table with Rocket.Chat paths

4. **Update `crates/ralph-core/data/ralph-tools.md`:**
   - Only if new `ralph tools` subcommands are added (check Tasks 1-5)

5. **Inline doc comments:**
   - All public structs, traits, functions, and methods in `ralph-rocketchat`
   - Follow the documentation style of `ralph-telegram` (concise, with examples where helpful)

## Dependencies
- All previous tasks (1-6) should be complete for accurate documentation

## Implementation Approach
1. Write `crates/ralph-rocketchat/README.md` mirroring the Telegram README structure
2. Write `docs/guide/rocketchat.md` as a comprehensive user guide
3. Update `CLAUDE.md` with Rocket.Chat references
4. Review all public APIs in `ralph-rocketchat` and add doc comments
5. Cross-reference between documents (README links to guide, guide links to README)

## Acceptance Criteria

1. **Crate README exists and is complete**
   - Given a user opens `crates/ralph-rocketchat/README.md`
   - When they follow the setup steps
   - Then they can configure Ralph with a Rocket.Chat backend

2. **User guide covers full setup**
   - Given a user with a Rocket.Chat server
   - When they follow `docs/guide/rocketchat.md`
   - Then they can go from zero to a working RObot integration

3. **Operator filtering is documented**
   - Given a user reading the docs
   - When they look for how to use RObot in a group channel
   - Then they find clear instructions on configuring `operator_id`

4. **CLAUDE.md is updated**
   - Given a developer reading `CLAUDE.md`
   - When they look at the Architecture section
   - Then `ralph-rocketchat` is listed with its purpose

5. **Backend selection is documented**
   - Given a user reading the docs
   - When they want to know how to choose between Telegram and Rocket.Chat
   - Then they find clear guidance on configuring one (and the mutual exclusion rule)

6. **Environment variables are documented**
   - Given a user who prefers env vars over config files
   - When they read the configuration section
   - Then `RALPH_ROCKETCHAT_AUTH_TOKEN` and `RALPH_ROCKETCHAT_SERVER_URL` are documented with resolution order

7. **Doc comments on public API**
   - Given a developer reading the `ralph-rocketchat` source
   - When they look at any public struct, trait, or function
   - Then a doc comment explains its purpose

8. **Troubleshooting section**
   - Given a user encountering common issues (auth failure, room not found)
   - When they check the troubleshooting section
   - Then they find actionable solutions

## Metadata
- **Complexity**: Low
- **Labels**: documentation, rocketchat
- **Required Skills**: Technical writing, Markdown, Rust doc comments
