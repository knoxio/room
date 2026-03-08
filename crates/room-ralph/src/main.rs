use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use clap::{CommandFactory, FromArgMatches};
use room_ralph::{loop_runner, room, Cli};

fn check_dependencies() -> Result<(), String> {
    let mut missing = Vec::new();
    for cmd in &["claude", "room"] {
        if std::process::Command::new(cmd)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_err()
        {
            missing.push(*cmd);
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("missing dependencies: {}", missing.join(", ")))
    }
}

fn launch_tmux(cli: &Cli) -> Result<(), String> {
    let session_name = format!("ralph-{}", cli.username);

    let exists = std::process::Command::new("tmux")
        .args(["has-session", "-t", &session_name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if exists {
        tracing::info!("tmux session {} already exists — attaching", session_name);
        let status = std::process::Command::new("tmux")
            .args(["attach-session", "-t", &session_name])
            .status()
            .map_err(|e| format!("tmux attach failed: {e}"))?;
        std::process::exit(status.code().unwrap_or(1));
    }

    let exe = std::env::current_exe().map_err(|e| format!("cannot find own path: {e}"))?;
    let mut args = vec![
        cli.room_id.clone(),
        cli.username.clone(),
        "--model".into(),
        cli.model.clone(),
        "--max-iter".into(),
        cli.max_iter.to_string(),
        "--cooldown".into(),
        cli.cooldown.to_string(),
    ];
    if let Some(issue) = &cli.issue {
        args.push("--issue".into());
        args.push(issue.clone());
    }
    if let Some(prompt) = &cli.prompt {
        args.push("--prompt".into());
        args.push(prompt.display().to_string());
    }
    for d in &cli.add_dirs {
        args.push("--add-dir".into());
        args.push(d.display().to_string());
    }

    let cmd_str = format!("{} {}", exe.display(), args.join(" "));
    std::process::Command::new("tmux")
        .args(["new-session", "-d", "-s", &session_name, &cmd_str])
        .status()
        .map_err(|e| format!("tmux new-session failed: {e}"))?;

    tracing::info!("started tmux session: {}", session_name);
    tracing::info!("attach with: tmux attach -t {}", session_name);
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::from_arg_matches(
        &Cli::command()
            .disable_version_flag(true)
            .arg(
                clap::Arg::new("version")
                    .short('v')
                    .short_alias('V')
                    .long("version")
                    .action(clap::ArgAction::Version)
                    .help("Print version"),
            )
            .get_matches(),
    )
    .expect("failed to parse CLI arguments");

    // Set up logging — file + stderr
    let log_file = room::log_file_path(&cli.username);
    let file_appender = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(false)
        .with_writer(move || {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_file)
                .unwrap_or_else(|_| std::fs::File::create("/dev/null").unwrap())
        });
    let stderr_layer = tracing_subscriber::fmt::layer().with_target(false);

    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    tracing_subscriber::registry()
        .with(stderr_layer)
        .with(file_appender)
        .init();

    if let Err(e) = check_dependencies() {
        tracing::error!("{}", e);
        return ExitCode::FAILURE;
    }

    if cli.tmux {
        match launch_tmux(&cli) {
            Ok(()) => return ExitCode::SUCCESS,
            Err(e) => {
                tracing::error!("{}", e);
                return ExitCode::FAILURE;
            }
        }
    }

    // Signal handling
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("caught SIGINT, shutting down");
        r.store(false, Ordering::SeqCst);
    });

    #[cfg(unix)]
    {
        let r = running.clone();
        tokio::spawn(async move {
            let mut sig = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to register SIGTERM handler");
            sig.recv().await;
            tracing::info!("caught SIGTERM, shutting down");
            r.store(false, Ordering::SeqCst);
        });
    }

    tracing::info!(
        "room-ralph starting: room={} user={} model={} issue={} max_iter={}",
        cli.room_id,
        cli.username,
        cli.model,
        cli.issue.as_deref().unwrap_or("none"),
        cli.max_iter,
    );

    let token = match room::join_room(&cli.room_id, &cli.username) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("failed to join room: {}", e);
            return ExitCode::FAILURE;
        }
    };

    let announce = format!(
        "online (room-ralph, model={}, iter limit={})",
        cli.model, cli.max_iter
    );
    room::send_message(&cli.room_id, &token, &announce).ok();

    match loop_runner::run_loop(&cli, token, &running).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("loop failed: {}", e);
            ExitCode::FAILURE
        }
    }
}
