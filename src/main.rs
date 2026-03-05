use std::path::PathBuf;

use clap::Parser;
use room::{broker::Broker, client::Client, history::default_chat_path};
use tokio::net::UnixStream;

#[derive(Parser, Debug)]
#[command(name = "room", about = "Multi-user chat room for agent/human coordination")]
struct Args {
    /// Room identifier
    room_id: String,

    /// Your username
    username: String,

    /// Number of history messages to replay on join
    #[arg(short = 'n', default_value_t = 20)]
    history_lines: usize,

    /// Chat file path (only used when creating a new room)
    #[arg(short = 'f')]
    chat_file: Option<PathBuf>,

    /// Non-interactive agent mode: read JSON from stdin, write JSON to stdout
    #[arg(long)]
    agent: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let socket_path = PathBuf::from(format!("/tmp/room-{}.sock", args.room_id));
    let meta_path = PathBuf::from(format!("/tmp/room-{}.meta", args.room_id));

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
        let chat_path = resolve_chat_path(&args, &meta_path);

        let meta = serde_json::json!({ "chat_path": chat_path.to_string_lossy() });
        let _ = std::fs::write(&meta_path, format!("{meta}\n"));

        eprintln!(
            "[room] starting broker for '{}', chat: {}",
            args.room_id,
            chat_path.display()
        );

        let broker = Broker::new(&args.room_id, chat_path, socket_path.clone());

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
            username: args.username,
            agent_mode: args.agent,
            history_lines: args.history_lines,
        };
        client.run().await?;

        // The local client has disconnected, but the broker should keep serving
        // other clients until a signal arrives.  Without this, the tokio runtime
        // shuts down immediately and cancels any in-flight spawn_blocking tasks
        // (e.g. file writes).
        tokio::signal::ctrl_c().await.ok();
    } else {
        eprintln!("[room] connecting to existing room '{}'", args.room_id);
        let client = Client {
            socket_path,
            username: args.username,
            agent_mode: args.agent,
            history_lines: args.history_lines,
        };
        client.run().await?;
    }

    Ok(())
}

fn resolve_chat_path(args: &Args, meta_path: &PathBuf) -> PathBuf {
    if let Some(ref p) = args.chat_file {
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
    default_chat_path(&args.room_id)
}
