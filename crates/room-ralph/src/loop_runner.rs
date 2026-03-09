use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::claude::{self, ClaudeOutput};
use crate::monitor;
use crate::progress;
use crate::prompt::{self, PromptConfig};
use crate::room;
use crate::Cli;

/// Run the main ralph loop: iterate, build prompt, call claude, handle output.
///
/// Takes ownership of `token` so it can be updated in-place if the broker
/// restarts and a re-join is needed.
pub async fn run_loop(cli: &Cli, token: String, running: &Arc<AtomicBool>) -> Result<(), String> {
    let progress_file = progress::progress_file_path(cli.issue.as_deref(), &cli.username);
    let mut iteration: u32 = 0;
    let mut token = token;
    let socket_str = cli.socket.as_ref().map(|p| p.display().to_string());
    let socket_ref = socket_str.as_deref();

    while running.load(Ordering::SeqCst) {
        iteration += 1;

        if at_max_iter(cli, iteration, &token, socket_ref) {
            break;
        }

        tracing::info!("--- iteration {} ---", iteration);

        let messages = poll_with_token_refresh(cli, &mut token, socket_ref);
        let prompt_text = build_iteration_prompt(cli, &messages, &progress_file, &token);

        if cli.dry_run {
            println!("=== DRY RUN: prompt ===\n{prompt_text}");
            return Ok(());
        }

        let prompt_file = write_prompt_file(&cli.username, &prompt_text)?;

        let Some(output) = try_invoke_claude(cli, &token, socket_ref, iteration, &prompt_file)
        else {
            cooldown(cli.cooldown, running).await;
            continue;
        };

        process_output(cli, &token, socket_ref, iteration, &output, &progress_file)?;
        cooldown(cli.cooldown, running).await;
    }

    shutdown(cli, &token, socket_ref, iteration);
    Ok(())
}

/// Returns `true` and sends a shutdown message if `iteration` has exceeded
/// `cli.max_iter` (when max_iter > 0).
fn at_max_iter(cli: &Cli, iteration: u32, token: &str, socket_ref: Option<&str>) -> bool {
    if cli.max_iter > 0 && iteration > cli.max_iter {
        tracing::info!("max iterations ({}) reached, stopping", cli.max_iter);
        room::send_message(
            &cli.room_id,
            token,
            &format!("max iterations reached ({}), shutting down", cli.max_iter),
            socket_ref,
        )
        .ok();
        return true;
    }
    false
}

/// Poll room for recent messages, transparently re-joining if the token has
/// expired due to a broker restart.
fn poll_with_token_refresh(
    cli: &Cli,
    token: &mut String,
    socket_ref: Option<&str>,
) -> Vec<room_protocol::Message> {
    match room::poll_messages(&cli.room_id, token, socket_ref) {
        Ok(msgs) => msgs,
        Err(e) if room::detect_token_expiry(&e) => {
            tracing::warn!("token expired during poll, re-joining: {}", e);
            rejoin_and_poll(cli, token, socket_ref)
        }
        Err(_) => Vec::new(),
    }
}

/// Re-join the room to obtain a fresh token, then poll for messages.
fn rejoin_and_poll(
    cli: &Cli,
    token: &mut String,
    socket_ref: Option<&str>,
) -> Vec<room_protocol::Message> {
    match room::join_room(&cli.room_id, &cli.username, socket_ref) {
        Ok(result) => {
            tracing::info!("re-joined as '{}' with new token", result.username);
            *token = result.token;
            room::poll_messages(&cli.room_id, token, socket_ref).unwrap_or_default()
        }
        Err(join_err) => {
            tracing::error!("re-join failed: {}", join_err);
            Vec::new()
        }
    }
}

/// Build the prompt text for this iteration.
fn build_iteration_prompt(
    cli: &Cli,
    messages: &[room_protocol::Message],
    progress_file: &Path,
    token: &str,
) -> String {
    let config = PromptConfig {
        room_id: &cli.room_id,
        username: &cli.username,
        token,
        custom_prompt_file: cli.prompt.as_deref(),
        personality_file: cli.personality.as_deref(),
        progress_file,
        issue: cli.issue.as_deref(),
    };
    prompt::build_prompt(&config, messages)
}

/// Write the prompt to a temp file and return its path.
fn write_prompt_file(username: &str, prompt_text: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(format!("/tmp/ralph-room-prompt-{username}.txt"));
    std::fs::write(&path, prompt_text).map_err(|e| format!("failed to write prompt file: {e}"))?;
    Ok(path)
}

/// Set running-status, invoke claude, and return its output. Returns `None`
/// if spawning fails (the caller should continue to the next iteration).
fn try_invoke_claude(
    cli: &Cli,
    token: &str,
    socket_ref: Option<&str>,
    iteration: u32,
    prompt_file: &Path,
) -> Option<ClaudeOutput> {
    let status_text = match cli.issue.as_deref() {
        Some(issue) => format!("running claude — iteration {iteration} for #{issue}"),
        None => format!("running claude — iteration {iteration}"),
    };
    room::set_status(&cli.room_id, token, &status_text, socket_ref).ok();

    tracing::info!(
        "running claude -p (model={}, iteration={})",
        cli.model,
        iteration
    );
    let (profile_allow, profile_disallow) =
        claude::merge_profile_with_overrides(cli.profile, &cli.allow_tools, &cli.disallow_tools);
    let effective_tools = claude::resolve_allowed_tools(&profile_allow);
    let effective_disallowed = claude::resolve_disallowed_tools(&profile_disallow);

    match claude::spawn_claude(
        &cli.model,
        prompt_file,
        &cli.add_dirs,
        &effective_tools,
        &effective_disallowed,
    ) {
        Ok(output) => Some(output),
        Err(e) => {
            tracing::error!("failed to spawn claude: {}", e);
            None
        }
    }
}

/// Process claude's output: log usage, detect context exhaustion, send status
/// updates to the room.
fn process_output(
    cli: &Cli,
    token: &str,
    socket_ref: Option<&str>,
    iteration: u32,
    output: &ClaudeOutput,
    progress_file: &Path,
) -> Result<(), String> {
    tracing::info!("claude exited with code {}", output.exit_code);

    let response = claude::extract_response(&output.raw_json);
    let input_tokens = monitor::parse_usage(&output.raw_json);
    let output_tokens = monitor::parse_output_tokens(&output.raw_json);
    tracing::info!(
        "{}",
        monitor::format_usage_summary(input_tokens, output_tokens)
    );
    monitor::log_usage(progress_file, input_tokens, output_tokens, iteration).ok();

    let should_cycle = monitor::should_restart(input_tokens)
        || claude::detect_context_exhaustion(output.exit_code, &response);

    if should_cycle {
        on_context_cycle(
            cli,
            token,
            socket_ref,
            iteration,
            input_tokens,
            &response,
            progress_file,
        )
    } else if output.exit_code != 0 {
        on_claude_error(cli, token, socket_ref, iteration, output.exit_code);
        Ok(())
    } else {
        Ok(())
    }
}

/// Write progress and broadcast a context-cycle notification.
fn on_context_cycle(
    cli: &Cli,
    token: &str,
    socket_ref: Option<&str>,
    iteration: u32,
    input_tokens: u64,
    response: &str,
    progress_file: &Path,
) -> Result<(), String> {
    progress::write_progress(progress_file, iteration, cli.issue.as_deref(), response)
        .map_err(|e| format!("failed to write progress: {e}"))?;
    room::set_status(
        &cli.room_id,
        token,
        &format!("restarting — context limit at iteration {iteration}"),
        socket_ref,
    )
    .ok();
    room::send_message(
        &cli.room_id,
        token,
        &format!(
            "context limit at iteration {} (tokens: {}), restarting with fresh context",
            iteration, input_tokens
        ),
        socket_ref,
    )
    .ok();
    Ok(())
}

/// Broadcast a claude error notification when exit_code != 0.
fn on_claude_error(
    cli: &Cli,
    token: &str,
    socket_ref: Option<&str>,
    iteration: u32,
    exit_code: i32,
) {
    tracing::warn!(
        "claude failed (exit {}), will retry after cooldown",
        exit_code
    );
    room::set_status(
        &cli.room_id,
        token,
        &format!("retrying — claude error (code {exit_code}) at iteration {iteration}"),
        socket_ref,
    )
    .ok();
    room::send_message(
        &cli.room_id,
        token,
        &format!(
            "claude exited with error (code {exit_code}), retrying in {}s",
            cli.cooldown
        ),
        socket_ref,
    )
    .ok();
}

/// Broadcast offline status and final message after the loop exits.
fn shutdown(cli: &Cli, token: &str, socket_ref: Option<&str>, iteration: u32) {
    tracing::info!("room-ralph stopped after {} iterations", iteration);
    room::set_status(&cli.room_id, token, "offline", socket_ref).ok();
    room::send_message(
        &cli.room_id,
        token,
        &format!("offline (room-ralph stopped after {iteration} iterations)"),
        socket_ref,
    )
    .ok();
}

/// Sleep for the cooldown period, but wake early if running is set to false.
async fn cooldown(seconds: u64, running: &Arc<AtomicBool>) {
    if !running.load(Ordering::SeqCst) {
        return;
    }
    tracing::debug!("cooldown {}s", seconds);
    tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
}
