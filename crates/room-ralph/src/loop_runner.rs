use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::claude;
use crate::monitor;
use crate::progress;
use crate::prompt::{self, PromptConfig};
use crate::room;
use crate::Cli;

/// Run the main ralph loop: iterate, build prompt, call claude, handle output.
pub async fn run_loop(cli: &Cli, token: &str, running: &Arc<AtomicBool>) -> Result<(), String> {
    let progress_file = progress::progress_file_path(cli.issue.as_deref(), &cli.username);
    let mut iteration: u32 = 0;

    while running.load(Ordering::SeqCst) {
        iteration += 1;

        if cli.max_iter > 0 && iteration > cli.max_iter {
            tracing::info!("max iterations ({}) reached, stopping", cli.max_iter);
            room::send_message(
                &cli.room_id,
                token,
                &format!("max iterations reached ({}), shutting down", cli.max_iter),
            )
            .ok();
            break;
        }

        tracing::info!("--- iteration {} ---", iteration);

        // Poll room for recent messages
        let messages = room::poll_messages(&cli.room_id, token).unwrap_or_default();

        // Build prompt
        let config = PromptConfig {
            room_id: &cli.room_id,
            username: &cli.username,
            token,
            custom_prompt_file: cli.prompt.as_deref(),
            personality_file: cli.personality.as_deref(),
            progress_file: &progress_file,
            issue: cli.issue.as_deref(),
        };
        let prompt_text = prompt::build_prompt(&config, &messages);

        // Dry run — print and exit
        if cli.dry_run {
            println!("=== DRY RUN: prompt ===");
            println!("{prompt_text}");
            return Ok(());
        }

        // Write prompt to temp file
        let prompt_file =
            std::path::PathBuf::from(format!("/tmp/ralph-room-prompt-{}.txt", cli.username));
        std::fs::write(&prompt_file, &prompt_text)
            .map_err(|e| format!("failed to write prompt file: {e}"))?;

        // Run claude
        tracing::info!(
            "running claude -p (model={}, iteration={})",
            cli.model,
            iteration
        );
        let effective_tools = claude::resolve_allowed_tools(&cli.allow_tools);
        let effective_disallowed = claude::resolve_disallowed_tools(&cli.disallow_tools);
        let claude_output = match claude::spawn_claude(
            &cli.model,
            &prompt_file,
            &cli.add_dirs,
            &effective_tools,
            &effective_disallowed,
        ) {
            Ok(o) => o,
            Err(e) => {
                tracing::error!("failed to spawn claude: {}", e);
                cooldown(cli.cooldown, running).await;
                continue;
            }
        };

        tracing::info!("claude exited with code {}", claude_output.exit_code);

        // Extract response text
        let response = claude::extract_response(&claude_output.raw_json);

        // Context monitoring — parse token usage
        let input_tokens = monitor::parse_usage(&claude_output.raw_json);
        let output_tokens = monitor::parse_output_tokens(&claude_output.raw_json);
        tracing::info!(
            "{}",
            monitor::format_usage_summary(input_tokens, output_tokens)
        );
        monitor::log_usage(&progress_file, input_tokens, output_tokens, iteration).ok();

        // Detect context exhaustion — two paths:
        // 1. Proactive: token usage exceeds threshold
        // 2. Reactive: claude crashed with context-related error message
        let should_cycle = if monitor::should_restart(input_tokens) {
            tracing::info!(
                "proactive restart: token usage ({}) exceeds threshold",
                input_tokens
            );
            true
        } else if claude::detect_context_exhaustion(claude_output.exit_code, &response) {
            tracing::info!("reactive restart: context exhaustion detected in output");
            true
        } else {
            false
        };

        if should_cycle {
            progress::write_progress(&progress_file, iteration, cli.issue.as_deref(), &response)
                .map_err(|e| format!("failed to write progress: {e}"))?;

            let msg = format!(
                "context limit at iteration {} (tokens: {}), restarting with fresh context",
                iteration, input_tokens
            );
            room::send_message(&cli.room_id, token, &msg).ok();
        } else if claude_output.exit_code != 0 {
            tracing::warn!(
                "claude failed (exit {}), will retry after cooldown",
                claude_output.exit_code
            );
            let msg = format!(
                "claude exited with error (code {}), retrying in {}s",
                claude_output.exit_code, cli.cooldown
            );
            room::send_message(&cli.room_id, token, &msg).ok();
        }

        // Re-join if token expired
        if room::detect_token_expiry(&response) {
            tracing::warn!("token appears invalid, re-joining");
            // For now, we cannot update the token in-place since it's borrowed.
            // In the shell version this mutated a global. Here we log and continue.
            // A future improvement: pass token as &mut or use a shared state.
            tracing::error!("token re-join not yet implemented in Rust version");
        }

        // Cooldown
        cooldown(cli.cooldown, running).await;
    }

    tracing::info!("room-ralph stopped after {} iterations", iteration);
    room::send_message(
        &cli.room_id,
        token,
        &format!("offline (room-ralph stopped after {iteration} iterations)"),
    )
    .ok();
    Ok(())
}

/// Sleep for the cooldown period, but wake early if running is set to false.
async fn cooldown(seconds: u64, running: &Arc<AtomicBool>) {
    if !running.load(Ordering::SeqCst) {
        return;
    }
    tracing::debug!("cooldown {}s", seconds);
    tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
}
