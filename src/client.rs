use std::path::PathBuf;

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

use crate::{message::Message, tui};

pub struct Client {
    pub socket_path: PathBuf,
    pub username: String,
    pub agent_mode: bool,
    pub history_lines: usize,
}

impl Client {
    pub async fn run(self) -> anyhow::Result<()> {
        let stream = UnixStream::connect(&self.socket_path).await?;
        let (read_half, mut write_half) = stream.into_split();

        // Handshake: send username
        write_half
            .write_all(format!("{}\n", self.username).as_bytes())
            .await?;

        let reader = BufReader::new(read_half);

        if self.agent_mode {
            run_agent(reader, write_half, &self.username, self.history_lines).await
        } else {
            tui::run(reader, write_half, &self.username, self.history_lines).await
        }
    }
}

async fn run_agent(
    mut reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    mut write_half: tokio::net::unix::OwnedWriteHalf,
    username: &str,
    history_lines: usize,
) -> anyhow::Result<()> {
    // Buffer messages until we see our own join (signals end of history replay),
    // then print the last `history_lines` buffered messages and stream the rest.
    let username_owned = username.to_owned();

    let inbound = tokio::spawn(async move {
        let mut history_buf: Vec<String> = Vec::new();
        let mut history_done = false;
        let mut line = String::new();

        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if history_done {
                        println!("{trimmed}");
                    } else {
                        // Look for our own join event to mark end of history
                        let is_own_join = serde_json::from_str::<Message>(trimmed)
                            .ok()
                            .map(|m| {
                                matches!(&m, Message::Join { user, .. } if user == &username_owned)
                            })
                            .unwrap_or(false);

                        if is_own_join {
                            // Flush last N history entries
                            let start = history_buf.len().saturating_sub(history_lines);
                            for h in &history_buf[start..] {
                                println!("{h}");
                            }
                            history_done = true;
                            println!("{trimmed}");
                        } else {
                            history_buf.push(trimmed.to_owned());
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[agent] read error: {e}");
                    break;
                }
            }
        }
    });

    let _outbound = tokio::spawn(async move {
        let stdin = tokio::io::stdin();
        let mut stdin_reader = BufReader::new(stdin);
        let mut line = String::new();
        loop {
            line.clear();
            match stdin_reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if write_half
                        .write_all(format!("{trimmed}\n").as_bytes())
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("[agent] stdin error: {e}");
                    break;
                }
            }
        }
    });

    // Stay alive until the broker closes the connection (inbound EOF),
    // even if stdin is already exhausted.  This lets agents receive responses
    // to messages they sent before their stdin closed.
    inbound.await.ok();
    Ok(())
}
