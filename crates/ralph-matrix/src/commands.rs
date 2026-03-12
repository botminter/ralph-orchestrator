//! Bot command handling for the Matrix daemon.
//!
//! Provides the shared command set (`!help`, `!status`, `!tasks`, `!memories`,
//! `!tail`, `!model`, `!models`, `!restart`, `!stop`) with Matrix markdown
//! formatting.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::loop_lock::{LockState, lock_path, lock_state};

/// Check if a message is a bot command (starts with `!`).
pub fn is_command(text: &str) -> bool {
    text.starts_with('!')
}

/// Parse and execute a bot command, returning the response message.
///
/// Returns `Some(response)` if the text was a recognized command,
/// or `None` if the command was not recognized (so the caller can
/// treat it as a regular message).
pub fn handle_command(text: &str, workspace_root: &Path) -> Option<String> {
    let (command, args) = parse_command(text);
    match command {
        "!help" => Some(cmd_help()),
        "!status" => Some(cmd_status(workspace_root)),
        "!tasks" => Some(cmd_tasks(workspace_root)),
        "!memories" => Some(cmd_memories(workspace_root)),
        "!tail" => Some(cmd_tail(workspace_root)),
        "!model" => Some(cmd_model(workspace_root, args)),
        "!models" => Some(cmd_models(workspace_root)),
        "!restart" => Some(cmd_restart(workspace_root)),
        "!stop" => Some(cmd_stop(workspace_root)),
        _ => None,
    }
}

/// Split a command string into the command name and optional arguments.
fn parse_command(text: &str) -> (&str, &str) {
    if let Some((first, rest)) = text.split_once(char::is_whitespace) {
        (first, rest.trim())
    } else {
        (text, "")
    }
}

fn truncate_with_ellipsis(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        input.to_string()
    } else {
        let mut truncated: String = input.chars().take(max_chars).collect();
        truncated.push_str("...");
        truncated
    }
}

/// `!help` — List available commands.
fn cmd_help() -> String {
    [
        "**Ralph Bot Commands**",
        "",
        "!status — Current loop status",
        "!tasks — Open tasks",
        "!memories — Recent memories",
        "!tail — Last 20 events",
        "!model — Show current backend/model",
        "!models — Show configured model options",
        "!restart — Restart the orchestration loop",
        "!stop — Stop the orchestration loop",
        "!help — This message",
    ]
    .join("\n")
}

#[derive(Debug, Clone)]
struct BackendModelInfo {
    backend: Option<String>,
    model: Option<String>,
    source: String,
}

/// `!model` — Show the active backend/model (or configured fallback).
fn cmd_model(workspace_root: &Path, args: &str) -> String {
    let requested = args.trim();
    if !requested.is_empty() {
        return [
            "**!model is read-only right now.**".to_string(),
            format!("Requested: `{}`", requested),
            "Change the model via config/CLI args, then restart Ralph.".to_string(),
            "Example: `ralph run -b pi -- --model gpt-5.3-codex`".to_string(),
        ]
        .join("\n");
    }

    let Some(info) = detect_backend_model_info(workspace_root) else {
        return [
            "**Model**".to_string(),
            String::new(),
            "No backend/model detected from runtime or config.".to_string(),
            "Use `!models` for config hints.".to_string(),
        ]
        .join("\n");
    };

    [
        "**Model**".to_string(),
        String::new(),
        format!(
            "Backend: `{}`",
            info.backend.as_deref().unwrap_or("unknown")
        ),
        format!("Model: `{}`", info.model.as_deref().unwrap_or("not set")),
        format!("Source: `{}`", info.source),
    ]
    .join("\n")
}

/// `!models` — Show models discovered from Ralph config files.
fn cmd_models(workspace_root: &Path) -> String {
    let mut models = BTreeSet::new();
    for path in candidate_config_paths(workspace_root) {
        if let Some(info) = parse_backend_model_from_yaml_file(&path)
            && let Some(model) = info.model
        {
            models.insert(model);
        }
    }

    let mut lines = vec!["**Configured Models**".to_string(), String::new()];

    if let Some(current) = detect_backend_model_info(workspace_root) {
        lines.push(format!(
            "Current backend: `{}`",
            current.backend.as_deref().unwrap_or("unknown")
        ));
        lines.push(format!(
            "Current model: `{}`",
            current.model.as_deref().unwrap_or("not set")
        ));
        lines.push(format!("Source: `{}`", current.source));
        lines.push(String::new());
    }

    if models.is_empty() {
        lines.push("No --model/-m entries found in ralph*.yml files.".to_string());
    } else {
        let plural = if models.len() == 1 { "" } else { "s" };
        lines.push(format!("Found {} model{} in config:", models.len(), plural));
        for model in models {
            lines.push(format!("• `{}`", model));
        }
    }

    lines.push(String::new());
    lines.push(
        "Tip: set a model with CLI args (e.g., `ralph run -b pi -- --model gpt-5.3-codex`)."
            .to_string(),
    );
    lines.join("\n")
}

fn detect_backend_model_info(workspace_root: &Path) -> Option<BackendModelInfo> {
    detect_backend_model_from_active_loop(workspace_root)
        .or_else(|| detect_backend_model_from_config_files(workspace_root))
}

fn detect_backend_model_from_active_loop(workspace_root: &Path) -> Option<BackendModelInfo> {
    if lock_state(workspace_root).ok()? != LockState::Active {
        return None;
    }

    let lock_contents = std::fs::read_to_string(lock_path(workspace_root)).ok()?;
    let lock: serde_json::Value = serde_json::from_str(&lock_contents).ok()?;
    let pid = lock
        .get("pid")
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())?;

    let cmdline_path = PathBuf::from(format!("/proc/{pid}/cmdline"));
    let bytes = std::fs::read(cmdline_path).ok()?;
    if bytes.is_empty() {
        return None;
    }

    let args: Vec<String> = bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect();

    if args.is_empty() {
        return None;
    }

    let backend = extract_cli_flag_value(&args, "--backend", "-b");
    let model = extract_cli_flag_value(&args, "--model", "-m");

    if backend.is_none() && model.is_none() {
        return None;
    }

    Some(BackendModelInfo {
        backend,
        model,
        source: format!("runtime (pid {pid})"),
    })
}

fn detect_backend_model_from_config_files(workspace_root: &Path) -> Option<BackendModelInfo> {
    for path in candidate_config_paths(workspace_root) {
        if let Some(mut info) = parse_backend_model_from_yaml_file(&path) {
            let display_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("ralph config");
            info.source = format!("config ({display_name})");
            return Some(info);
        }
    }
    None
}

fn parse_backend_model_from_yaml_file(path: &Path) -> Option<BackendModelInfo> {
    let content = std::fs::read_to_string(path).ok()?;
    let config: serde_yaml::Value = serde_yaml::from_str(&content).ok()?;
    let cli = config.get("cli")?;

    let backend = cli
        .get("backend")
        .and_then(serde_yaml::Value::as_str)
        .map(str::to_string);

    let args: Vec<String> = cli
        .get("args")
        .and_then(serde_yaml::Value::as_sequence)
        .map(|seq| {
            seq.iter()
                .filter_map(serde_yaml::Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let model = extract_cli_flag_value(&args, "--model", "-m");

    if backend.is_none() && model.is_none() {
        return None;
    }

    Some(BackendModelInfo {
        backend,
        model,
        source: String::new(),
    })
}

fn candidate_config_paths(workspace_root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    for preferred in ["ralph.yml", "ralph.yaml"] {
        let path = workspace_root.join(preferred);
        if path.exists() {
            paths.push(path);
        }
    }

    let Ok(entries) = std::fs::read_dir(workspace_root) else {
        return paths;
    };

    let mut extras: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_ralph_config_file(path))
        .filter(|path| !paths.contains(path))
        .collect();
    extras.sort();

    paths.extend(extras);
    paths
}

fn is_ralph_config_file(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if !file_name.to_ascii_lowercase().starts_with("ralph") {
        return false;
    }

    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("yml") || ext.eq_ignore_ascii_case("yaml"))
}

fn extract_cli_flag_value(args: &[String], long_flag: &str, short_flag: &str) -> Option<String> {
    for (i, arg) in args.iter().enumerate() {
        if arg == long_flag || arg == short_flag {
            if let Some(value) = args.get(i + 1)
                && !value.starts_with('-')
            {
                return Some(value.clone());
            }
            continue;
        }

        if let Some(value) = arg.strip_prefix(&format!("{long_flag}="))
            && !value.is_empty()
        {
            return Some(value.to_string());
        }

        if let Some(value) = arg.strip_prefix(&format!("{short_flag}="))
            && !value.is_empty()
        {
            return Some(value.to_string());
        }
    }

    None
}

/// `!status` — Current iteration, hat, elapsed time, loop ID.
fn cmd_status(workspace_root: &Path) -> String {
    let state = match lock_state(workspace_root) {
        Ok(state) => state,
        Err(e) => {
            return format!("Failed to check lock state: {}", e);
        }
    };

    if state == LockState::Inactive {
        return "No active loop (no lock file found).".to_string();
    }

    if state == LockState::Stale {
        return "No active loop (stale lock file found).".to_string();
    }

    let lock_path = lock_path(workspace_root);
    let lock_content = match std::fs::read_to_string(&lock_path) {
        Ok(c) => c,
        Err(e) => return format!("Failed to read lock file: {}", e),
    };

    let lock: serde_json::Value = match serde_json::from_str(&lock_content) {
        Ok(v) => v,
        Err(e) => {
            return format!("Failed to parse lock file: {}", e);
        }
    };

    let pid = lock.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
    let started = lock
        .get("started")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let elapsed_str = if let Ok(started_dt) = chrono::DateTime::parse_from_rfc3339(started) {
        let elapsed = chrono::Utc::now().signed_duration_since(started_dt);
        let mins = elapsed.num_minutes();
        let secs = elapsed.num_seconds() % 60;
        if mins > 0 {
            format!("{}m {}s", mins, secs)
        } else {
            format!("{}s", secs)
        }
    } else {
        "unknown".to_string()
    };

    let iteration_count = count_iterations(workspace_root);

    let prompt_preview = lock
        .get("prompt")
        .and_then(|v| v.as_str())
        .map(|p| {
            let preview: String = p.chars().take(100).collect();
            if p.len() > 100 {
                format!("{}...", preview)
            } else {
                preview
            }
        })
        .unwrap_or_else(|| "none".to_string());

    let mut lines = vec![
        "**Loop Status**".to_string(),
        String::new(),
        format!("PID: `{}`", pid),
        format!("Elapsed: `{}`", elapsed_str),
        format!("Iterations: `{}`", iteration_count),
        format!("Started: `{}`", started),
    ];

    lines.push(String::new());
    lines.push(format!("Prompt: {}", prompt_preview));

    lines.join("\n")
}

/// Count iterations from the current events file.
fn count_iterations(workspace_root: &Path) -> usize {
    let pointer_path = workspace_root.join(".ralph/current-events");
    let events_path = if pointer_path.exists() {
        match std::fs::read_to_string(&pointer_path) {
            Ok(p) => workspace_root.join(p.trim()),
            Err(_) => return 0,
        }
    } else {
        workspace_root.join(".ralph/events.jsonl")
    };

    if !events_path.exists() {
        return 0;
    }

    let content = match std::fs::read_to_string(&events_path) {
        Ok(c) => c,
        Err(_) => return 0,
    };

    let mut max_iteration: usize = 0;
    for line in content.lines() {
        if let Ok(event) = serde_json::from_str::<serde_json::Value>(line)
            && let Some(iter) = event.get("iteration").and_then(|v| v.as_u64())
        {
            let iter = iter as usize;
            if iter > max_iteration {
                max_iteration = iter;
            }
        }
    }
    max_iteration
}

/// `!tasks` — List open tasks from `.ralph/agent/tasks.jsonl`.
fn cmd_tasks(workspace_root: &Path) -> String {
    let tasks_path = workspace_root.join(".ralph/agent/tasks.jsonl");

    if !tasks_path.exists() {
        return "No tasks file found.".to_string();
    }

    let content = match std::fs::read_to_string(&tasks_path) {
        Ok(c) => c,
        Err(e) => return format!("Failed to read tasks: {}", e),
    };

    let mut open_tasks: Vec<(String, String, u64)> = Vec::new();
    let mut closed_count = 0u32;

    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(task) = serde_json::from_str::<serde_json::Value>(line) {
            let status = task.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if status == "open" {
                let id = task
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string();
                let title = task
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("untitled")
                    .to_string();
                let priority = task.get("priority").and_then(|v| v.as_u64()).unwrap_or(3);
                open_tasks.push((id, title, priority));
            } else if status == "closed" {
                closed_count += 1;
            }
        }
    }

    open_tasks.sort_by_key(|t| t.2);

    if open_tasks.is_empty() {
        return format!("No open tasks. ({} completed)", closed_count);
    }

    let mut lines = vec![format!(
        "**Open Tasks** ({} open, {} closed)",
        open_tasks.len(),
        closed_count
    )];
    lines.push(String::new());

    for (id, title, priority) in &open_tasks {
        let priority_label = match priority {
            1 => "P1",
            2 => "P2",
            _ => "P3",
        };
        lines.push(format!("{}  `{}`\n    {}", priority_label, id, title));
    }

    lines.join("\n")
}

/// `!memories` — Last N memories from `.ralph/agent/memories.md`.
fn cmd_memories(workspace_root: &Path) -> String {
    let memories_path = workspace_root.join(".ralph/agent/memories.md");

    if !memories_path.exists() {
        return "No memories file found.".to_string();
    }

    let content = match std::fs::read_to_string(&memories_path) {
        Ok(c) => c,
        Err(e) => return format!("Failed to read memories: {}", e),
    };

    let mut memories: Vec<(String, String)> = Vec::new();

    let mut current_id = String::new();
    let mut current_content = String::new();

    for line in content.lines() {
        if let Some(id) = line.strip_prefix("### ") {
            if !current_id.is_empty() {
                memories.push((current_id.clone(), current_content.trim().to_string()));
            }
            current_id = id.trim().to_string();
            current_content.clear();
        } else if !current_id.is_empty() {
            current_content.push_str(line);
            current_content.push('\n');
        }
    }

    if !current_id.is_empty() {
        memories.push((current_id, current_content.trim().to_string()));
    }

    if memories.is_empty() {
        return "No memories found.".to_string();
    }

    let show_count = 5;
    let start = memories.len().saturating_sub(show_count);
    let shown = &memories[start..];

    let mut lines = vec![format!(
        "**Recent Memories** (showing {}/{})",
        shown.len(),
        memories.len()
    )];
    lines.push(String::new());

    for (id, content) in shown {
        let preview: String = content
            .lines()
            .filter(|l| l.starts_with('>'))
            .map(|l| {
                l.strip_prefix("> ")
                    .unwrap_or(l.strip_prefix('>').unwrap_or(l))
            })
            .collect::<Vec<_>>()
            .join(" ");

        let preview = truncate_with_ellipsis(&preview, 120);

        lines.push(format!("`{}`\n  {}", id, preview));
    }

    lines.join("\n")
}

/// `!restart` — Request a restart of the orchestration loop.
fn cmd_restart(workspace_root: &Path) -> String {
    let restart_path = workspace_root.join(".ralph/restart-requested");

    let state = match lock_state(workspace_root) {
        Ok(state) => state,
        Err(e) => {
            return format!("Failed to check lock state: {}", e);
        }
    };
    if state != LockState::Active {
        return "No active loop to restart.".to_string();
    }

    match std::fs::write(&restart_path, "") {
        Ok(()) => {
            "Restart requested. The loop will restart at the next iteration boundary.".to_string()
        }
        Err(e) => format!("Failed to write restart signal: {}", e),
    }
}

/// `!stop` — Request a stop of the orchestration loop.
fn cmd_stop(workspace_root: &Path) -> String {
    let stop_path = workspace_root.join(".ralph/stop-requested");

    let state = match lock_state(workspace_root) {
        Ok(state) => state,
        Err(e) => {
            return format!("Failed to check lock state: {}", e);
        }
    };
    if state != LockState::Active {
        return "No active loop to stop.".to_string();
    }

    match std::fs::write(&stop_path, "") {
        Ok(()) => "Stop requested. The loop will stop at the next iteration boundary.".to_string(),
        Err(e) => format!("Failed to write stop signal: {}", e),
    }
}

/// `!tail` — Last 20 lines of the current events file.
fn cmd_tail(workspace_root: &Path) -> String {
    let pointer_path = workspace_root.join(".ralph/current-events");
    let events_path = if pointer_path.exists() {
        match std::fs::read_to_string(&pointer_path) {
            Ok(p) => workspace_root.join(p.trim()),
            Err(e) => {
                return format!("Failed to read current-events pointer: {}", e);
            }
        }
    } else {
        workspace_root.join(".ralph/events.jsonl")
    };

    if !events_path.exists() {
        return "No events file found.".to_string();
    }

    let content = match std::fs::read_to_string(&events_path) {
        Ok(c) => c,
        Err(e) => return format!("Failed to read events: {}", e),
    };

    let all_lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();

    if all_lines.is_empty() {
        return "Events file is empty.".to_string();
    }

    let tail_count = 20;
    let start = all_lines.len().saturating_sub(tail_count);
    let tail = &all_lines[start..];

    let mut lines = vec![format!(
        "**Last {} Events** ({} total)",
        tail.len(),
        all_lines.len()
    )];
    lines.push(String::new());

    for event_line in tail {
        if let Ok(event) = serde_json::from_str::<serde_json::Value>(event_line) {
            let topic = event.get("topic").and_then(|v| v.as_str()).unwrap_or("?");
            let iteration = event
                .get("iteration")
                .and_then(|v| v.as_u64())
                .map(|i| format!("#{}", i))
                .unwrap_or_default();
            let hat = event.get("hat").and_then(|v| v.as_str()).unwrap_or("");
            let payload = event.get("payload").and_then(|v| v.as_str()).unwrap_or("");

            let payload_preview = truncate_with_ellipsis(payload, 60);

            let hat_str = if hat.is_empty() {
                String::new()
            } else {
                format!(" [{}]", hat)
            };

            lines.push(format!(
                "`{}{}` {}{}",
                topic,
                iteration,
                hat_str,
                if payload_preview.is_empty() {
                    String::new()
                } else {
                    format!("\n  {}", payload_preview)
                }
            ));
        } else {
            let preview = truncate_with_ellipsis(event_line, 80);
            lines.push(preview);
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn setup_workspace(dir: &TempDir) {
        std::fs::create_dir_all(dir.path().join(".ralph/agent")).unwrap();
    }

    #[test]
    fn is_command_recognizes_bang() {
        assert!(is_command("!help"));
        assert!(is_command("!status"));
        assert!(!is_command("hello"));
        assert!(!is_command("use !help"));
        assert!(!is_command("/help"));
    }

    #[test]
    fn parse_command_simple() {
        assert_eq!(parse_command("!help"), ("!help", ""));
        assert_eq!(parse_command("!status"), ("!status", ""));
    }

    #[test]
    fn parse_command_with_args() {
        assert_eq!(parse_command("!memories 10"), ("!memories", "10"));
        assert_eq!(
            parse_command("!model claude-sonnet-4"),
            ("!model", "claude-sonnet-4")
        );
    }

    #[test]
    fn handle_command_returns_none_for_unknown() {
        let dir = TempDir::new().unwrap();
        assert!(handle_command("!unknown", dir.path()).is_none());
    }

    #[test]
    fn handle_command_returns_some_for_known() {
        let dir = TempDir::new().unwrap();
        assert!(handle_command("!help", dir.path()).is_some());
    }

    #[test]
    fn cmd_help_lists_commands() {
        let result = cmd_help();
        assert!(result.contains("!status"));
        assert!(result.contains("!tasks"));
        assert!(result.contains("!memories"));
        assert!(result.contains("!tail"));
        assert!(result.contains("!help"));
        assert!(result.contains("!model"));
        assert!(result.contains("!models"));
        assert!(result.contains("!restart"));
        assert!(result.contains("!stop"));
    }

    #[test]
    fn cmd_help_uses_markdown_not_html() {
        let result = cmd_help();
        assert!(result.contains("**Ralph Bot Commands**"));
        assert!(!result.contains("<b>"));
        assert!(!result.contains("</b>"));
    }

    #[test]
    fn cmd_status_no_lock_file() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);
        let result = cmd_status(dir.path());
        assert!(result.contains("No active loop"));
    }

    #[test]
    fn cmd_status_with_stale_lock_file() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);

        let lock = serde_json::json!({
            "pid": 12345,
            "started": "2026-01-30T10:00:00Z",
            "prompt": "Build a feature"
        });
        let lock_path = dir.path().join(".ralph/loop.lock");
        std::fs::write(&lock_path, serde_json::to_string(&lock).unwrap()).unwrap();

        let result = cmd_status(dir.path());
        assert!(result.contains("No active loop"));
        assert!(result.contains("stale lock"));
    }

    #[cfg(unix)]
    #[test]
    fn cmd_status_with_active_lock_file() {
        use nix::fcntl::{Flock, FlockArg};

        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);

        let lock = serde_json::json!({
            "pid": 12345,
            "started": "2026-01-30T10:00:00Z",
            "prompt": "Build a feature"
        });
        let lock_path = dir.path().join(".ralph/loop.lock");
        std::fs::write(&lock_path, serde_json::to_string(&lock).unwrap()).unwrap();

        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        let _flock = Flock::lock(file, FlockArg::LockExclusiveNonblock).unwrap();

        let result = cmd_status(dir.path());
        assert!(result.contains("12345"));
        assert!(result.contains("Build a feature"));
    }

    #[cfg(unix)]
    #[test]
    fn cmd_status_uses_markdown_formatting() {
        use nix::fcntl::{Flock, FlockArg};

        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);

        let lock = serde_json::json!({
            "pid": 12345,
            "started": "2026-01-30T10:00:00Z",
            "prompt": "Build a feature"
        });
        let lock_path = dir.path().join(".ralph/loop.lock");
        std::fs::write(&lock_path, serde_json::to_string(&lock).unwrap()).unwrap();

        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        let _flock = Flock::lock(file, FlockArg::LockExclusiveNonblock).unwrap();

        let result = cmd_status(dir.path());
        assert!(result.contains("**Loop Status**"));
        assert!(result.contains("PID: `12345`"));
        assert!(!result.contains("<b>"));
        assert!(!result.contains("<code>"));
    }

    #[test]
    fn cmd_tasks_no_file() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);
        let result = cmd_tasks(dir.path());
        assert!(result.contains("No tasks file"));
    }

    #[test]
    fn cmd_tasks_with_open_and_closed() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);

        let tasks_path = dir.path().join(".ralph/agent/tasks.jsonl");
        let mut f = std::fs::File::create(&tasks_path).unwrap();
        writeln!(
            f,
            r#"{{"id":"task-1","title":"Add auth","status":"open","priority":1}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"id":"task-2","title":"Fix bug","status":"closed","priority":2}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"id":"task-3","title":"Add tests","status":"open","priority":2}}"#
        )
        .unwrap();

        let result = cmd_tasks(dir.path());
        assert!(result.contains("2 open"));
        assert!(result.contains("1 closed"));
        assert!(result.contains("Add auth"));
        assert!(result.contains("Add tests"));
        assert!(!result.contains("Fix bug"));
    }

    #[test]
    fn cmd_tasks_uses_markdown_formatting() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);

        let tasks_path = dir.path().join(".ralph/agent/tasks.jsonl");
        let mut f = std::fs::File::create(&tasks_path).unwrap();
        writeln!(
            f,
            r#"{{"id":"task-1","title":"Add auth","status":"open","priority":1}}"#
        )
        .unwrap();

        let result = cmd_tasks(dir.path());
        assert!(result.contains("**Open Tasks**"));
        assert!(result.contains("`task-1`"));
        assert!(!result.contains("<b>"));
        assert!(!result.contains("<code>"));
    }

    #[test]
    fn cmd_tasks_all_closed() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);

        let tasks_path = dir.path().join(".ralph/agent/tasks.jsonl");
        let mut f = std::fs::File::create(&tasks_path).unwrap();
        writeln!(
            f,
            r#"{{"id":"task-1","title":"Done","status":"closed","priority":1}}"#
        )
        .unwrap();

        let result = cmd_tasks(dir.path());
        assert!(result.contains("No open tasks"));
        assert!(result.contains("1 completed"));
    }

    #[test]
    fn cmd_memories_no_file() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);
        let result = cmd_memories(dir.path());
        assert!(result.contains("No memories file"));
    }

    #[test]
    fn cmd_memories_with_entries() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);

        let mem_path = dir.path().join(".ralph/agent/memories.md");
        std::fs::write(
            &mem_path,
            "# Memories\n\n## Patterns\n\n### mem-001\n> First memory\n<!-- tags: test -->\n\n### mem-002\n> Second memory\n<!-- tags: test -->\n",
        )
        .unwrap();

        let result = cmd_memories(dir.path());
        assert!(result.contains("mem-001"));
        assert!(result.contains("mem-002"));
        assert!(result.contains("First memory"));
        assert!(result.contains("Second memory"));
    }

    #[test]
    fn cmd_memories_uses_markdown_formatting() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);

        let mem_path = dir.path().join(".ralph/agent/memories.md");
        std::fs::write(
            &mem_path,
            "# Memories\n\n## Patterns\n\n### mem-001\n> First memory\n<!-- tags: test -->\n",
        )
        .unwrap();

        let result = cmd_memories(dir.path());
        assert!(result.contains("**Recent Memories**"));
        assert!(result.contains("`mem-001`"));
        assert!(!result.contains("<b>"));
        assert!(!result.contains("<code>"));
    }

    #[test]
    fn cmd_tail_no_events() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);
        let result = cmd_tail(dir.path());
        assert!(result.contains("No events file"));
    }

    #[test]
    fn cmd_tail_with_events() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);

        let events_path = dir.path().join(".ralph/events.jsonl");
        let mut f = std::fs::File::create(&events_path).unwrap();
        for i in 0..5 {
            writeln!(
                f,
                r#"{{"topic":"work.start","iteration":{},"hat":"executor","payload":"task {}","ts":"2026-01-30T10:00:00Z"}}"#,
                i, i
            )
            .unwrap();
        }

        let result = cmd_tail(dir.path());
        assert!(result.contains("Last 5 Events"));
        assert!(result.contains("5 total"));
        assert!(result.contains("work.start"));
    }

    #[test]
    fn cmd_tail_uses_markdown_formatting() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);

        let events_path = dir.path().join(".ralph/events.jsonl");
        let mut f = std::fs::File::create(&events_path).unwrap();
        writeln!(
            f,
            r#"{{"topic":"work.start","iteration":1,"hat":"executor","payload":"task 1","ts":"2026-01-30T10:00:00Z"}}"#
        )
        .unwrap();

        let result = cmd_tail(dir.path());
        assert!(result.contains("**Last 1 Events**"));
        assert!(result.contains("`work.start#1`"));
        assert!(!result.contains("<b>"));
        assert!(!result.contains("<code>"));
    }

    #[test]
    fn cmd_tail_with_current_events_pointer() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);

        let events_file = ".ralph/events-20260130-100000.jsonl";
        let events_path = dir.path().join(events_file);
        let mut f = std::fs::File::create(&events_path).unwrap();
        writeln!(
            f,
            r#"{{"topic":"plan.start","iteration":1,"payload":"planning","ts":"2026-01-30T10:00:00Z"}}"#
        )
        .unwrap();

        let pointer_path = dir.path().join(".ralph/current-events");
        std::fs::write(&pointer_path, events_file).unwrap();

        let result = cmd_tail(dir.path());
        assert!(result.contains("plan.start"));
    }

    #[test]
    fn cmd_tail_truncates_long_payloads() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);

        let events_path = dir.path().join(".ralph/events.jsonl");
        let mut f = std::fs::File::create(&events_path).unwrap();
        let long_payload = "a".repeat(200);
        writeln!(
            f,
            r#"{{"topic":"work.done","iteration":1,"payload":"{}","ts":"2026-01-30T10:00:00Z"}}"#,
            long_payload
        )
        .unwrap();

        let result = cmd_tail(dir.path());
        assert!(result.contains("..."));
        assert!(result.len() < 300);
    }

    #[test]
    fn cmd_restart_no_active_loop() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);
        let result = cmd_restart(dir.path());
        assert!(result.contains("No active loop"));
    }

    #[test]
    fn cmd_stop_no_active_loop() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);
        let result = cmd_stop(dir.path());
        assert!(result.contains("No active loop"));
    }

    #[cfg(unix)]
    #[test]
    fn cmd_restart_writes_signal_file() {
        use nix::fcntl::{Flock, FlockArg};

        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);

        let lock = serde_json::json!({
            "pid": 12345,
            "started": "2026-01-30T10:00:00Z",
            "prompt": "Test prompt"
        });
        let lock_path = dir.path().join(".ralph/loop.lock");
        std::fs::write(&lock_path, serde_json::to_string(&lock).unwrap()).unwrap();

        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        let _flock = Flock::lock(file, FlockArg::LockExclusiveNonblock).unwrap();

        let result = cmd_restart(dir.path());
        assert!(result.contains("Restart requested"));

        let restart_path = dir.path().join(".ralph/restart-requested");
        assert!(restart_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn cmd_stop_writes_signal_file() {
        use nix::fcntl::{Flock, FlockArg};

        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);

        let lock = serde_json::json!({
            "pid": 12345,
            "started": "2026-01-30T10:00:00Z",
            "prompt": "Test prompt"
        });
        let lock_path = dir.path().join(".ralph/loop.lock");
        std::fs::write(&lock_path, serde_json::to_string(&lock).unwrap()).unwrap();

        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        let _flock = Flock::lock(file, FlockArg::LockExclusiveNonblock).unwrap();

        let result = cmd_stop(dir.path());
        assert!(result.contains("Stop requested"));

        let stop_path = dir.path().join(".ralph/stop-requested");
        assert!(stop_path.exists());
    }

    #[test]
    fn handle_command_recognizes_restart() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);
        assert!(handle_command("!restart", dir.path()).is_some());
    }

    #[test]
    fn handle_command_recognizes_stop() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);
        assert!(handle_command("!stop", dir.path()).is_some());
    }

    #[test]
    fn cmd_help_lists_restart() {
        let result = cmd_help();
        assert!(result.contains("!restart"));
    }

    #[test]
    fn cmd_help_lists_stop() {
        let result = cmd_help();
        assert!(result.contains("!stop"));
    }

    #[test]
    fn handle_command_recognizes_model() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);
        assert!(handle_command("!model", dir.path()).is_some());
    }

    #[test]
    fn handle_command_recognizes_models() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);
        assert!(handle_command("!models", dir.path()).is_some());
    }

    #[test]
    fn cmd_help_lists_model_commands() {
        let result = cmd_help();
        assert!(result.contains("!model"));
        assert!(result.contains("!models"));
    }

    #[test]
    fn cmd_model_reads_backend_and_model_from_config() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);

        std::fs::write(
            dir.path().join("ralph.yml"),
            r"cli:
  backend: pi
  args:
    - --model
    - gpt-5.3-codex
",
        )
        .unwrap();

        let result = cmd_model(dir.path(), "");
        assert!(result.contains("pi"));
        assert!(result.contains("gpt-5.3-codex"));
        assert!(result.contains("config (ralph.yml)"));
    }

    #[test]
    fn cmd_model_uses_markdown_formatting() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);

        std::fs::write(
            dir.path().join("ralph.yml"),
            r"cli:
  backend: pi
  args:
    - --model
    - gpt-5.3-codex
",
        )
        .unwrap();

        let result = cmd_model(dir.path(), "");
        assert!(result.contains("**Model**"));
        assert!(result.contains("`pi`"));
        assert!(result.contains("`gpt-5.3-codex`"));
        assert!(!result.contains("<b>"));
        assert!(!result.contains("<code>"));
    }

    #[test]
    fn cmd_model_with_args_is_read_only() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);

        let result = cmd_model(dir.path(), "claude-sonnet-4");
        assert!(result.contains("read-only"));
        assert!(result.contains("claude-sonnet-4"));
    }

    #[test]
    fn cmd_model_with_args_uses_markdown() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);

        let result = cmd_model(dir.path(), "claude-sonnet-4");
        assert!(result.contains("`claude-sonnet-4`"));
        assert!(result.contains("`ralph run -b pi -- --model gpt-5.3-codex`"));
        assert!(!result.contains("<code>"));
    }

    #[test]
    fn cmd_models_lists_all_models_from_ralph_configs() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);

        std::fs::write(
            dir.path().join("ralph.yml"),
            r"cli:
  backend: pi
  args:
    - --model
    - gpt-5.3-codex
",
        )
        .unwrap();

        std::fs::write(
            dir.path().join("ralph.bot.yml"),
            r"cli:
  backend: custom
  args:
    - --model=claude-sonnet-4
",
        )
        .unwrap();

        let result = cmd_models(dir.path());
        assert!(result.contains("gpt-5.3-codex"));
        assert!(result.contains("claude-sonnet-4"));
        assert!(result.contains("Found 2 models"));
    }

    #[test]
    fn extract_cli_flag_value_supports_equals_and_split_forms() {
        let args = vec![
            "--model".to_string(),
            "split-value".to_string(),
            "--other".to_string(),
        ];
        assert_eq!(
            extract_cli_flag_value(&args, "--model", "-m"),
            Some("split-value".to_string())
        );

        let args = vec!["--model=equals-value".to_string()];
        assert_eq!(
            extract_cli_flag_value(&args, "--model", "-m"),
            Some("equals-value".to_string())
        );

        let args = vec!["-m=short-equals".to_string()];
        assert_eq!(
            extract_cli_flag_value(&args, "--model", "-m"),
            Some("short-equals".to_string())
        );
    }

    #[test]
    fn no_html_in_any_command_output() {
        let dir = TempDir::new().unwrap();
        setup_workspace(&dir);

        let commands = ["!help", "!status", "!tasks", "!memories", "!tail"];
        for cmd in commands {
            let result = handle_command(cmd, dir.path()).unwrap();
            assert!(
                !result.contains("<b>") && !result.contains("<code>") && !result.contains("<pre>"),
                "Command {} produced HTML output: {}",
                cmd,
                result
            );
        }
    }
}
