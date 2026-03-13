use std::path::PathBuf;

use room_cli::broker::daemon::{is_pid_alive, DaemonConfig, DaemonState};
use room_cli::paths;

pub async fn run_daemon(
    socket: PathBuf,
    data_dir: PathBuf,
    state_dir: PathBuf,
    ws_port: Option<u16>,
    rooms: Vec<String>,
    grace_period_secs: u64,
) -> anyhow::Result<()> {
    paths::ensure_room_dirs().map_err(|e| anyhow::anyhow!("cannot create ~/.room dirs: {e}"))?;

    // Stale PID check: guard against starting a second daemon over the system
    // daemon.  Only applies when using the default socket path — daemons with
    // explicit socket overrides (tests, CI, non-system instances) are independent
    // and must not interfere with the system PID file.
    //
    // We also skip the check when the PID file already holds our own PID.  That
    // happens when `ensure_daemon_running` wrote our PID before we started (the
    // auto-start path) — in that case there is no conflict.
    if socket == paths::room_socket_path() {
        let pid_path = paths::room_pid_path();
        if pid_path.exists() {
            let file_pid = std::fs::read_to_string(&pid_path)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok());
            let is_own = file_pid == Some(std::process::id());
            if !is_own {
                if is_pid_alive(&pid_path) {
                    anyhow::bail!(
                        "daemon already running (PID file: {}). \
                         Stop the existing daemon or remove the PID file manually.",
                        pid_path.display()
                    );
                }
                eprintln!(
                    "[daemon] stale PID file at {} (process gone), cleaning up",
                    pid_path.display()
                );
                let _ = std::fs::remove_file(&pid_path);
            }
        }
    }

    let config = DaemonConfig {
        socket_path: socket,
        data_dir,
        state_dir,
        ws_port,
        grace_period_secs,
    };

    let daemon = DaemonState::new(config);

    // Create initial rooms.
    for room_id in &rooms {
        match daemon.create_room(room_id).await {
            Ok(_) => eprintln!("[daemon] created room: {room_id}"),
            Err(e) => eprintln!("[daemon] failed to create room {room_id}: {e}"),
        }
    }

    // Set up signal handling for graceful shutdown.
    let shutdown = daemon.shutdown_handle();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        eprintln!("[daemon] caught SIGINT, shutting down");
        let _ = shutdown.send(true);
    });

    daemon.run().await
}
