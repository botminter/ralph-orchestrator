//! Bot setup and management commands.
//!
//! Provides:
//! - `ralph bot onboard` — Interactive wizard for Telegram bot setup
//! - `ralph bot status` — Check current bot configuration status
//! - `ralph bot test` — Send a test message to verify the bot works
//! - `ralph bot token set <token>` — Store/overwrite the bot token

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ralph_core::RalphConfig;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tracing::warn;

use crate::{ConfigSource, HatsSource};

// ─────────────────────────────────────────────────────────────────────────────
// CLI STRUCTS
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub struct BotArgs {
    #[command(subcommand)]
    pub command: BotCommands,
}

#[derive(Subcommand, Debug)]
pub enum BotCommands {
    /// Interactive setup wizard for RObot backend (Telegram, Rocket.Chat, or Matrix)
    Onboard(OnboardArgs),
    /// Check current bot configuration status
    Status,
    /// Send a test message to verify the bot works
    Test(TestArgs),
    /// Manage bot tokens
    Token(TokenArgs),
    /// Run as a persistent daemon, listening on Telegram and starting loops on demand
    Daemon(DaemonArgs),
}

#[derive(Parser, Debug)]
pub struct OnboardArgs {
    /// Backend to onboard: "telegram" (default), "rocketchat", or "matrix"
    #[arg(long, default_value = "telegram")]
    pub backend: String,

    // ── Telegram-specific flags ──────────────────────────────────────────
    /// Skip interactive token prompt, provide token directly (Telegram)
    #[arg(long)]
    pub token: Option<String>,

    /// Skip chat_id detection, provide chat_id directly (Telegram)
    #[arg(long)]
    pub chat_id: Option<i64>,

    /// Timeout in seconds for waiting for a Telegram message (Telegram)
    #[arg(long, default_value = "120")]
    pub timeout: u64,

    // ── Rocket.Chat-specific flags ───────────────────────────────────────
    /// Rocket.Chat server URL (e.g., https://chat.example.com)
    #[arg(long)]
    pub server_url: Option<String>,

    /// Rocket.Chat bot user ID (for X-User-Id header)
    #[arg(long)]
    pub bot_user_id: Option<String>,

    /// Rocket.Chat Personal Access Token
    #[arg(long)]
    pub auth_token: Option<String>,

    /// Rocket.Chat room ID where the bot operates
    #[arg(long)]
    pub room_id: Option<String>,

    /// Operator user ID for filtering messages in group chats
    #[arg(long)]
    pub operator_id: Option<String>,

    // ── Matrix-specific flags ────────────────────────────────────────────
    /// Matrix homeserver URL (e.g., https://matrix.example.com)
    #[arg(long)]
    pub homeserver_url: Option<String>,

    /// Matrix access token (alternative to password login)
    #[arg(long)]
    pub access_token: Option<String>,
}

#[derive(Parser, Debug)]
pub struct TestArgs {
    /// Message to send (default: "Hello from Ralph!")
    #[arg(default_value = "Hello from Ralph!")]
    pub message: String,
}

#[derive(Parser, Debug)]
pub struct TokenArgs {
    #[command(subcommand)]
    pub command: TokenCommands,
}

#[derive(Subcommand, Debug)]
pub enum TokenCommands {
    /// Store or overwrite the bot token
    Set(SetTokenArgs),
}

#[derive(Parser, Debug)]
pub struct SetTokenArgs {
    /// Bot token or access token to store
    #[arg(value_name = "TOKEN")]
    pub token: String,

    /// Optional config file to update with the token
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Backend to store the token for (telegram, matrix, rocketchat).
    /// If omitted, auto-detects from ralph.yml; defaults to telegram.
    #[arg(long)]
    pub backend: Option<String>,
}

#[derive(Parser, Debug)]
pub struct DaemonArgs {}

// ─────────────────────────────────────────────────────────────────────────────
// DISPATCHER
// ─────────────────────────────────────────────────────────────────────────────

pub async fn execute(
    args: BotArgs,
    config_sources: &[ConfigSource],
    hats_source: Option<&HatsSource>,
    use_colors: bool,
) -> Result<()> {
    match args.command {
        BotCommands::Onboard(onboard_args) => match onboard_args.backend.as_str() {
            "telegram" => onboard_telegram(onboard_args, use_colors).await,
            "rocketchat" => onboard_rocketchat(onboard_args, use_colors).await,
            "matrix" => onboard_matrix(onboard_args, use_colors).await,
            other => anyhow::bail!(
                "Unknown backend {:?}. Supported backends: telegram, rocketchat, matrix",
                other
            ),
        },
        BotCommands::Status => bot_status(use_colors).await,
        BotCommands::Test(test_args) => bot_test(test_args, use_colors).await,
        BotCommands::Token(token_args) => bot_token(token_args, use_colors),
        BotCommands::Daemon(daemon_args) => {
            run_daemon(daemon_args, config_sources, hats_source, use_colors).await
        }
    }
}

fn bot_token(args: TokenArgs, use_colors: bool) -> Result<()> {
    match args.command {
        TokenCommands::Set(set_args) => bot_token_set(set_args, use_colors),
    }
}

fn bot_token_set(args: SetTokenArgs, use_colors: bool) -> Result<()> {
    let token = args.token;
    let config_path = args
        .config
        .clone()
        .unwrap_or_else(|| PathBuf::from("ralph.yml"));

    let backend = if let Some(ref b) = args.backend {
        match b.as_str() {
            "telegram" => Backend::Telegram,
            "rocketchat" => Backend::RocketChat,
            "matrix" => Backend::Matrix,
            other => anyhow::bail!(
                "Unknown backend {:?}. Supported: telegram, rocketchat, matrix",
                other
            ),
        }
    } else {
        let detected = detect_configured_backend_from(&config_path);
        if detected == Backend::None {
            Backend::Telegram
        } else {
            detected
        }
    };

    let mut keychain_ok = false;

    let (store_result, keychain_label) = match backend {
        Backend::Telegram => (store_bot_token(&token), "ralph/telegram-bot-token"),
        Backend::Matrix => (
            store_matrix_access_token(&token),
            "ralph/matrix-access-token",
        ),
        Backend::RocketChat => (
            store_rocketchat_auth_token(&token),
            "ralph/rocketchat-auth-token",
        ),
        Backend::None => unreachable!(),
    };

    match store_result {
        Ok(()) => {
            keychain_ok = true;
            print_success(
                use_colors,
                &format!("Token stored in OS keychain ({keychain_label})"),
            );
        }
        Err(e) => {
            print_warning(
                use_colors,
                &format!("Could not store token in keychain: {e}"),
            );
        }
    }

    let has_config = args.config.is_some();

    let should_write_config = has_config || !keychain_ok;
    if should_write_config {
        match backend {
            Backend::Telegram => save_bot_token_config(&config_path, &token)?,
            Backend::Matrix => save_matrix_token_config(&config_path, &token)?,
            Backend::RocketChat => save_rocketchat_token_config(&config_path, &token)?,
            Backend::None => unreachable!(),
        }
        print_success(
            use_colors,
            &format!("Token stored in {}", config_path.display()),
        );
    }

    if !keychain_ok && !has_config {
        print_warning(
            use_colors,
            &format!(
                "Keychain storage failed; token saved to {} instead.",
                config_path.display()
            ),
        );
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// ONBOARD WIZARD
// ─────────────────────────────────────────────────────────────────────────────

async fn onboard_telegram(args: OnboardArgs, use_colors: bool) -> Result<()> {
    println!();
    if use_colors {
        println!("\x1b[1mRalph Telegram Bot Setup\x1b[0m");
        println!("\x1b[1m========================\x1b[0m");
    } else {
        println!("Ralph Telegram Bot Setup");
        println!("========================");
    }
    println!();

    // Step 1: Get token
    let token = if let Some(t) = args.token {
        t
    } else {
        println!("Step 1: Create a Telegram bot");
        println!("  1. Open Telegram and message @BotFather");
        println!("  2. Send /newbot and follow the prompts");
        println!("  3. Copy the bot token");
        println!();
        prompt_token()?
    };

    // Step 2: Validate token
    println!();
    println!("Step 2: Validate token");
    print!("  Checking token with Telegram API...");
    io::stdout().flush()?;

    let bot_info = match telegram_get_me(&token).await {
        Ok(info) => {
            println!();
            print_success(use_colors, &format!("Token valid! Bot: @{}", info.username));
            info
        }
        Err(e) => {
            println!();
            print_error(use_colors, &format!("Token validation failed: {e}"));
            println!();
            println!("  Troubleshooting:");
            println!("    - Check the token was copied correctly from BotFather");
            println!("    - Ensure the token hasn't been revoked");
            println!("    - Check your internet connection");
            anyhow::bail!("Token validation failed");
        }
    };

    // Step 3: Get chat_id
    let chat_id = if let Some(id) = args.chat_id {
        id
    } else {
        println!();
        println!("Step 3: Connect your Telegram account");
        println!(
            "  Send any message to your bot: https://t.me/{}",
            bot_info.username
        );
        print!("  Waiting for message... (timeout: {}s)", args.timeout);
        io::stdout().flush()?;

        match telegram_get_updates(&token, args.timeout).await {
            Ok(update) => {
                println!();
                print_success(
                    use_colors,
                    &format!(
                        "Message received from: {} (chat_id: {})",
                        update.from_name, update.chat_id
                    ),
                );
                update.chat_id
            }
            Err(e) => {
                println!();
                print_error(use_colors, &format!("No message received: {e}"));
                println!();
                println!("  Troubleshooting:");
                println!("    - Make sure you're messaging @{}", bot_info.username);
                println!("    - Try sending /start to the bot");
                println!(
                    "    - You can retry with: ralph bot onboard --token <token> --timeout 300"
                );
                anyhow::bail!("Chat ID detection failed");
            }
        }
    };

    // Step 4: Save configuration
    println!();
    println!("Step 4: Save configuration");

    // Store token in keychain (fallback to config if unavailable)
    let mut config_token: Option<&str> = None;
    match store_bot_token(&token) {
        Ok(()) => {
            print_success(
                use_colors,
                "Token stored in OS keychain (ralph/telegram-bot-token)",
            );
        }
        Err(e) => {
            print_warning(
                use_colors,
                &format!("Could not store token in keychain: {e}"),
            );
            println!("    Set RALPH_TELEGRAM_BOT_TOKEN env var instead.");
            config_token = Some(token.as_str());
        }
    }

    // Update ralph.yml
    match save_robot_config(args.timeout, config_token) {
        Ok(()) => {
            if config_token.is_some() {
                print_warning(
                    use_colors,
                    "Stored bot token in ralph.yml (legacy). Consider using env var or keychain.",
                );
            }
            print_success(use_colors, "Updated ralph.yml (RObot.enabled: true)");
        }
        Err(e) => {
            print_warning(use_colors, &format!("Could not update ralph.yml: {e}"));
            println!("    Add manually:");
            println!("      RObot:");
            println!("        enabled: true");
            println!("        timeout_seconds: {}", args.timeout);
        }
    }

    // Save telegram state
    match save_telegram_state(chat_id) {
        Ok(()) => {
            print_success(
                use_colors,
                &format!("Created .ralph/telegram-state.json (chat_id: {})", chat_id),
            );
        }
        Err(e) => {
            print_warning(use_colors, &format!("Could not save telegram state: {e}"));
        }
    }

    // Step 5: Verify
    println!();
    println!("Step 5: Verify");

    match telegram_send_message(
        &token,
        chat_id,
        "Ralph bot setup complete! I'm ready to assist during orchestration runs.",
    )
    .await
    {
        Ok(_) => {
            print_success(use_colors, "Test message sent to your Telegram!");
        }
        Err(e) => {
            print_warning(use_colors, &format!("Could not send test message: {e}"));
            println!("    Setup saved. Verify later with: ralph bot test");
        }
    }

    println!();
    if use_colors {
        println!(
            "\x1b[32mSetup complete!\x1b[0m Run `ralph run` to start with Telegram integration."
        );
    } else {
        println!("Setup complete! Run `ralph run` to start with Telegram integration.");
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// ONBOARD — ROCKET.CHAT
// ─────────────────────────────────────────────────────────────────────────────

async fn onboard_rocketchat(args: OnboardArgs, use_colors: bool) -> Result<()> {
    use ralph_rocketchat::client::{RocketChatApi, RocketChatClient};

    println!();
    if use_colors {
        println!("\x1b[1mRalph Rocket.Chat Bot Setup\x1b[0m");
        println!("\x1b[1m===========================\x1b[0m");
    } else {
        println!("Ralph Rocket.Chat Bot Setup");
        println!("===========================");
    }
    println!();

    // Step 1: Get server URL
    let server_url = if let Some(url) = args.server_url {
        url
    } else {
        println!("Step 1: Rocket.Chat server URL");
        println!("  e.g., https://chat.example.com");
        println!();
        prompt_input("  Server URL: ")?
    };

    // Step 2: Get bot user ID
    let bot_user_id = if let Some(uid) = args.bot_user_id {
        uid
    } else {
        println!();
        println!("Step 2: Bot user ID");
        println!("  The bot's Rocket.Chat user ID (used for X-User-Id header).");
        println!("  Find it in Administration > Users > your bot user.");
        println!();
        prompt_input("  Bot user ID: ")?
    };

    // Step 3: Get auth token
    let auth_token = if let Some(tok) = args.auth_token {
        tok
    } else {
        println!();
        println!("Step 3: Personal Access Token");
        println!("  Create one in My Account > Personal Access Tokens.");
        println!("  Ensure 'Ignore Two Factor Authentication' is checked.");
        println!();
        prompt_input("  Auth token: ")?
    };

    // Step 4: Validate credentials
    println!();
    println!("Step 4: Validate credentials");
    print!("  Checking credentials with Rocket.Chat API...");
    io::stdout().flush()?;

    let client = RocketChatClient::new(&server_url, &auth_token, &bot_user_id);

    let bot_info = match client.get_me().await {
        Ok(info) => {
            println!();
            let display = info.name.as_deref().unwrap_or(&info.username);
            print_success(
                use_colors,
                &format!("Credentials valid! Bot: {} (@{})", display, info.username),
            );
            info
        }
        Err(e) => {
            println!();
            print_error(use_colors, &format!("Credential validation failed: {e}"));
            println!();
            println!("  Troubleshooting:");
            println!("    - Check that server_url is correct and reachable");
            println!("    - Verify bot_user_id matches the bot's user in Rocket.Chat");
            println!("    - Ensure the Personal Access Token hasn't been revoked");
            println!("    - Check your internet connection");
            anyhow::bail!("Credential validation failed");
        }
    };
    // Confirm the user ID matches what the server reports
    if bot_info.id != bot_user_id {
        print_warning(
            use_colors,
            &format!(
                "Provided bot_user_id ({}) differs from server-reported ID ({}). Using server value.",
                bot_user_id, bot_info.id
            ),
        );
    }

    // Step 5: Get room ID
    let room_id = if let Some(rid) = args.room_id {
        rid
    } else {
        println!();
        println!("Step 5: Room ID");
        println!("  The room where Ralph will send messages.");
        println!("  Find it in the room's kebab menu > Channel Administration > shows in URL.");
        println!();
        prompt_input("  Room ID: ")?
    };

    // Validate room
    print!("  Checking room...");
    io::stdout().flush()?;
    match client.get_room_info(&room_id).await {
        Ok(room) => {
            println!();
            let room_display = room
                .fname
                .as_deref()
                .or(room.name.as_deref())
                .unwrap_or(&room_id);
            let room_type = match room.t.as_str() {
                "c" => "channel",
                "p" => "private group",
                "d" => "direct message",
                other => other,
            };
            print_success(
                use_colors,
                &format!("Room found: {} ({})", room_display, room_type),
            );
        }
        Err(e) => {
            println!();
            print_warning(use_colors, &format!("Could not validate room: {e}"));
            println!("    The bot may not have access. Setup will continue.");
        }
    }

    // Step 6: Get operator ID
    let operator_id = if let Some(oid) = args.operator_id {
        Some(oid)
    } else {
        println!();
        println!("Step 6: Operator ID (optional)");
        println!("  In group chats, only messages from this user are processed.");
        println!("  Leave blank for DMs or if filtering is not needed.");
        println!();
        let input = prompt_input_optional("  Operator user ID (or press Enter to skip): ")?;
        if input.is_empty() { None } else { Some(input) }
    };

    // Step 7: Save configuration
    println!();
    println!("Step 7: Save configuration");

    // Store auth token in keychain
    match store_rocketchat_auth_token(&auth_token) {
        Ok(()) => {
            print_success(
                use_colors,
                "Auth token stored in OS keychain (ralph/rocketchat-auth-token)",
            );
        }
        Err(e) => {
            print_warning(
                use_colors,
                &format!("Could not store token in keychain: {e}"),
            );
            println!("    Set RALPH_ROCKETCHAT_AUTH_TOKEN env var instead.");
        }
    }

    // Update ralph.yml
    match save_rocketchat_config(&server_url, &bot_info.id, &room_id, operator_id.as_deref()) {
        Ok(()) => {
            print_success(
                use_colors,
                "Updated ralph.yml (RObot.rocketchat configured)",
            );
        }
        Err(e) => {
            print_warning(use_colors, &format!("Could not update ralph.yml: {e}"));
            println!("    Add manually:");
            println!("      RObot:");
            println!("        enabled: true");
            println!("        rocketchat:");
            println!("          server_url: {}", server_url);
            println!("          bot_user_id: {}", bot_info.id);
            println!("          room_id: {}", room_id);
            if let Some(ref oid) = operator_id {
                println!("        operator_id: {}", oid);
            }
        }
    }

    // Step 8: Verify
    println!();
    println!("Step 8: Verify");

    match client
        .send_message(
            &room_id,
            "Ralph bot setup complete! I'm ready to assist during orchestration runs.",
            None,
        )
        .await
    {
        Ok(_) => {
            print_success(use_colors, "Test message sent to your Rocket.Chat room!");
        }
        Err(e) => {
            print_warning(use_colors, &format!("Could not send test message: {e}"));
            println!("    Setup saved. Verify later with: ralph bot test");
        }
    }

    println!();
    if use_colors {
        println!(
            "\x1b[32mSetup complete!\x1b[0m Run `ralph run` to start with Rocket.Chat integration."
        );
    } else {
        println!("Setup complete! Run `ralph run` to start with Rocket.Chat integration.");
    }

    Ok(())
}

async fn onboard_matrix(args: OnboardArgs, use_colors: bool) -> Result<()> {
    use ralph_matrix::{MatrixApi, MatrixClient};

    println!();
    if use_colors {
        println!("\x1b[1mRalph Matrix Bot Setup\x1b[0m");
        println!("\x1b[1m======================\x1b[0m");
    } else {
        println!("Ralph Matrix Bot Setup");
        println!("======================");
    }
    println!();

    // Step 1: Get homeserver URL
    let homeserver_url = if let Some(url) = args.homeserver_url {
        url
    } else {
        println!("Step 1: Matrix homeserver URL");
        println!("  e.g., https://matrix.example.com");
        println!();
        prompt_input("  Homeserver URL: ")?
    };

    // Step 2: Get access token (or login with password)
    let access_token = if let Some(tok) = args.access_token {
        tok
    } else {
        println!();
        println!("Step 2: Authentication");
        println!("  You can provide an access token directly, or log in with username/password.");
        println!(
            "  Access tokens can be found in Element: Settings > Help & About > Access Token."
        );
        println!();
        let token_input =
            prompt_input_optional("  Access token (or press Enter to use password login): ")?;

        if token_input.is_empty() {
            // Password login flow
            println!();
            let username = prompt_input("  Username (e.g., @bot:example.com or just bot): ")?;
            let password = prompt_input("  Password: ")?;

            println!();
            print!("  Logging in with password...");
            io::stdout().flush()?;

            let client = MatrixClient::new();
            match client.login(&homeserver_url, &username, &password).await {
                Ok(()) => {
                    println!();
                    print_success(use_colors, "Password login successful!");
                    println!("    Note: An access token was generated for this session.");
                    println!("    You should create a dedicated access token for production use.");
                }
                Err(e) => {
                    println!();
                    print_error(use_colors, &format!("Password login failed: {e}"));
                    println!();
                    println!("  Troubleshooting:");
                    println!("    - Check that homeserver URL is correct and reachable");
                    println!("    - Verify username and password are correct");
                    println!("    - Check your internet connection");
                    anyhow::bail!("Password login failed");
                }
            }

            // Password login doesn't give us a portable token to store.
            // Prompt for an access token instead.
            println!();
            println!(
                "  Password login verified credentials, but an access token is needed for storage."
            );
            println!(
                "  Please provide an access token (Element: Settings > Help & About > Access Token)."
            );
            println!();
            prompt_input("  Access token: ")?
        } else {
            token_input
        }
    };

    // Step 3: Validate credentials
    println!();
    println!("Step 3: Validate credentials");
    print!("  Checking credentials with Matrix homeserver...");
    io::stdout().flush()?;

    let client = MatrixClient::new();
    match client
        .login_with_token(&homeserver_url, &access_token)
        .await
    {
        Ok(()) => {}
        Err(e) => {
            println!();
            print_error(use_colors, &format!("Credential validation failed: {e}"));
            println!();
            println!("  Troubleshooting:");
            println!("    - Check that homeserver URL is correct and reachable");
            println!("    - Verify the access token hasn't been revoked");
            println!("    - Check your internet connection");
            anyhow::bail!("Credential validation failed");
        }
    }

    match client.get_display_name().await {
        Ok(display_name) => {
            println!();
            print_success(
                use_colors,
                &format!("Credentials valid! Bot: {}", display_name),
            );
        }
        Err(e) => {
            println!();
            print_warning(
                use_colors,
                &format!("Logged in but could not fetch display name: {e}"),
            );
            println!("    Setup will continue.");
        }
    }

    // Step 4: Get room ID
    let room_id = if let Some(rid) = args.room_id {
        rid
    } else {
        println!();
        println!("Step 4: Room ID");
        println!("  The room where Ralph will send messages.");
        println!("  Find it in Element: Room Settings > Advanced > Internal room ID.");
        println!("  Format: !abc123:example.com");
        println!();
        prompt_input("  Room ID: ")?
    };

    // Sync + join so the SDK can see the room, then validate
    let _ = client.ensure_room(&room_id).await;
    print!("  Checking room...");
    io::stdout().flush()?;
    match client.get_room_info(&room_id).await {
        Ok(room) => {
            println!();
            let room_display = room.name.as_deref().unwrap_or(&room_id);
            let members = room.member_count;
            print_success(
                use_colors,
                &format!("Room found: {} ({} members)", room_display, members),
            );
        }
        Err(e) => {
            println!();
            print_warning(use_colors, &format!("Could not validate room: {e}"));
            println!("    The bot may not have joined. Setup will continue.");
        }
    }

    // Step 5: Get operator ID
    let operator_id = if let Some(oid) = args.operator_id {
        Some(oid)
    } else {
        println!();
        println!("Step 5: Operator ID (optional)");
        println!("  In group rooms, only messages from this user are processed.");
        println!("  Format: @username:example.com");
        println!("  Leave blank for DMs or if filtering is not needed.");
        println!();
        let input = prompt_input_optional("  Operator user ID (or press Enter to skip): ")?;
        if input.is_empty() { None } else { Some(input) }
    };

    // Step 6: Save configuration
    println!();
    println!("Step 6: Save configuration");

    // Store access token in keychain
    match store_matrix_access_token(&access_token) {
        Ok(()) => {
            print_success(
                use_colors,
                "Access token stored in OS keychain (ralph/matrix-access-token)",
            );
        }
        Err(e) => {
            print_warning(
                use_colors,
                &format!("Could not store token in keychain: {e}"),
            );
            println!("    Set RALPH_MATRIX_ACCESS_TOKEN env var instead.");
        }
    }

    // Update ralph.yml
    match save_matrix_config(&homeserver_url, &room_id, operator_id.as_deref()) {
        Ok(()) => {
            print_success(use_colors, "Updated ralph.yml (RObot.matrix configured)");
        }
        Err(e) => {
            print_warning(use_colors, &format!("Could not update ralph.yml: {e}"));
            println!("    Add manually:");
            println!("      RObot:");
            println!("        enabled: true");
            println!("        matrix:");
            println!("          homeserver_url: {}", homeserver_url);
            println!("          room_id: {}", room_id);
            if let Some(ref oid) = operator_id {
                println!("        operator_id: {}", oid);
            }
        }
    }

    // Step 7: Verify
    println!();
    println!("Step 7: Verify");

    match client
        .send_message(
            &room_id,
            "Ralph bot setup complete! I'm ready to assist during orchestration runs.",
            None,
        )
        .await
    {
        Ok(_) => {
            print_success(use_colors, "Test message sent to your Matrix room!");
        }
        Err(e) => {
            print_warning(use_colors, &format!("Could not send test message: {e}"));
            println!("    Setup saved. Verify later with: ralph bot test");
        }
    }

    println!();
    if use_colors {
        println!(
            "\x1b[32mSetup complete!\x1b[0m Run `ralph run` to start with Matrix integration."
        );
    } else {
        println!("Setup complete! Run `ralph run` to start with Matrix integration.");
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// STATUS COMMAND
// ─────────────────────────────────────────────────────────────────────────────

async fn bot_status(use_colors: bool) -> Result<()> {
    println!();
    if use_colors {
        println!("\x1b[1mRalph Bot Status\x1b[0m");
        println!("\x1b[1m================\x1b[0m");
    } else {
        println!("Ralph Bot Status");
        println!("================");
    }
    println!();

    let backend = detect_configured_backend();

    match backend {
        Backend::RocketChat => bot_status_rocketchat(use_colors).await,
        Backend::Matrix => bot_status_matrix(use_colors).await,
        Backend::Telegram => bot_status_telegram(use_colors).await,
        Backend::None => {
            print_error(use_colors, "No RObot backend configured");
            println!();
            println!("  Set up a backend with:");
            println!("    ralph bot onboard --backend telegram");
            println!("    ralph bot onboard --backend rocketchat");
            println!("    ralph bot onboard --backend matrix");
            Ok(())
        }
    }
}

async fn bot_status_telegram(use_colors: bool) -> Result<()> {
    print_success(use_colors, "Backend: Telegram");
    println!();

    // Check keychain
    let keychain_token = load_bot_token();
    let has_keychain = keychain_token.is_some();
    if has_keychain {
        print_success(use_colors, "Keychain: token stored");
    } else {
        print_status(use_colors, "Keychain: no token found");
    }

    // Check env var
    let has_env = std::env::var("RALPH_TELEGRAM_BOT_TOKEN").is_ok();
    if has_env {
        print_success(use_colors, "Env var: RALPH_TELEGRAM_BOT_TOKEN set");
    } else {
        print_status(use_colors, "Env var: RALPH_TELEGRAM_BOT_TOKEN not set");
    }

    // Check config
    let config_token = load_config_bot_token();
    if config_token.is_some() {
        print_warning(
            use_colors,
            "Config: bot_token in ralph.yml (consider migrating to keychain)",
        );
    } else {
        print_status(use_colors, "Config: no token in ralph.yml");
    }

    // Check RObot enabled
    let robot_enabled = is_robot_enabled();
    if robot_enabled {
        print_success(use_colors, "RObot: enabled in ralph.yml");
    } else {
        print_status(use_colors, "RObot: not enabled in ralph.yml");
    }

    // Check telegram state
    let state_path = Path::new(".ralph/telegram-state.json");
    if state_path.exists() {
        if let Ok(content) = std::fs::read_to_string(state_path) {
            if let Ok(state) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(chat_id) = state.get("chat_id").and_then(|v| v.as_i64()) {
                    print_success(
                        use_colors,
                        &format!("Telegram state: chat_id = {}", chat_id),
                    );
                } else {
                    print_warning(use_colors, "Telegram state: file exists but no chat_id");
                }
            } else {
                print_warning(use_colors, "Telegram state: file exists but invalid JSON");
            }
        }
    } else {
        print_status(use_colors, "Telegram state: not found");
    }

    // Validate token if available
    let effective_token = std::env::var("RALPH_TELEGRAM_BOT_TOKEN")
        .ok()
        .or(keychain_token)
        .or(config_token);

    println!();
    if let Some(token) = effective_token {
        print!("  Validating token with Telegram API...");
        io::stdout().flush()?;
        match telegram_get_me(&token).await {
            Ok(info) => {
                println!();
                print_success(
                    use_colors,
                    &format!("Bot: @{} ({})", info.username, info.first_name),
                );
            }
            Err(e) => {
                println!();
                print_error(use_colors, &format!("Token validation failed: {e}"));
            }
        }
    } else {
        print_error(
            use_colors,
            "No token available. Run `ralph bot onboard` to set up.",
        );
    }

    Ok(())
}

async fn bot_status_rocketchat(use_colors: bool) -> Result<()> {
    use ralph_rocketchat::client::{RocketChatApi, RocketChatClient};

    print_success(use_colors, "Backend: Rocket.Chat");
    println!();

    // Load config
    let config_path = Path::new("ralph.yml");
    let config = RalphConfig::from_file(config_path)
        .with_context(|| format!("Failed to load config from {}", config_path.display()))?;

    let rc = config
        .robot
        .rocketchat
        .as_ref()
        .context("RObot.rocketchat section missing from ralph.yml")?;

    // Server URL
    let server_url = config.robot.resolve_rocketchat_server_url();
    if let Some(ref url) = server_url {
        print_success(use_colors, &format!("Server URL: {}", url));
    } else {
        print_error(use_colors, "Server URL: not configured");
    }

    // Auth token
    let auth_token = config.robot.resolve_rocketchat_auth_token();
    if auth_token.is_some() {
        print_success(use_colors, "Auth token: configured");
    } else {
        print_error(
            use_colors,
            "Auth token: not found (set RALPH_ROCKETCHAT_AUTH_TOKEN or config)",
        );
    }

    // Bot user ID
    if let Some(ref bot_user_id) = rc.bot_user_id {
        print_success(use_colors, &format!("Bot user ID: {}", bot_user_id));
    } else {
        print_error(use_colors, "Bot user ID: not configured");
    }

    // Room ID
    if let Some(ref room_id) = rc.room_id {
        print_success(use_colors, &format!("Room ID: {}", room_id));
    } else {
        print_error(use_colors, "Room ID: not configured");
    }

    // Operator ID
    if let Some(ref operator_id) = config.robot.operator_id {
        print_success(use_colors, &format!("Operator ID: {}", operator_id));
    } else {
        print_status(
            use_colors,
            "Operator ID: not configured (all users can interact)",
        );
    }

    // RObot enabled
    if config.robot.enabled {
        print_success(use_colors, "RObot: enabled");
    } else {
        print_status(use_colors, "RObot: not enabled");
    }

    // Validate credentials if we have enough info
    println!();
    if let (Some(url), Some(token), Some(bot_user_id)) =
        (server_url, auth_token, rc.bot_user_id.clone())
    {
        print!("  Validating credentials with Rocket.Chat API...");
        io::stdout().flush()?;
        let client = RocketChatClient::new(url, token, &bot_user_id);
        match client.get_me().await {
            Ok(user) => {
                println!();
                let display_name = user.name.as_deref().unwrap_or(&user.username);
                print_success(
                    use_colors,
                    &format!("Bot: {} (@{})", display_name, user.username),
                );
            }
            Err(e) => {
                println!();
                print_error(use_colors, &format!("Credential validation failed: {e}"));
            }
        }

        // Validate room if configured
        if let Some(ref room_id) = rc.room_id {
            match client.get_room_info(room_id).await {
                Ok(room) => {
                    let room_name = room.name.as_deref().unwrap_or("(DM)");
                    print_success(
                        use_colors,
                        &format!("Room: {} (type: {})", room_name, room.t),
                    );
                }
                Err(e) => {
                    print_error(use_colors, &format!("Room validation failed: {e}"));
                }
            }
        }
    } else {
        print_error(
            use_colors,
            "Cannot validate: missing server_url, auth_token, or bot_user_id",
        );
    }

    Ok(())
}

async fn bot_status_matrix(use_colors: bool) -> Result<()> {
    use ralph_matrix::{MatrixApi, MatrixClient};

    print_success(use_colors, "Backend: Matrix");
    println!();

    // Load config
    let config_path = Path::new("ralph.yml");
    let config = RalphConfig::from_file(config_path)
        .with_context(|| format!("Failed to load config from {}", config_path.display()))?;

    let mx = config
        .robot
        .matrix
        .as_ref()
        .context("RObot.matrix section missing from ralph.yml")?;

    // Homeserver URL
    let homeserver_url = config.robot.resolve_matrix_homeserver_url();
    if let Some(ref url) = homeserver_url {
        print_success(use_colors, &format!("Homeserver URL: {}", url));
    } else {
        print_error(use_colors, "Homeserver URL: not configured");
    }

    // Access token — keychain
    let keychain_token = load_matrix_access_token();
    if keychain_token.is_some() {
        print_success(use_colors, "Keychain: access token stored");
    } else {
        print_status(use_colors, "Keychain: no access token found");
    }

    // Access token — env var
    let has_env = std::env::var("RALPH_MATRIX_ACCESS_TOKEN").is_ok();
    if has_env {
        print_success(use_colors, "Env var: RALPH_MATRIX_ACCESS_TOKEN set");
    } else {
        print_status(use_colors, "Env var: RALPH_MATRIX_ACCESS_TOKEN not set");
    }

    // Access token — config file
    let config_token = mx.access_token.clone();
    if config_token.is_some() {
        print_warning(
            use_colors,
            "Config: access_token in ralph.yml (consider migrating to keychain)",
        );
    } else {
        print_status(use_colors, "Config: no access_token in ralph.yml");
    }

    // Bot user ID
    if let Some(ref bot_user_id) = mx.bot_user_id {
        print_success(use_colors, &format!("Bot user ID: {}", bot_user_id));
    } else {
        print_status(use_colors, "Bot user ID: not configured");
    }

    // Room ID
    if let Some(ref room_id) = mx.room_id {
        print_success(use_colors, &format!("Room ID: {}", room_id));
    } else {
        print_error(use_colors, "Room ID: not configured");
    }

    // Operator ID
    if let Some(ref operator_id) = config.robot.operator_id {
        print_success(use_colors, &format!("Operator ID: {}", operator_id));
    } else {
        print_status(
            use_colors,
            "Operator ID: not configured (all users can interact)",
        );
    }

    // RObot enabled
    if config.robot.enabled {
        print_success(use_colors, "RObot: enabled");
    } else {
        print_status(use_colors, "RObot: not enabled");
    }

    // Live validation if we have enough info
    println!();
    let effective_token = config.robot.resolve_matrix_access_token();
    if let (Some(url), Some(token)) = (homeserver_url, effective_token) {
        print!("  Validating credentials with Matrix homeserver...");
        io::stdout().flush()?;
        let client = MatrixClient::new();
        match client.login_with_token(&url, &token).await {
            Ok(()) => match client.get_display_name().await {
                Ok(display_name) => {
                    println!();
                    print_success(use_colors, &format!("Bot: {}", display_name));
                }
                Err(e) => {
                    println!();
                    print_warning(
                        use_colors,
                        &format!("Logged in but could not get display name: {e}"),
                    );
                }
            },
            Err(e) => {
                println!();
                print_error(use_colors, &format!("Credential validation failed: {e}"));
            }
        }

        // Validate room if configured — sync + join first so the SDK
        // knows about the room (its local cache is empty after login).
        if let Some(ref room_id) = mx.room_id {
            if let Err(e) = client.ensure_room(room_id).await {
                print_error(use_colors, &format!("Room join/sync failed: {e}"));
            } else {
                match client.get_room_info(room_id).await {
                    Ok(room) => {
                        let room_name = room.name.as_deref().unwrap_or("(unnamed)");
                        print_success(use_colors, &format!("Room: {}", room_name));
                    }
                    Err(e) => {
                        print_error(use_colors, &format!("Room validation failed: {e}"));
                    }
                }
            }
        }
    } else {
        print_error(
            use_colors,
            "Cannot validate: missing homeserver_url or access_token",
        );
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// TEST COMMAND
// ─────────────────────────────────────────────────────────────────────────────

async fn bot_test(args: TestArgs, use_colors: bool) -> Result<()> {
    let backend = detect_configured_backend();

    match backend {
        Backend::RocketChat => bot_test_rocketchat(args, use_colors).await,
        Backend::Matrix => bot_test_matrix(args, use_colors).await,
        Backend::Telegram => bot_test_telegram(args, use_colors).await,
        Backend::None => {
            print_error(use_colors, "No RObot backend configured");
            println!();
            println!("  Set up a backend with:");
            println!("    ralph bot onboard --backend telegram");
            println!("    ralph bot onboard --backend rocketchat");
            println!("    ralph bot onboard --backend matrix");
            Ok(())
        }
    }
}

async fn bot_test_telegram(args: TestArgs, use_colors: bool) -> Result<()> {
    // Resolve token
    let token = resolve_token().context(
        "No bot token available. Run `ralph bot onboard` or set RALPH_TELEGRAM_BOT_TOKEN",
    )?;

    // Resolve chat_id
    let chat_id =
        resolve_chat_id().context("No chat_id found. Run `ralph bot onboard` to detect it")?;

    print!("  Sending message to chat {}...", chat_id);
    io::stdout().flush()?;

    match telegram_send_message(&token, chat_id, &args.message).await {
        Ok(_) => {
            println!();
            print_success(use_colors, "Message sent!");
        }
        Err(e) => {
            println!();
            print_error(use_colors, &format!("Failed to send message: {e}"));
            anyhow::bail!("Send failed");
        }
    }

    Ok(())
}

async fn bot_test_rocketchat(args: TestArgs, use_colors: bool) -> Result<()> {
    use ralph_rocketchat::client::{RocketChatApi, RocketChatClient};

    // Load config
    let config_path = Path::new("ralph.yml");
    let config = RalphConfig::from_file(config_path)
        .with_context(|| format!("Failed to load config from {}", config_path.display()))?;

    let rc = config
        .robot
        .rocketchat
        .as_ref()
        .context("RObot.rocketchat section missing from ralph.yml")?;

    // Resolve credentials
    let server_url = config
        .robot
        .resolve_rocketchat_server_url()
        .context("No server_url configured. Run `ralph bot onboard --backend rocketchat`")?;

    let auth_token = config
        .robot
        .resolve_rocketchat_auth_token()
        .context("No auth_token configured. Set RALPH_ROCKETCHAT_AUTH_TOKEN or run `ralph bot onboard --backend rocketchat`")?;

    let bot_user_id = rc
        .bot_user_id
        .as_ref()
        .context("No bot_user_id configured. Run `ralph bot onboard --backend rocketchat`")?;

    let room_id = rc
        .room_id
        .as_ref()
        .context("No room_id configured. Run `ralph bot onboard --backend rocketchat`")?;

    print!("  Sending message to room {}...", room_id);
    io::stdout().flush()?;

    let client = RocketChatClient::new(server_url, auth_token, bot_user_id);
    match client.send_message(room_id, &args.message, None).await {
        Ok(_) => {
            println!();
            print_success(use_colors, "Message sent!");
        }
        Err(e) => {
            println!();
            print_error(use_colors, &format!("Failed to send message: {e}"));
            anyhow::bail!("Send failed");
        }
    }

    Ok(())
}

async fn bot_test_matrix(args: TestArgs, use_colors: bool) -> Result<()> {
    use ralph_matrix::{MatrixApi, MatrixClient};

    // Load config
    let config_path = Path::new("ralph.yml");
    let config = RalphConfig::from_file(config_path)
        .with_context(|| format!("Failed to load config from {}", config_path.display()))?;

    let mx = config
        .robot
        .matrix
        .as_ref()
        .context("RObot.matrix section missing from ralph.yml")?;

    // Resolve credentials
    let homeserver_url = config
        .robot
        .resolve_matrix_homeserver_url()
        .context("No homeserver_url configured. Run `ralph bot onboard --backend matrix`")?;

    let access_token = config
        .robot
        .resolve_matrix_access_token()
        .context("No access_token configured. Set RALPH_MATRIX_ACCESS_TOKEN or run `ralph bot onboard --backend matrix`")?;

    let room_id = mx
        .room_id
        .as_ref()
        .context("No room_id configured. Run `ralph bot onboard --backend matrix`")?;

    // Connect to homeserver and ensure room is joined
    let client = MatrixClient::new();
    client
        .login_with_token(&homeserver_url, &access_token)
        .await
        .context("Failed to authenticate with Matrix homeserver")?;

    client
        .ensure_room(room_id)
        .await
        .context("Failed to sync/join Matrix room")?;

    print!("  Sending message to room {}...", room_id);
    io::stdout().flush()?;

    match client.send_message(room_id, &args.message, None).await {
        Ok(_) => {
            println!();
            print_success(use_colors, "Message sent!");
        }
        Err(e) => {
            println!();
            print_error(use_colors, &format!("Failed to send message: {e}"));
            anyhow::bail!("Send failed");
        }
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// DAEMON COMMAND
// ─────────────────────────────────────────────────────────────────────────────

/// Run the bot daemon — delegates to the configured communication adapter.
///
/// Supports Telegram, Rocket.Chat, and Matrix backends. The adapter implements
/// [`DaemonAdapter`] and handles all platform-specific concerns.
async fn run_daemon(
    _args: DaemonArgs,
    config_sources: &[ConfigSource],
    hats_source: Option<&HatsSource>,
    use_colors: bool,
) -> Result<()> {
    use ralph_proto::DaemonAdapter;

    let workspace_root = std::env::current_dir().context("Failed to get current directory")?;
    let primary_sources: Vec<_> = config_sources
        .iter()
        .filter(|s| !matches!(s, ConfigSource::Override { .. }))
        .collect();

    if primary_sources.len() > 1 {
        warn!("Multiple config sources specified, using first one. Others ignored.");
    }

    let has_overrides = config_sources
        .iter()
        .any(|s| matches!(s, ConfigSource::Override { .. }));
    if has_overrides || hats_source.is_some() {
        warn!("Config overrides/hats will be resolved into a temporary runtime config.");
    }

    let direct_file = if let Some(ConfigSource::File(path)) = primary_sources.first() {
        let path = if path.is_absolute() {
            path.clone()
        } else {
            workspace_root.join(path)
        };

        if !path.exists() {
            anyhow::bail!("Config file not found: {}", path.display());
        }

        if has_overrides || hats_source.is_some() {
            None
        } else {
            let config = RalphConfig::from_file(&path)
                .with_context(|| format!("Failed to load config from {}", path.display()))?;

            Some((config, path))
        }
    } else {
        None
    };

    let used_direct_file = direct_file.is_some();

    let (config, config_path) = if let Some((config, path)) = direct_file {
        (config, path)
    } else {
        let config = crate::preflight::load_config_for_preflight(config_sources, hats_source)
            .await
            .context("Failed to load config for bot daemon")?;
        let path = write_temp_config_for_daemon(&workspace_root, &config)
            .context("Failed to write temporary runtime config")?;
        (config, path)
    };

    // Preserve previous behavior for plain default run.
    let default_path = workspace_root.join("ralph.yml");
    if primary_sources.is_empty() && !has_overrides && !default_path.exists() {
        anyhow::bail!("Config file not found: {}", default_path.display());
    }

    if !primary_sources.is_empty() && !used_direct_file {
        warn!("Using resolved runtime config: {}", config_path.display());
    }

    // Detect which backend is configured and build the appropriate adapter
    let adapter: Box<dyn DaemonAdapter> = if let Some(rc) = &config.robot.rocketchat {
        let auth_token = config
            .robot
            .resolve_rocketchat_auth_token()
            .context("No Rocket.Chat auth token available. Set RALPH_ROCKETCHAT_AUTH_TOKEN env var or set RObot.rocketchat.auth_token in config")?;
        let server_url = config
            .robot
            .resolve_rocketchat_server_url()
            .context("No Rocket.Chat server URL available. Set RALPH_ROCKETCHAT_SERVER_URL env var or set RObot.rocketchat.server_url in config")?;
        let bot_user_id = rc
            .bot_user_id
            .clone()
            .context("RObot.rocketchat.bot_user_id is required")?;
        let room_id = rc
            .room_id
            .clone()
            .context("RObot.rocketchat.room_id is required")?;
        let operator_id = config.robot.operator_id.clone();

        if use_colors {
            println!("\x1b[1mRalph Daemon\x1b[0m (Rocket.Chat)");
        } else {
            println!("Ralph Daemon (Rocket.Chat)");
        }

        Box::new(ralph_rocketchat::daemon::RocketChatDaemon::new(
            server_url,
            auth_token,
            bot_user_id,
            room_id,
            operator_id,
        ))
    } else if let Some(mx) = &config.robot.matrix {
        let access_token = config
            .robot
            .resolve_matrix_access_token()
            .context("No Matrix access token available. Set RALPH_MATRIX_ACCESS_TOKEN env var or set RObot.matrix.access_token in config")?;
        let homeserver_url = config
            .robot
            .resolve_matrix_homeserver_url()
            .context("No Matrix homeserver URL available. Set RALPH_MATRIX_HOMESERVER_URL env var or set RObot.matrix.homeserver_url in config")?;
        let room_id = mx
            .room_id
            .clone()
            .context("RObot.matrix.room_id is required")?;
        let operator_id = config.robot.operator_id.clone();

        if use_colors {
            println!("\x1b[1mRalph Daemon\x1b[0m (Matrix)");
        } else {
            println!("Ralph Daemon (Matrix)");
        }

        Box::new(ralph_matrix::daemon::MatrixDaemon::new(
            homeserver_url,
            access_token,
            room_id,
            operator_id,
        ))
    } else {
        // Telegram backend (default)
        let token = config.robot.resolve_bot_token().context(
            "No bot token available. Run `ralph bot onboard` or set RALPH_TELEGRAM_BOT_TOKEN",
        )?;
        let chat_id =
            resolve_chat_id().context("No chat_id found. Run `ralph bot onboard` to detect it")?;

        if use_colors {
            println!("\x1b[1mRalph Daemon\x1b[0m (Telegram)");
        } else {
            println!("Ralph Daemon (Telegram)");
        }

        // Resolve custom API URL (env var > config file)
        let api_url = std::env::var("RALPH_TELEGRAM_API_URL")
            .ok()
            .or_else(|| load_config_api_url_from(&config_path));

        Box::new(ralph_telegram::TelegramDaemon::new(token, api_url, chat_id))
    };

    // Build the start_loop callback — wraps our CLI loop runner
    let start_loop: ralph_proto::StartLoopFn = Box::new(move |prompt: String| {
        let config_path = Some(config_path.clone());
        Box::pin(async move {
            let ws = std::env::current_dir()?;
            let reason = crate::loop_runner::start_loop(prompt, ws, config_path).await?;
            Ok(format!("{:?}", reason))
        })
    });

    adapter.run_daemon(workspace_root, start_loop).await?;

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// TELEGRAM API HELPERS (raw reqwest, no teloxide)
// ─────────────────────────────────────────────────────────────────────────────

/// Bot info returned by getMe.
struct BotInfo {
    first_name: String,
    username: String,
}

/// Update info from getUpdates.
struct UpdateInfo {
    chat_id: i64,
    from_name: String,
}

/// Validate a bot token via the Telegram getMe API.
async fn telegram_get_me(token: &str) -> Result<BotInfo> {
    let url = format!("https://api.telegram.org/bot{}/getMe", token);
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .context("Network error calling Telegram API")?;

    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .context("Failed to parse Telegram API response")?;

    if !status.is_success() || body.get("ok") != Some(&serde_json::Value::Bool(true)) {
        let description = body
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown error");
        anyhow::bail!("Telegram API error: {}", description);
    }

    let result = body
        .get("result")
        .context("Missing 'result' in Telegram response")?;
    let first_name = result
        .get("first_name")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();
    let username = result
        .get("username")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown_bot")
        .to_string();

    Ok(BotInfo {
        first_name,
        username,
    })
}

/// Long-poll for the first message sent to the bot.
async fn telegram_get_updates(token: &str, timeout_secs: u64) -> Result<UpdateInfo> {
    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    // Telegram long polling uses a max of 50 seconds per request
    let poll_timeout = std::cmp::min(timeout_secs, 30);
    let mut offset: Option<i64> = None;

    while std::time::Instant::now() < deadline {
        let remaining = deadline.duration_since(std::time::Instant::now()).as_secs();
        if remaining == 0 {
            break;
        }
        let this_timeout = std::cmp::min(poll_timeout, remaining);

        let mut url = format!(
            "https://api.telegram.org/bot{}/getUpdates?timeout={}",
            token, this_timeout
        );
        if let Some(off) = offset {
            url.push_str(&format!("&offset={}", off));
        }

        let resp = client
            .get(&url)
            .timeout(std::time::Duration::from_secs(this_timeout + 10))
            .send()
            .await
            .context("Network error calling Telegram API")?;

        let body: serde_json::Value = resp
            .json()
            .await
            .context("Failed to parse Telegram API response")?;

        if let Some(results) = body.get("result").and_then(|v| v.as_array()) {
            for update in results {
                // Track offset for next poll
                if let Some(update_id) = update.get("update_id").and_then(|v| v.as_i64()) {
                    offset = Some(update_id + 1);
                }

                // Extract message
                if let Some(message) = update.get("message") {
                    let chat_id = message
                        .get("chat")
                        .and_then(|c| c.get("id"))
                        .and_then(|v| v.as_i64());

                    let from_name = message
                        .get("from")
                        .and_then(|f| {
                            let first = f.get("first_name").and_then(|v| v.as_str());
                            let last = f.get("last_name").and_then(|v| v.as_str());
                            match (first, last) {
                                (Some(f), Some(l)) => Some(format!("{} {}", f, l)),
                                (Some(f), None) => Some(f.to_string()),
                                _ => None,
                            }
                        })
                        .unwrap_or_else(|| "Unknown".to_string());

                    if let Some(chat_id) = chat_id {
                        return Ok(UpdateInfo { chat_id, from_name });
                    }
                }
            }
        }
    }

    anyhow::bail!("Timed out waiting for a message ({}s)", timeout_secs)
}

/// Send a message to a Telegram chat.
pub(crate) async fn telegram_send_message(token: &str, chat_id: i64, text: &str) -> Result<()> {
    let url = format!("https://api.telegram.org/bot{}/sendMessage", token);
    let client = reqwest::Client::new();

    let payload = serde_json::json!({
        "chat_id": chat_id,
        "text": text,
    });

    let resp = client
        .post(&url)
        .json(&payload)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .context("Network error calling Telegram API")?;

    let body: serde_json::Value = resp
        .json()
        .await
        .context("Failed to parse Telegram API response")?;

    if body.get("ok") != Some(&serde_json::Value::Bool(true)) {
        let description = body
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown error");
        anyhow::bail!("Telegram sendMessage failed: {}", description);
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// KEYCHAIN HELPERS
// ─────────────────────────────────────────────────────────────────────────────

/// Store bot token in OS keychain.
fn store_bot_token(token: &str) -> Result<()> {
    let entry = keyring::Entry::new("ralph", "telegram-bot-token")
        .context("Failed to create keychain entry")?;
    if let Err(err) = entry.set_password(token) {
        // Some keychains refuse overwrites; try delete + set as a fallback.
        if entry.delete_credential().is_ok() {
            entry
                .set_password(token)
                .context("Failed to store token in keychain after deleting existing entry")?;
        } else {
            return Err(anyhow::anyhow!(
                "Failed to store token in keychain: {}",
                err
            ));
        }
    }
    Ok(())
}

/// Load bot token from OS keychain.
fn load_bot_token() -> Option<String> {
    keyring::Entry::new("ralph", "telegram-bot-token")
        .ok()
        .and_then(|e| e.get_password().ok())
}

/// Store Rocket.Chat auth token in OS keychain.
fn store_rocketchat_auth_token(token: &str) -> Result<()> {
    let entry = keyring::Entry::new("ralph", "rocketchat-auth-token")
        .context("Failed to create keychain entry")?;
    if let Err(err) = entry.set_password(token) {
        if entry.delete_credential().is_ok() {
            entry
                .set_password(token)
                .context("Failed to store token in keychain after deleting existing entry")?;
        } else {
            return Err(anyhow::anyhow!(
                "Failed to store token in keychain: {}",
                err
            ));
        }
    }
    Ok(())
}

/// Store Matrix access token in OS keychain.
fn store_matrix_access_token(token: &str) -> Result<()> {
    let entry = keyring::Entry::new("ralph", "matrix-access-token")
        .context("Failed to create keychain entry")?;
    if let Err(err) = entry.set_password(token) {
        if entry.delete_credential().is_ok() {
            entry
                .set_password(token)
                .context("Failed to store token in keychain after deleting existing entry")?;
        } else {
            return Err(anyhow::anyhow!(
                "Failed to store token in keychain: {}",
                err
            ));
        }
    }
    Ok(())
}

/// Load Matrix access token from OS keychain.
fn load_matrix_access_token() -> Option<String> {
    keyring::Entry::new("ralph", "matrix-access-token")
        .ok()
        .and_then(|e| e.get_password().ok())
}

// ─────────────────────────────────────────────────────────────────────────────
// CONFIG HELPERS
// ─────────────────────────────────────────────────────────────────────────────

/// Save RObot config to ralph.yml.
///
/// If ralph.yml exists, parses it and updates the RObot section.
/// If it doesn't exist, creates a minimal config.
fn save_robot_config(timeout: u64, bot_token: Option<&str>) -> Result<()> {
    let config_path = Path::new("ralph.yml");

    let robot = serde_yaml::Value::Mapping({
        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("enabled".to_string()),
            serde_yaml::Value::Bool(true),
        );
        m.insert(
            serde_yaml::Value::String("timeout_seconds".to_string()),
            serde_yaml::Value::Number(serde_yaml::Number::from(timeout)),
        );
        if let Some(token) = bot_token {
            let mut telegram = serde_yaml::Mapping::new();
            telegram.insert(
                serde_yaml::Value::String("bot_token".to_string()),
                serde_yaml::Value::String(token.to_string()),
            );
            m.insert(
                serde_yaml::Value::String("telegram".to_string()),
                serde_yaml::Value::Mapping(telegram),
            );
        }
        m
    });

    if config_path.exists() {
        // Read existing config as raw YAML value to preserve structure
        let content = std::fs::read_to_string(config_path).context("Failed to read ralph.yml")?;

        let mut doc: serde_yaml::Value =
            serde_yaml::from_str(&content).context("Failed to parse ralph.yml")?;

        // Update or insert RObot section
        if let serde_yaml::Value::Mapping(ref mut map) = doc {
            map.insert(serde_yaml::Value::String("RObot".to_string()), robot);
        }

        let yaml_str = serde_yaml::to_string(&doc).context("Failed to serialize config")?;
        std::fs::write(config_path, yaml_str).context("Failed to write ralph.yml")?;
    } else {
        // Create minimal config
        let yaml = if let Some(token) = bot_token {
            format!(
                "RObot:\n  enabled: true\n  timeout_seconds: {}\n  telegram:\n    bot_token: {}\n",
                timeout, token
            )
        } else {
            format!("RObot:\n  enabled: true\n  timeout_seconds: {}\n", timeout)
        };
        std::fs::write(config_path, yaml).context("Failed to create ralph.yml")?;
    }

    Ok(())
}

/// Save Rocket.Chat RObot config to ralph.yml.
///
/// If ralph.yml exists, parses it and updates the RObot section with rocketchat sub-key.
/// If it doesn't exist, creates a minimal config.
fn save_rocketchat_config(
    server_url: &str,
    bot_user_id: &str,
    room_id: &str,
    operator_id: Option<&str>,
) -> Result<()> {
    let config_path = Path::new("ralph.yml");

    let mut rc_map = serde_yaml::Mapping::new();
    rc_map.insert(
        serde_yaml::Value::String("server_url".to_string()),
        serde_yaml::Value::String(server_url.to_string()),
    );
    rc_map.insert(
        serde_yaml::Value::String("bot_user_id".to_string()),
        serde_yaml::Value::String(bot_user_id.to_string()),
    );
    rc_map.insert(
        serde_yaml::Value::String("room_id".to_string()),
        serde_yaml::Value::String(room_id.to_string()),
    );

    let mut robot_map = serde_yaml::Mapping::new();
    robot_map.insert(
        serde_yaml::Value::String("enabled".to_string()),
        serde_yaml::Value::Bool(true),
    );
    robot_map.insert(
        serde_yaml::Value::String("timeout_seconds".to_string()),
        serde_yaml::Value::Number(serde_yaml::Number::from(300u64)),
    );
    robot_map.insert(
        serde_yaml::Value::String("rocketchat".to_string()),
        serde_yaml::Value::Mapping(rc_map),
    );
    if let Some(oid) = operator_id {
        robot_map.insert(
            serde_yaml::Value::String("operator_id".to_string()),
            serde_yaml::Value::String(oid.to_string()),
        );
    }

    let robot = serde_yaml::Value::Mapping(robot_map);

    if config_path.exists() {
        let content = std::fs::read_to_string(config_path).context("Failed to read ralph.yml")?;
        let mut doc: serde_yaml::Value =
            serde_yaml::from_str(&content).context("Failed to parse ralph.yml")?;

        if let serde_yaml::Value::Mapping(ref mut map) = doc {
            map.insert(serde_yaml::Value::String("RObot".to_string()), robot);
        }

        let yaml_str = serde_yaml::to_string(&doc).context("Failed to serialize config")?;
        std::fs::write(config_path, yaml_str).context("Failed to write ralph.yml")?;
    } else {
        let mut lines = vec![
            "RObot:".to_string(),
            "  enabled: true".to_string(),
            "  timeout_seconds: 300".to_string(),
            "  rocketchat:".to_string(),
            format!("    server_url: {}", server_url),
            format!("    bot_user_id: {}", bot_user_id),
            format!("    room_id: {}", room_id),
        ];
        if let Some(oid) = operator_id {
            lines.push(format!("  operator_id: {}", oid));
        }
        lines.push(String::new()); // trailing newline
        std::fs::write(config_path, lines.join("\n")).context("Failed to create ralph.yml")?;
    }

    Ok(())
}

#[allow(dead_code)] // Called by onboard_matrix() in sub-task 5.6
fn save_matrix_config(
    homeserver_url: &str,
    room_id: &str,
    operator_id: Option<&str>,
) -> Result<()> {
    let config_path = Path::new("ralph.yml");

    let mut matrix_map = serde_yaml::Mapping::new();
    matrix_map.insert(
        serde_yaml::Value::String("homeserver_url".to_string()),
        serde_yaml::Value::String(homeserver_url.to_string()),
    );
    matrix_map.insert(
        serde_yaml::Value::String("room_id".to_string()),
        serde_yaml::Value::String(room_id.to_string()),
    );

    let mut robot_map = serde_yaml::Mapping::new();
    robot_map.insert(
        serde_yaml::Value::String("enabled".to_string()),
        serde_yaml::Value::Bool(true),
    );
    robot_map.insert(
        serde_yaml::Value::String("timeout_seconds".to_string()),
        serde_yaml::Value::Number(serde_yaml::Number::from(300u64)),
    );
    robot_map.insert(
        serde_yaml::Value::String("matrix".to_string()),
        serde_yaml::Value::Mapping(matrix_map),
    );
    if let Some(oid) = operator_id {
        robot_map.insert(
            serde_yaml::Value::String("operator_id".to_string()),
            serde_yaml::Value::String(oid.to_string()),
        );
    }

    let robot = serde_yaml::Value::Mapping(robot_map);

    if config_path.exists() {
        let content = std::fs::read_to_string(config_path).context("Failed to read ralph.yml")?;
        let mut doc: serde_yaml::Value =
            serde_yaml::from_str(&content).context("Failed to parse ralph.yml")?;

        if let serde_yaml::Value::Mapping(ref mut map) = doc {
            map.insert(serde_yaml::Value::String("RObot".to_string()), robot);
        }

        let yaml_str = serde_yaml::to_string(&doc).context("Failed to serialize config")?;
        std::fs::write(config_path, yaml_str).context("Failed to write ralph.yml")?;
    } else {
        let mut lines = vec![
            "RObot:".to_string(),
            "  enabled: true".to_string(),
            "  timeout_seconds: 300".to_string(),
            "  matrix:".to_string(),
            format!("    homeserver_url: {}", homeserver_url),
            format!("    room_id: {}", room_id),
        ];
        if let Some(oid) = operator_id {
            lines.push(format!("  operator_id: {}", oid));
        }
        lines.push(String::new()); // trailing newline
        std::fs::write(config_path, lines.join("\n")).context("Failed to create ralph.yml")?;
    }

    Ok(())
}

/// Write resolved config to a temporary runtime file so loop_runner receives a config path.
fn write_temp_config_for_daemon(workspace_root: &Path, config: &RalphConfig) -> Result<PathBuf> {
    let state_dir = workspace_root.join(".ralph");
    std::fs::create_dir_all(&state_dir).context("Failed to create .ralph directory")?;

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| anyhow::anyhow!("Failed to generate runtime config filename: {e}"))?
        .as_nanos();
    let path = state_dir.join(format!(
        "daemon-config-{}-{}.yml",
        std::process::id(),
        nanos
    ));

    let yaml = serde_yaml::to_string(config).context("Failed to serialize runtime config")?;
    std::fs::write(&path, yaml).context("Failed to write temporary runtime config")?;

    Ok(path)
}

/// Save only the bot token into a config file, preserving other keys.
fn save_bot_token_config(path: &Path, token: &str) -> Result<()> {
    let doc = if path.exists() {
        let content = std::fs::read_to_string(path).context("Failed to read config file")?;
        serde_yaml::from_str(&content).context("Failed to parse config file")?
    } else {
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
    };

    let mut root = match doc {
        serde_yaml::Value::Mapping(map) => map,
        _ => serde_yaml::Mapping::new(),
    };

    let robot_key = if root.contains_key("RObot") {
        serde_yaml::Value::String("RObot".to_string())
    } else if root.contains_key("robot") {
        serde_yaml::Value::String("robot".to_string())
    } else {
        serde_yaml::Value::String("RObot".to_string())
    };

    let mut robot_map = match root.get(&robot_key) {
        Some(serde_yaml::Value::Mapping(map)) => map.clone(),
        _ => serde_yaml::Mapping::new(),
    };

    let mut telegram_map = match robot_map.get("telegram") {
        Some(serde_yaml::Value::Mapping(map)) => map.clone(),
        _ => serde_yaml::Mapping::new(),
    };
    telegram_map.insert(
        serde_yaml::Value::String("bot_token".to_string()),
        serde_yaml::Value::String(token.to_string()),
    );
    robot_map.insert(
        serde_yaml::Value::String("telegram".to_string()),
        serde_yaml::Value::Mapping(telegram_map),
    );

    root.insert(robot_key, serde_yaml::Value::Mapping(robot_map));

    let yaml_str = serde_yaml::to_string(&serde_yaml::Value::Mapping(root))
        .context("Failed to serialize config")?;
    std::fs::write(path, yaml_str).context("Failed to write config file")?;
    Ok(())
}

/// Save Matrix access token into a config file, preserving other keys.
fn save_matrix_token_config(path: &Path, token: &str) -> Result<()> {
    let doc = if path.exists() {
        let content = std::fs::read_to_string(path).context("Failed to read config file")?;
        serde_yaml::from_str(&content).context("Failed to parse config file")?
    } else {
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
    };

    let mut root = match doc {
        serde_yaml::Value::Mapping(map) => map,
        _ => serde_yaml::Mapping::new(),
    };

    let robot_key = if root.contains_key("RObot") {
        serde_yaml::Value::String("RObot".to_string())
    } else if root.contains_key("robot") {
        serde_yaml::Value::String("robot".to_string())
    } else {
        serde_yaml::Value::String("RObot".to_string())
    };

    let mut robot_map = match root.get(&robot_key) {
        Some(serde_yaml::Value::Mapping(map)) => map.clone(),
        _ => serde_yaml::Mapping::new(),
    };

    let mut matrix_map = match robot_map.get("matrix") {
        Some(serde_yaml::Value::Mapping(map)) => map.clone(),
        _ => serde_yaml::Mapping::new(),
    };
    matrix_map.insert(
        serde_yaml::Value::String("access_token".to_string()),
        serde_yaml::Value::String(token.to_string()),
    );
    robot_map.insert(
        serde_yaml::Value::String("matrix".to_string()),
        serde_yaml::Value::Mapping(matrix_map),
    );

    root.insert(robot_key, serde_yaml::Value::Mapping(robot_map));

    let yaml_str = serde_yaml::to_string(&serde_yaml::Value::Mapping(root))
        .context("Failed to serialize config")?;
    std::fs::write(path, yaml_str).context("Failed to write config file")?;
    Ok(())
}

/// Save Rocket.Chat auth token into a config file, preserving other keys.
fn save_rocketchat_token_config(path: &Path, token: &str) -> Result<()> {
    let doc = if path.exists() {
        let content = std::fs::read_to_string(path).context("Failed to read config file")?;
        serde_yaml::from_str(&content).context("Failed to parse config file")?
    } else {
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
    };

    let mut root = match doc {
        serde_yaml::Value::Mapping(map) => map,
        _ => serde_yaml::Mapping::new(),
    };

    let robot_key = if root.contains_key("RObot") {
        serde_yaml::Value::String("RObot".to_string())
    } else if root.contains_key("robot") {
        serde_yaml::Value::String("robot".to_string())
    } else {
        serde_yaml::Value::String("RObot".to_string())
    };

    let mut robot_map = match root.get(&robot_key) {
        Some(serde_yaml::Value::Mapping(map)) => map.clone(),
        _ => serde_yaml::Mapping::new(),
    };

    let mut rc_map = match robot_map.get("rocketchat") {
        Some(serde_yaml::Value::Mapping(map)) => map.clone(),
        _ => serde_yaml::Mapping::new(),
    };
    rc_map.insert(
        serde_yaml::Value::String("auth_token".to_string()),
        serde_yaml::Value::String(token.to_string()),
    );
    robot_map.insert(
        serde_yaml::Value::String("rocketchat".to_string()),
        serde_yaml::Value::Mapping(rc_map),
    );

    root.insert(robot_key, serde_yaml::Value::Mapping(robot_map));

    let yaml_str = serde_yaml::to_string(&serde_yaml::Value::Mapping(root))
        .context("Failed to serialize config")?;
    std::fs::write(path, yaml_str).context("Failed to write config file")?;
    Ok(())
}

/// Save telegram state with chat_id.
fn save_telegram_state(chat_id: i64) -> Result<()> {
    let state_dir = Path::new(".ralph");
    if !state_dir.exists() {
        std::fs::create_dir_all(state_dir).context("Failed to create .ralph directory")?;
    }

    let state = serde_json::json!({
        "chat_id": chat_id,
        "last_seen": null,
        "last_update_id": null,
        "pending_questions": {}
    });

    let state_path = state_dir.join("telegram-state.json");
    let content =
        serde_json::to_string_pretty(&state).context("Failed to serialize telegram state")?;
    std::fs::write(&state_path, format!("{}\n", content))
        .context("Failed to write telegram-state.json")?;

    Ok(())
}

/// Read bot token from a config file (legacy).
fn load_config_bot_token_from(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let config: serde_yaml::Value = serde_yaml::from_str(&content).ok()?;
    config
        .get("RObot")
        .or_else(|| config.get("robot"))
        .and_then(|r| r.get("telegram"))
        .and_then(|t| t.get("bot_token"))
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// Read bot token from ralph.yml (legacy).
fn load_config_bot_token() -> Option<String> {
    load_config_bot_token_from(Path::new("ralph.yml"))
}

/// Read custom Telegram API URL from a config file.
fn load_config_api_url_from(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let config: serde_yaml::Value = serde_yaml::from_str(&content).ok()?;
    config
        .get("RObot")
        .or_else(|| config.get("robot"))
        .and_then(|r| r.get("telegram"))
        .and_then(|t| t.get("api_url"))
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// Detected RObot backend from config file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Backend {
    Telegram,
    RocketChat,
    Matrix,
    None,
}

/// Detect which RObot backend is configured in `ralph.yml`.
///
/// Checks the `RObot` (or `robot`) section for `rocketchat`, `matrix`, and `telegram`
/// sub-keys. Returns [`Backend::None`] if none is present or the config
/// file cannot be read. Priority: rocketchat > matrix > telegram.
pub(crate) fn detect_configured_backend() -> Backend {
    detect_configured_backend_from(Path::new("ralph.yml"))
}

/// Testable version that accepts an arbitrary config path.
fn detect_configured_backend_from(path: &Path) -> Backend {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Backend::None,
    };
    let config: serde_yaml::Value = match serde_yaml::from_str(&content) {
        Ok(c) => c,
        Err(_) => return Backend::None,
    };

    let robot = config.get("RObot").or_else(|| config.get("robot"));

    let robot = match robot {
        Some(r) => r,
        None => return Backend::None,
    };

    let has_rc = robot.get("rocketchat").is_some();
    let has_matrix = robot.get("matrix").is_some();
    let has_tg = robot.get("telegram").is_some();

    match (has_rc, has_matrix, has_tg) {
        (true, _, _) => Backend::RocketChat,
        (_, true, _) => Backend::Matrix,
        (_, _, true) => Backend::Telegram,
        _ => Backend::None,
    }
}

/// Check if RObot is enabled in config.
fn is_robot_enabled() -> bool {
    let content = match std::fs::read_to_string("ralph.yml") {
        Ok(c) => c,
        Err(_) => return false,
    };
    let config: serde_yaml::Value = match serde_yaml::from_str(&content) {
        Ok(c) => c,
        Err(_) => return false,
    };
    config
        .get("RObot")
        .and_then(|r| r.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

fn normalize_token(value: Option<String>) -> Option<String> {
    value.and_then(|raw| {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn resolve_token_from(
    env_token: Option<String>,
    keychain_token: Option<String>,
    config_token: Option<String>,
) -> Option<String> {
    normalize_token(env_token)
        .or_else(|| normalize_token(keychain_token))
        .or_else(|| normalize_token(config_token))
}

/// Resolve token from all sources (env > keychain > config).
pub(crate) fn resolve_token() -> Option<String> {
    resolve_token_from(
        std::env::var("RALPH_TELEGRAM_BOT_TOKEN").ok(),
        load_bot_token(),
        load_config_bot_token(),
    )
}

/// Resolve chat_id from telegram state.
pub(crate) fn resolve_chat_id() -> Option<i64> {
    let content = std::fs::read_to_string(".ralph/telegram-state.json").ok()?;
    let state: serde_json::Value = serde_json::from_str(&content).ok()?;
    state.get("chat_id").and_then(|v| v.as_i64())
}

// ─────────────────────────────────────────────────────────────────────────────
// INPUT HELPERS
// ─────────────────────────────────────────────────────────────────────────────

/// Prompt user for bot token with retry on empty input.
fn prompt_token() -> Result<String> {
    loop {
        print!("  Paste your bot token: ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .context("Failed to read input")?;
        let token = input.trim().to_string();
        if token.is_empty() {
            println!("  Token cannot be empty. Please try again.");
            continue;
        }
        return Ok(token);
    }
}

/// Prompt user for a required input value with retry on empty.
fn prompt_input(prompt: &str) -> Result<String> {
    loop {
        print!("{prompt}");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .context("Failed to read input")?;
        let value = input.trim().to_string();
        if value.is_empty() {
            println!("  Value cannot be empty. Please try again.");
            continue;
        }
        return Ok(value);
    }
}

/// Prompt user for an optional input value (empty string is allowed).
fn prompt_input_optional(prompt: &str) -> Result<String> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("Failed to read input")?;
    Ok(input.trim().to_string())
}

// ─────────────────────────────────────────────────────────────────────────────
// OUTPUT HELPERS
// ─────────────────────────────────────────────────────────────────────────────

fn print_success(use_colors: bool, msg: &str) {
    if use_colors {
        println!("  \x1b[32m\u{2713}\x1b[0m {}", msg);
    } else {
        println!("  OK: {}", msg);
    }
}

fn print_error(use_colors: bool, msg: &str) {
    if use_colors {
        println!("  \x1b[31m\u{2717}\x1b[0m {}", msg);
    } else {
        println!("  ERROR: {}", msg);
    }
}

fn print_warning(use_colors: bool, msg: &str) {
    if use_colors {
        println!("  \x1b[33m!\x1b[0m {}", msg);
    } else {
        println!("  WARN: {}", msg);
    }
}

fn print_status(use_colors: bool, msg: &str) {
    if use_colors {
        println!("  \x1b[2m-\x1b[0m {}", msg);
    } else {
        println!("  {}", msg);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// TESTS
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::CwdGuard;
    use std::path::PathBuf;

    #[test]
    fn test_normalize_token_trims_and_discards_empty() {
        assert_eq!(normalize_token(None), None);
        assert_eq!(
            normalize_token(Some("  token-123  ".to_string())),
            Some("token-123".to_string())
        );
        assert_eq!(normalize_token(Some("   ".to_string())), None);
    }

    #[test]
    fn test_resolve_token_from_prefers_env_then_keychain_then_config() {
        let resolved = resolve_token_from(
            Some("  env-token  ".to_string()),
            Some("key-token".to_string()),
            Some("config-token".to_string()),
        );
        assert_eq!(resolved.as_deref(), Some("env-token"));

        let resolved = resolve_token_from(
            Some("   ".to_string()),
            Some("  key-token  ".to_string()),
            Some("config-token".to_string()),
        );
        assert_eq!(resolved.as_deref(), Some("key-token"));

        let resolved = resolve_token_from(None, None, Some("  cfg  ".to_string()));
        assert_eq!(resolved.as_deref(), Some("cfg"));
    }

    #[tokio::test]
    async fn test_run_daemon_rejects_builtin_config() {
        let sources = vec![ConfigSource::Builtin("tdd".to_string())];

        let err = run_daemon(DaemonArgs {}, &sources, None, false)
            .await
            .expect_err("expected daemon setup error");
        assert!(
            !err.to_string()
                .contains("Builtin presets are not supported"),
            "unexpected unsupported-config error: {err}"
        );
    }

    #[tokio::test]
    async fn test_run_daemon_rejects_remote_config() {
        let sources = vec![ConfigSource::Remote(
            "https://example.com/ralph.yml".to_string(),
        )];

        let err = run_daemon(DaemonArgs {}, &sources, None, false)
            .await
            .expect_err("expected remote config error");
        assert!(
            err.to_string()
                .contains("Failed to load config for bot daemon"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_run_daemon_errors_on_missing_config_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::set(temp_dir.path());

        let sources = vec![ConfigSource::File(PathBuf::from("missing.yml"))];
        let err = run_daemon(DaemonArgs {}, &sources, None, false)
            .await
            .expect_err("expected missing config error");
        assert!(
            err.to_string().contains("Config file not found"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_save_telegram_state_creates_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::set(temp_dir.path());

        save_telegram_state(123_456_789).expect("save telegram state");

        let state_path = temp_dir.path().join(".ralph").join("telegram-state.json");

        // Verify the file was created with correct content
        let read_content = std::fs::read_to_string(&state_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&read_content).unwrap();
        assert_eq!(
            parsed.get("chat_id").unwrap().as_i64().unwrap(),
            123_456_789
        );
        assert!(parsed.get("pending_questions").unwrap().is_object());
    }

    #[test]
    fn test_save_robot_config_creates_minimal_config_without_token() {
        let temp_dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::set(temp_dir.path());

        save_robot_config(180, None).expect("save robot config");

        let content = std::fs::read_to_string("ralph.yml").unwrap();
        let config: serde_yaml::Value = serde_yaml::from_str(&content).unwrap();
        let robot = config.get("RObot").unwrap();
        assert!(robot.get("enabled").unwrap().as_bool().unwrap());
        assert_eq!(
            robot.get("timeout_seconds").and_then(|v| v.as_u64()),
            Some(180)
        );
        assert!(robot.get("telegram").is_none());
    }

    #[test]
    fn test_telegram_get_me_parses_response() {
        // Test JSON parsing logic (not actual API call)
        let body: serde_json::Value = serde_json::from_str(
            r#"{
                "ok": true,
                "result": {
                    "id": 123456,
                    "is_bot": true,
                    "first_name": "Ralph Bot",
                    "username": "ralph_test_bot"
                }
            }"#,
        )
        .unwrap();

        let result = body.get("result").unwrap();
        let first_name = result
            .get("first_name")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown");
        let username = result
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown_bot");

        assert_eq!(first_name, "Ralph Bot");
        assert_eq!(username, "ralph_test_bot");
    }

    #[test]
    fn test_telegram_get_updates_parses_message() {
        // Test JSON parsing logic for update with message
        let body: serde_json::Value = serde_json::from_str(
            r#"{
                "ok": true,
                "result": [{
                    "update_id": 100,
                    "message": {
                        "message_id": 1,
                        "from": {
                            "id": 999,
                            "first_name": "John",
                            "last_name": "Doe"
                        },
                        "chat": {
                            "id": 999,
                            "type": "private"
                        },
                        "text": "hello"
                    }
                }]
            }"#,
        )
        .unwrap();

        let results = body.get("result").unwrap().as_array().unwrap();
        assert_eq!(results.len(), 1);

        let update = &results[0];
        let message = update.get("message").unwrap();
        let chat_id = message
            .get("chat")
            .unwrap()
            .get("id")
            .unwrap()
            .as_i64()
            .unwrap();
        assert_eq!(chat_id, 999);

        let from = message.get("from").unwrap();
        let first_name = from.get("first_name").unwrap().as_str().unwrap();
        let last_name = from.get("last_name").unwrap().as_str().unwrap();
        assert_eq!(format!("{} {}", first_name, last_name), "John Doe");
    }

    #[test]
    fn test_robot_config_yaml_generation() {
        // Test that we generate valid YAML for a minimal config
        let yaml = format!("RObot:\n  enabled: true\n  timeout_seconds: {}\n", 300);
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        let robot = parsed.get("RObot").unwrap();
        assert!(robot.get("enabled").unwrap().as_bool().unwrap());
        assert_eq!(robot.get("timeout_seconds").unwrap().as_u64().unwrap(), 300);
    }

    #[test]
    fn test_robot_config_update_preserves_existing() {
        // Test that updating an existing config preserves other fields
        let existing_yaml = "cli:\n  backend: claude\nevent_loop:\n  max_iterations: 50\n";
        let mut doc: serde_yaml::Value = serde_yaml::from_str(existing_yaml).unwrap();

        let robot = serde_yaml::Value::Mapping({
            let mut m = serde_yaml::Mapping::new();
            m.insert(
                serde_yaml::Value::String("enabled".to_string()),
                serde_yaml::Value::Bool(true),
            );
            m.insert(
                serde_yaml::Value::String("timeout_seconds".to_string()),
                serde_yaml::Value::Number(serde_yaml::Number::from(300_u64)),
            );
            m
        });

        if let serde_yaml::Value::Mapping(ref mut map) = doc {
            map.insert(serde_yaml::Value::String("RObot".to_string()), robot);
        }

        // Verify existing fields preserved
        assert!(doc.get("cli").is_some());
        assert!(doc.get("event_loop").is_some());
        // Verify RObot added
        let robot = doc.get("RObot").unwrap();
        assert!(robot.get("enabled").unwrap().as_bool().unwrap());
    }

    #[test]
    fn test_telegram_send_message_payload() {
        // Test that we build the correct JSON payload
        let payload = serde_json::json!({
            "chat_id": 123_456_789_i64,
            "text": "Hello from Ralph!",
        });

        assert_eq!(payload["chat_id"].as_i64().unwrap(), 123_456_789);
        assert_eq!(payload["text"].as_str().unwrap(), "Hello from Ralph!");
    }

    #[test]
    fn test_telegram_error_response_parsing() {
        let body: serde_json::Value = serde_json::from_str(
            r#"{
                "ok": false,
                "error_code": 401,
                "description": "Unauthorized"
            }"#,
        )
        .unwrap();

        let is_ok = body.get("ok") == Some(&serde_json::Value::Bool(true));
        assert!(!is_ok);

        let description = body
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown error");
        assert_eq!(description, "Unauthorized");
    }

    #[test]
    fn test_save_robot_config_with_token_writes_bot_token() {
        let temp_dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::set(temp_dir.path());

        save_robot_config(300, Some("test-token")).unwrap();

        let content = std::fs::read_to_string("ralph.yml").unwrap();
        let config: serde_yaml::Value = serde_yaml::from_str(&content).unwrap();
        let token = config
            .get("RObot")
            .and_then(|r| r.get("telegram"))
            .and_then(|t| t.get("bot_token"))
            .and_then(|v| v.as_str());
        assert_eq!(token, Some("test-token"));
    }

    #[test]
    fn test_save_robot_config_updates_existing_config() {
        let temp_dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::set(temp_dir.path());

        std::fs::write("ralph.yml", "cli:\n  backend: claude\n").unwrap();

        save_robot_config(120, None).unwrap();

        let content = std::fs::read_to_string("ralph.yml").unwrap();
        let config: serde_yaml::Value = serde_yaml::from_str(&content).unwrap();
        assert!(config.get("cli").is_some());
        let robot = config.get("RObot").unwrap();
        assert_eq!(
            robot.get("timeout_seconds").and_then(|v| v.as_u64()),
            Some(120)
        );
    }

    #[test]
    fn test_load_config_bot_token_from_reads_token() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("custom.yml");
        let yaml = "RObot:\n  telegram:\n    bot_token: token-123\n";
        std::fs::write(&config_path, yaml).unwrap();

        let token = load_config_bot_token_from(&config_path);
        assert_eq!(token.as_deref(), Some("token-123"));
    }

    #[test]
    fn test_load_config_bot_token_from_reads_lowercase_robot() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("custom.yml");
        let yaml = "robot:\n  telegram:\n    bot_token: token-lower\n";
        std::fs::write(&config_path, yaml).unwrap();

        let token = load_config_bot_token_from(&config_path);
        assert_eq!(token.as_deref(), Some("token-lower"));
    }

    #[test]
    fn test_save_bot_token_config_writes_token() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("config.yml");

        save_bot_token_config(&config_path, "new-token").unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let config: serde_yaml::Value = serde_yaml::from_str(&content).unwrap();
        let token = config
            .get("RObot")
            .and_then(|r| r.get("telegram"))
            .and_then(|t| t.get("bot_token"))
            .and_then(|v| v.as_str());
        assert_eq!(token, Some("new-token"));
    }

    #[test]
    fn test_save_bot_token_config_preserves_existing() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("config.yml");
        let yaml = "cli:\n  backend: claude\nRObot:\n  enabled: true\n";
        std::fs::write(&config_path, yaml).unwrap();

        save_bot_token_config(&config_path, "new-token").unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let config: serde_yaml::Value = serde_yaml::from_str(&content).unwrap();
        assert!(config.get("cli").is_some());
        let robot = config.get("RObot").unwrap();
        assert_eq!(robot.get("enabled").and_then(|v| v.as_bool()), Some(true));
        let token = robot
            .get("telegram")
            .and_then(|t| t.get("bot_token"))
            .and_then(|v| v.as_str());
        assert_eq!(token, Some("new-token"));
    }

    #[test]
    fn test_save_bot_token_config_updates_lowercase_robot_key() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("config.yml");
        let yaml = "robot:\n  enabled: true\n";
        std::fs::write(&config_path, yaml).unwrap();

        save_bot_token_config(&config_path, "token-xyz").unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let config: serde_yaml::Value = serde_yaml::from_str(&content).unwrap();
        let token = config
            .get("robot")
            .and_then(|r| r.get("telegram"))
            .and_then(|t| t.get("bot_token"))
            .and_then(|v| v.as_str());
        assert_eq!(token, Some("token-xyz"));
        assert!(config.get("RObot").is_none());
    }

    #[test]
    fn test_load_config_bot_token_reads_legacy_config() {
        let temp_dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::set(temp_dir.path());
        std::fs::write(
            temp_dir.path().join("ralph.yml"),
            "RObot:\n  telegram:\n    bot_token: legacy-token\n",
        )
        .unwrap();

        assert_eq!(load_config_bot_token().as_deref(), Some("legacy-token"));
    }

    #[test]
    fn test_is_robot_enabled_reads_config() {
        let temp_dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::set(temp_dir.path());
        std::fs::write(
            temp_dir.path().join("ralph.yml"),
            "RObot:\n  enabled: true\n",
        )
        .unwrap();

        assert!(is_robot_enabled());

        std::fs::write(
            temp_dir.path().join("ralph.yml"),
            "RObot:\n  enabled: false\n",
        )
        .unwrap();

        assert!(!is_robot_enabled());
    }

    #[test]
    fn test_resolve_chat_id_reads_state_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::set(temp_dir.path());
        std::fs::create_dir_all(".ralph").unwrap();
        std::fs::write(
            ".ralph/telegram-state.json",
            r#"{"chat_id": 4242, "pending_questions": {}}"#,
        )
        .unwrap();

        assert_eq!(resolve_chat_id(), Some(4242));
    }

    #[test]
    fn test_resolve_chat_id_missing_file_returns_none() {
        let temp_dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::set(temp_dir.path());

        assert_eq!(resolve_chat_id(), None);
    }

    #[test]
    fn test_resolve_token_from_prefers_env_and_trims() {
        let resolved = resolve_token_from(
            Some("  env-token  ".to_string()),
            Some("keychain-token".to_string()),
            Some("config-token".to_string()),
        );

        assert_eq!(resolved.as_deref(), Some("env-token"));
    }

    #[test]
    fn test_resolve_token_from_skips_empty_values() {
        let resolved = resolve_token_from(
            Some("   ".to_string()),
            Some(String::new()),
            Some(" config-token ".to_string()),
        );

        assert_eq!(resolved.as_deref(), Some("config-token"));
    }

    #[test]
    fn test_resolve_token_from_returns_none_when_all_empty() {
        let resolved = resolve_token_from(
            Some("   ".to_string()),
            Some(String::new()),
            Some("   ".to_string()),
        );

        assert_eq!(resolved, None);
    }

    #[test]
    fn test_is_robot_enabled_missing_config_returns_false() {
        let temp_dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::set(temp_dir.path());

        assert!(!is_robot_enabled());
    }

    #[test]
    fn test_is_robot_enabled_invalid_yaml_returns_false() {
        let temp_dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::set(temp_dir.path());
        std::fs::write(temp_dir.path().join("ralph.yml"), "not: [valid").unwrap();

        assert!(!is_robot_enabled());
    }

    #[test]
    fn test_resolve_chat_id_invalid_json_returns_none() {
        let temp_dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::set(temp_dir.path());
        std::fs::create_dir_all(".ralph").unwrap();
        std::fs::write(".ralph/telegram-state.json", "not-json").unwrap();

        assert_eq!(resolve_chat_id(), None);
    }

    #[test]
    fn test_load_config_bot_token_from_missing_file_returns_none() {
        let temp_dir = tempfile::tempdir().unwrap();
        let missing_path = temp_dir.path().join("missing.yml");

        assert_eq!(load_config_bot_token_from(&missing_path), None);
    }

    // ── detect_configured_backend tests ─────────────────────────────────

    #[test]
    fn test_detect_backend_telegram() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("ralph.yml");
        std::fs::write(
            &config_path,
            "RObot:\n  enabled: true\n  telegram:\n    bot_token: tok\n",
        )
        .unwrap();

        assert_eq!(
            detect_configured_backend_from(&config_path),
            Backend::Telegram
        );
    }

    #[test]
    fn test_detect_backend_rocketchat() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("ralph.yml");
        std::fs::write(
            &config_path,
            "RObot:\n  enabled: true\n  rocketchat:\n    server_url: https://rc.example.com\n",
        )
        .unwrap();

        assert_eq!(
            detect_configured_backend_from(&config_path),
            Backend::RocketChat
        );
    }

    #[test]
    fn test_detect_backend_none_when_no_backend_section() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("ralph.yml");
        std::fs::write(&config_path, "RObot:\n  enabled: true\n").unwrap();

        assert_eq!(detect_configured_backend_from(&config_path), Backend::None);
    }

    #[test]
    fn test_detect_backend_none_when_no_robot_section() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("ralph.yml");
        std::fs::write(&config_path, "cli:\n  backend: claude\n").unwrap();

        assert_eq!(detect_configured_backend_from(&config_path), Backend::None);
    }

    #[test]
    fn test_detect_backend_none_when_file_missing() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("missing.yml");

        assert_eq!(detect_configured_backend_from(&config_path), Backend::None);
    }

    #[test]
    fn test_detect_backend_lowercase_robot_key() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("ralph.yml");
        std::fs::write(&config_path, "robot:\n  telegram:\n    bot_token: tok\n").unwrap();

        assert_eq!(
            detect_configured_backend_from(&config_path),
            Backend::Telegram
        );
    }

    #[test]
    fn test_detect_backend_rocketchat_wins_when_both_present() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("ralph.yml");
        std::fs::write(
            &config_path,
            "RObot:\n  telegram:\n    bot_token: tok\n  rocketchat:\n    server_url: https://rc.example.com\n",
        )
        .unwrap();

        // rocketchat takes priority (config validation rejects this, but detection is deterministic)
        assert_eq!(
            detect_configured_backend_from(&config_path),
            Backend::RocketChat
        );
    }

    #[test]
    fn test_detect_backend_invalid_yaml_returns_none() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("ralph.yml");
        std::fs::write(&config_path, "not: [valid yaml").unwrap();

        assert_eq!(detect_configured_backend_from(&config_path), Backend::None);
    }

    #[test]
    fn test_detect_backend_matrix() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("ralph.yml");
        std::fs::write(
            &config_path,
            "RObot:\n  enabled: true\n  matrix:\n    homeserver_url: https://matrix.example.com\n",
        )
        .unwrap();

        assert_eq!(
            detect_configured_backend_from(&config_path),
            Backend::Matrix
        );
    }

    #[test]
    fn test_detect_backend_matrix_and_telegram() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("ralph.yml");
        std::fs::write(
            &config_path,
            "RObot:\n  telegram:\n    bot_token: tok\n  matrix:\n    homeserver_url: https://matrix.example.com\n",
        )
        .unwrap();

        // matrix takes priority over telegram (rc > matrix > tg)
        assert_eq!(
            detect_configured_backend_from(&config_path),
            Backend::Matrix
        );
    }

    #[test]
    fn test_detect_backend_rc_wins_over_matrix() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("ralph.yml");
        std::fs::write(
            &config_path,
            "RObot:\n  matrix:\n    homeserver_url: https://matrix.example.com\n  rocketchat:\n    server_url: https://rc.example.com\n",
        )
        .unwrap();

        // rocketchat takes priority over matrix (rc > matrix > tg)
        assert_eq!(
            detect_configured_backend_from(&config_path),
            Backend::RocketChat
        );
    }

    // ── onboard argument routing tests ─────────────────────────────────

    fn parse_bot_args(args: &[&str]) -> BotArgs {
        use clap::Parser;
        BotArgs::try_parse_from(std::iter::once("bot").chain(args.iter().copied()))
            .expect("failed to parse BotArgs")
    }

    #[test]
    fn test_onboard_defaults_to_telegram_backend() {
        let bot = parse_bot_args(&["onboard"]);
        match bot.command {
            BotCommands::Onboard(ref args) => {
                assert_eq!(args.backend, "telegram");
            }
            _ => panic!("expected Onboard command"),
        }
    }

    #[test]
    fn test_onboard_backend_rocketchat() {
        let bot = parse_bot_args(&["onboard", "--backend", "rocketchat"]);
        match bot.command {
            BotCommands::Onboard(ref args) => {
                assert_eq!(args.backend, "rocketchat");
            }
            _ => panic!("expected Onboard command"),
        }
    }

    #[test]
    fn test_onboard_rocketchat_specific_flags() {
        let bot = parse_bot_args(&[
            "onboard",
            "--backend",
            "rocketchat",
            "--server-url",
            "https://rc.example.com",
            "--bot-user-id",
            "bot123",
            "--auth-token",
            "secret-token",
            "--room-id",
            "GENERAL",
            "--operator-id",
            "user456",
        ]);
        match bot.command {
            BotCommands::Onboard(ref args) => {
                assert_eq!(args.backend, "rocketchat");
                assert_eq!(args.server_url.as_deref(), Some("https://rc.example.com"));
                assert_eq!(args.bot_user_id.as_deref(), Some("bot123"));
                assert_eq!(args.auth_token.as_deref(), Some("secret-token"));
                assert_eq!(args.room_id.as_deref(), Some("GENERAL"));
                assert_eq!(args.operator_id.as_deref(), Some("user456"));
            }
            _ => panic!("expected Onboard command"),
        }
    }

    #[test]
    fn test_onboard_telegram_specific_flags() {
        let bot = parse_bot_args(&[
            "onboard",
            "--backend",
            "telegram",
            "--token",
            "123:ABC",
            "--chat-id",
            "987654",
            "--timeout",
            "60",
        ]);
        match bot.command {
            BotCommands::Onboard(ref args) => {
                assert_eq!(args.backend, "telegram");
                assert_eq!(args.token.as_deref(), Some("123:ABC"));
                assert_eq!(args.chat_id, Some(987_654));
                assert_eq!(args.timeout, 60);
            }
            _ => panic!("expected Onboard command"),
        }
    }

    #[test]
    fn test_onboard_rc_flags_default_to_none() {
        let bot = parse_bot_args(&["onboard", "--backend", "rocketchat"]);
        match bot.command {
            BotCommands::Onboard(ref args) => {
                assert_eq!(args.backend, "rocketchat");
                assert!(args.server_url.is_none());
                assert!(args.bot_user_id.is_none());
                assert!(args.auth_token.is_none());
                assert!(args.room_id.is_none());
                assert!(args.operator_id.is_none());
            }
            _ => panic!("expected Onboard command"),
        }
    }

    #[test]
    fn test_onboard_backend_matrix() {
        let bot = parse_bot_args(&["onboard", "--backend", "matrix"]);
        match bot.command {
            BotCommands::Onboard(ref args) => {
                assert_eq!(args.backend, "matrix");
            }
            _ => panic!("expected Onboard command"),
        }
    }

    #[test]
    fn test_onboard_matrix_specific_flags() {
        let bot = parse_bot_args(&[
            "onboard",
            "--backend",
            "matrix",
            "--homeserver-url",
            "https://matrix.example.com",
            "--access-token",
            "syt_secret_token_123",
            "--room-id",
            "!room123:example.com",
            "--operator-id",
            "@user:example.com",
        ]);
        match bot.command {
            BotCommands::Onboard(ref args) => {
                assert_eq!(args.backend, "matrix");
                assert_eq!(
                    args.homeserver_url.as_deref(),
                    Some("https://matrix.example.com")
                );
                assert_eq!(args.access_token.as_deref(), Some("syt_secret_token_123"));
                assert_eq!(args.room_id.as_deref(), Some("!room123:example.com"));
                assert_eq!(args.operator_id.as_deref(), Some("@user:example.com"));
            }
            _ => panic!("expected Onboard command"),
        }
    }

    #[test]
    fn test_onboard_matrix_flags_default_to_none() {
        let bot = parse_bot_args(&["onboard", "--backend", "matrix"]);
        match bot.command {
            BotCommands::Onboard(ref args) => {
                assert_eq!(args.backend, "matrix");
                assert!(args.homeserver_url.is_none());
                assert!(args.access_token.is_none());
                assert!(args.room_id.is_none());
                assert!(args.operator_id.is_none());
            }
            _ => panic!("expected Onboard command"),
        }
    }

    #[tokio::test]
    async fn test_execute_onboard_unknown_backend_errors() {
        let args = BotArgs {
            command: BotCommands::Onboard(OnboardArgs {
                backend: "slack".to_string(),
                token: None,
                chat_id: None,
                timeout: 120,
                server_url: None,
                bot_user_id: None,
                auth_token: None,
                room_id: None,
                operator_id: None,
                homeserver_url: None,
                access_token: None,
            }),
        };
        let err = execute(args, &[], None, false)
            .await
            .expect_err("expected error for unknown backend");
        assert!(
            err.to_string().contains("Unknown backend"),
            "unexpected error: {err}"
        );
        assert!(
            err.to_string().contains("slack"),
            "error should mention the bad backend: {err}"
        );
    }
}
