use std::path::PathBuf;

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

use crate::{
    message::Message,
    oneshot::transport::{join_session_target, resolve_socket_target},
    paths, tui,
};

pub struct Client {
    pub socket_path: PathBuf,
    pub room_id: String,
    pub username: String,
    pub agent_mode: bool,
    pub history_lines: usize,
}

impl Client {
    pub async fn run(self) -> anyhow::Result<()> {
        // Ensure a token exists for this room/user so that subsequent oneshot
        // commands (send, poll, watch) work without a manual `room join`.
        self.ensure_token().await;

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
            tui::run(
                reader,
                write_half,
                &self.room_id,
                &self.username,
                self.history_lines,
                self.socket_path.clone(),
            )
            .await
        }
    }

    /// Ensure a valid session token exists for this room/user pair.
    ///
    /// Always attempts a `JOIN:` handshake to acquire a fresh token. This handles
    /// broker restarts (which invalidate old tokens) transparently. If the join
    /// succeeds, the new token is written to `~/.room/state/`. If it fails with
    /// "username_taken", the existing token file is assumed valid (the user is
    /// already registered with the broker). Other errors are logged but not
    /// propagated — the interactive session can proceed regardless.
    async fn ensure_token(&self) {
        if let Err(e) = paths::ensure_room_dirs() {
            eprintln!("[tui] cannot create ~/.room dirs: {e}");
            return;
        }

        // Use resolve_socket_target so that ROOM_SOCKET env and daemon
        // auto-discovery are consistent with all other oneshot commands.
        let target = resolve_socket_target(&self.room_id, None);
        match join_session_target(&target, &self.username).await {
            Ok((returned_user, token)) => {
                let token_data = serde_json::json!({"username": returned_user, "token": token});
                let path = paths::token_path(&self.room_id, &returned_user);
                if let Err(e) = std::fs::write(&path, format!("{token_data}\n")) {
                    eprintln!("[tui] failed to write token file: {e}");
                }
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("already in use") {
                    // Username is registered — existing token should be valid.
                    let token_path = paths::token_path(&self.room_id, &self.username);
                    if !has_valid_token_file(&token_path) {
                        eprintln!(
                            "[tui] username registered but no token file found — \
                             run `room join {} {}` to recover",
                            self.room_id, self.username
                        );
                    }
                } else if msg.contains("cannot connect") {
                    // Broker not running — token will be acquired on next session.
                } else {
                    eprintln!("[tui] auto-join failed: {e}");
                }
            }
        }
    }
}

/// Check whether a token file exists and contains valid JSON with a `token` field.
fn has_valid_token_file(path: &std::path::Path) -> bool {
    if !path.exists() {
        return false;
    }
    let Ok(data) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(data.trim()) else {
        return false;
    };
    v["token"].as_str().is_some()
}

/// Resolve the default username from the `$USER` environment variable.
///
/// Returns `None` when `$USER` is not set or is empty.
pub fn default_username() -> Option<String> {
    std::env::var("USER").ok().filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn has_valid_token_file_returns_false_for_missing_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.token");
        assert!(!has_valid_token_file(&path));
    }

    #[test]
    fn has_valid_token_file_returns_true_for_valid_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.token");
        let data = serde_json::json!({"username": "alice", "token": "tok-123"});
        std::fs::write(&path, format!("{data}\n")).unwrap();
        assert!(has_valid_token_file(&path));
    }

    #[test]
    fn has_valid_token_file_returns_false_for_corrupt_json() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("corrupt.token");
        std::fs::write(&path, "not valid json").unwrap();
        assert!(!has_valid_token_file(&path));
    }

    #[test]
    fn has_valid_token_file_returns_false_for_missing_token_field() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("no-token.token");
        let data = serde_json::json!({"username": "alice"});
        std::fs::write(&path, format!("{data}\n")).unwrap();
        assert!(!has_valid_token_file(&path));
    }

    #[test]
    fn default_username_returns_user_env_var() {
        // $USER should be set on macOS/Linux test environments.
        let result = default_username();
        assert!(
            result.is_some(),
            "$USER should be set in the test environment"
        );
        assert!(!result.unwrap().is_empty(), "$USER should not be empty");
    }

    /// has_valid_token_file returns false for empty file.
    #[test]
    fn has_valid_token_file_returns_false_for_empty_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("empty.token");
        std::fs::write(&path, "").unwrap();
        assert!(!has_valid_token_file(&path));
    }

    /// has_valid_token_file returns false when token field is null.
    #[test]
    fn has_valid_token_file_returns_false_for_null_token() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("null-token.token");
        let data = serde_json::json!({"username": "alice", "token": null});
        std::fs::write(&path, format!("{data}\n")).unwrap();
        assert!(!has_valid_token_file(&path));
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
