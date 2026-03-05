use std::path::PathBuf;

use clap::{Parser, Subcommand};
use room::{broker::Broker, client::Client, history::default_chat_path, oneshot};
use tokio::net::UnixStream;

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Register a username with the broker and receive a session token.
    ///
    /// Writes the token to `/tmp/room-<room_id>.token`. Subsequent `send`,
    /// `poll`, and `watch` calls read the username and token from that file —
    /// no username argument required. Returns an error if the username is
    /// already in use in the room.
    Join { room_id: String, username: String },
    /// One-shot send a message to a room (requires a running broker).
    ///
    /// The broker resolves the sender's identity from the token issued by `room join`.
    /// Prints the broadcast message JSON and exits.
    Send {
        room_id: String,
        /// Session token from `room join` (required)
        #[arg(short = 't', long)]
        token: String,
        /// Recipient username for a direct message
        #[arg(long)]
        to: Option<String>,
        /// Message content; all remaining tokens are joined with spaces
        #[arg(trailing_var_arg = true, num_args = 1..)]
        message: Vec<String>,
    },
    /// Poll for new messages, printing NDJSON to stdout.
    ///
    /// Updates a per-user cursor file so subsequent calls return only unseen messages.
    Poll {
        room_id: String,
        /// Session token from `room join` (required)
        #[arg(short = 't', long)]
        token: String,
        /// Return only messages after this message ID (overrides stored cursor)
        #[arg(long)]
        since: Option<String>,
    },
    /// Fetch the last N messages from history without updating the poll cursor.
    ///
    /// Reads the NDJSON chat file directly — no broker connection required.
    /// Useful for agents that need to re-read recent context after a context reset.
    Pull {
        room_id: String,
        /// Session token from `room join` (required)
        #[arg(short = 't', long)]
        token: String,
        /// Number of messages to return (default: 20, max: 200)
        #[arg(short = 'n', default_value_t = 20)]
        count: usize,
    },
    /// Watch for new messages from other users, blocking until at least one arrives.
    ///
    /// Polls the chat file on a configurable interval. Shares the cursor file with
    /// `room poll` so no messages are re-delivered. Exits after printing the first
    /// batch of foreign messages as NDJSON.
    Watch {
        room_id: String,
        /// Session token from `room join` (required)
        #[arg(short = 't', long)]
        token: String,
        /// Poll interval in seconds (default: 5)
        #[arg(long, default_value_t = 5)]
        interval: u64,
    },
}

#[derive(Parser, Debug)]
#[command(
    name = "room",
    version,
    disable_version_flag = true,
    about = "Multi-user chat room for agent/human coordination"
)]
struct Args {
    /// Print version and exit
    #[arg(short = 'v', long = "version", action = clap::ArgAction::Version)]
    _version: (),

    /// Room identifier (required when no subcommand is given)
    room_id: Option<String>,

    /// Your username (required when no subcommand is given)
    username: Option<String>,

    /// Number of history messages to replay on join
    #[arg(short = 'n', default_value_t = 20)]
    history_lines: usize,

    /// Chat file path (only used when creating a new room)
    #[arg(short = 'f')]
    chat_file: Option<PathBuf>,

    /// Non-interactive agent mode: read JSON from stdin, write JSON to stdout
    #[arg(long)]
    agent: bool,

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    match args.command {
        Some(Cmd::Join { room_id, username }) => {
            oneshot::cmd_join(&room_id, &username).await?;
        }
        Some(Cmd::Send {
            room_id,
            token,
            to,
            message,
        }) => {
            let content = message.join(" ");
            oneshot::cmd_send(&room_id, &token, to.as_deref(), &content).await?;
        }
        Some(Cmd::Poll {
            room_id,
            token,
            since,
        }) => {
            oneshot::cmd_poll(&room_id, &token, since).await?;
        }
        Some(Cmd::Pull {
            room_id,
            token,
            count,
        }) => {
            oneshot::cmd_pull(&room_id, &token, count).await?;
        }
        Some(Cmd::Watch {
            room_id,
            token,
            interval,
        }) => {
            oneshot::cmd_watch(&room_id, &token, interval).await?;
        }
        None => {
            let room_id = args.room_id.unwrap_or_else(|| {
                eprintln!("error: room_id is required when no subcommand is given");
                std::process::exit(1);
            });
            let username = args.username.unwrap_or_else(|| {
                eprintln!("error: username is required when no subcommand is given");
                std::process::exit(1);
            });
            run_join(
                room_id,
                username,
                args.history_lines,
                args.chat_file,
                args.agent,
            )
            .await?;
        }
    }

    Ok(())
}

async fn run_join(
    room_id: String,
    username: String,
    history_lines: usize,
    chat_file: Option<PathBuf>,
    agent: bool,
) -> anyhow::Result<()> {
    let socket_path = PathBuf::from(format!("/tmp/room-{}.sock", room_id));
    let meta_path = PathBuf::from(format!("/tmp/room-{}.meta", room_id));

    let become_broker = match UnixStream::connect(&socket_path).await {
        Ok(_) => false,
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            eprintln!("[room] stale socket detected (ECONNREFUSED), cleaning up");
            let _ = std::fs::remove_file(&socket_path);
            true
        }
        Err(e) => {
            eprintln!("[room] no broker found ({e}), becoming broker");
            true
        }
    };

    if become_broker {
        let resolved_chat_path = resolve_chat_path(&chat_file, &meta_path, &room_id);

        let meta = serde_json::json!({ "chat_path": resolved_chat_path.to_string_lossy() });
        let _ = std::fs::write(&meta_path, format!("{meta}\n"));

        eprintln!(
            "[room] starting broker for '{}', chat: {}",
            room_id,
            resolved_chat_path.display()
        );

        let broker = Broker::new(&room_id, resolved_chat_path, socket_path.clone());

        tokio::spawn(async move {
            if let Err(e) = broker.run().await {
                eprintln!("[broker] fatal: {e:#}");
            }
        });

        // Wait until the socket is ready to accept connections
        for _ in 0..50 {
            if UnixStream::connect(&socket_path).await.is_ok() {
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
        }

        let client = Client {
            socket_path,
            room_id: room_id.clone(),
            username,
            agent_mode: agent,
            history_lines,
        };
        client.run().await?;

        // The local client has disconnected, but the broker should keep serving
        // other clients until a signal arrives.  Without this, the tokio runtime
        // shuts down immediately and cancels any in-flight spawn_blocking tasks
        // (e.g. file writes).
        tokio::signal::ctrl_c().await.ok();
    } else {
        eprintln!("[room] connecting to existing room '{room_id}'");
        let client = Client {
            socket_path,
            room_id,
            username,
            agent_mode: agent,
            history_lines,
        };
        client.run().await?;
    }

    Ok(())
}

fn resolve_chat_path(chat_file: &Option<PathBuf>, meta_path: &PathBuf, room_id: &str) -> PathBuf {
    if let Some(ref p) = chat_file {
        return p.clone();
    }
    if meta_path.exists() {
        if let Ok(data) = std::fs::read_to_string(meta_path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                if let Some(p) = v["chat_path"].as_str() {
                    return PathBuf::from(p);
                }
            }
        }
    }
    default_chat_path(room_id)
}
