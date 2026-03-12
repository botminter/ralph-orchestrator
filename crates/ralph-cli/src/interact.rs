//! Interact commands for human-in-the-loop communication.
//!
//! Provides non-blocking notification tools for agents:
//! - `ralph tools interact progress "message"` — Send a progress update via the configured backend

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::bot;

#[derive(Parser, Debug)]
pub struct InteractArgs {
    #[command(subcommand)]
    pub command: InteractCommands,
}

#[derive(Subcommand, Debug)]
pub enum InteractCommands {
    /// Send a non-blocking progress update via the configured RObot backend
    Progress(ProgressArgs),
}

#[derive(Parser, Debug)]
pub struct ProgressArgs {
    /// The message to send
    pub message: String,
}

pub async fn execute(args: InteractArgs) -> Result<()> {
    match args.command {
        InteractCommands::Progress(progress_args) => send_progress(progress_args).await,
    }
}

async fn send_progress(args: ProgressArgs) -> Result<()> {
    let backend = bot::detect_configured_backend();

    match backend {
        bot::Backend::Telegram => send_progress_telegram(&args.message).await,
        bot::Backend::RocketChat => send_progress_rocketchat(&args.message).await,
        bot::Backend::Matrix => send_progress_matrix(&args.message).await,
        bot::Backend::None => {
            anyhow::bail!(
                "No RObot backend configured. Run `ralph bot onboard` to set up Telegram, Rocket.Chat, or Matrix."
            );
        }
    }
}

async fn send_progress_telegram(message: &str) -> Result<()> {
    let token = bot::resolve_token()
        .context("No bot token. Run `ralph bot onboard` or set RALPH_TELEGRAM_BOT_TOKEN")?;
    let chat_id =
        bot::resolve_chat_id().context("No chat_id found. Run `ralph bot onboard` to detect it")?;

    bot::telegram_send_message(&token, chat_id, message).await?;

    println!("Sent.");
    Ok(())
}

async fn send_progress_rocketchat(message: &str) -> Result<()> {
    use ralph_rocketchat::client::{RocketChatApi, RocketChatClient};

    let config_path = std::path::Path::new("ralph.yml");
    let config = ralph_core::RalphConfig::from_file(config_path)
        .with_context(|| format!("Failed to load config from {}", config_path.display()))?;

    let rc = config
        .robot
        .rocketchat
        .as_ref()
        .context("RObot.rocketchat section missing from ralph.yml")?;

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

    let client = RocketChatClient::new(&server_url, &auth_token, bot_user_id);
    client.send_message(room_id, message, None).await?;

    println!("Sent.");
    Ok(())
}

async fn send_progress_matrix(message: &str) -> Result<()> {
    use ralph_matrix::{MatrixApi, MatrixClient};

    let config_path = std::path::Path::new("ralph.yml");
    let config = ralph_core::RalphConfig::from_file(config_path)
        .with_context(|| format!("Failed to load config from {}", config_path.display()))?;

    let mx = config
        .robot
        .matrix
        .as_ref()
        .context("RObot.matrix section missing from ralph.yml")?;

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

    let client = MatrixClient::new();
    client
        .login_with_token(&homeserver_url, &access_token)
        .await
        .context("Failed to authenticate with Matrix homeserver")?;

    client
        .ensure_room(room_id)
        .await
        .context("Failed to sync/join Matrix room")?;

    client.send_message(room_id, message, None).await?;

    println!("Sent.");
    Ok(())
}
