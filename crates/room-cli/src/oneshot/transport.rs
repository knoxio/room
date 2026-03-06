use std::path::Path;

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

use crate::message::Message;

/// Connect to a running broker and deliver a single message without joining the room.
/// Returns the broadcast echo (with broker-assigned id/ts) so callers have the message ID.
pub async fn send_message(
    socket_path: &Path,
    username: &str,
    content: &str,
) -> anyhow::Result<Message> {
    let stream = UnixStream::connect(socket_path).await.map_err(|e| {
        anyhow::anyhow!("cannot connect to broker at {}: {e}", socket_path.display())
    })?;
    let (r, mut w) = stream.into_split();
    w.write_all(format!("SEND:{username}\n").as_bytes()).await?;
    w.write_all(format!("{content}\n").as_bytes()).await?;

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let msg: Message = serde_json::from_str(line.trim())
        .map_err(|e| anyhow::anyhow!("broker returned invalid JSON: {e}: {:?}", line.trim()))?;
    Ok(msg)
}

/// Connect to a running broker and deliver a single message authenticated by token.
///
/// Sends `TOKEN:<token>\n` as the handshake (instead of `SEND:<username>\n`).
/// The broker resolves the username from its in-memory token map.
pub async fn send_message_with_token(
    socket_path: &Path,
    token: &str,
    content: &str,
) -> anyhow::Result<Message> {
    let stream = UnixStream::connect(socket_path).await.map_err(|e| {
        anyhow::anyhow!("cannot connect to broker at {}: {e}", socket_path.display())
    })?;
    let (r, mut w) = stream.into_split();
    w.write_all(format!("TOKEN:{token}\n").as_bytes()).await?;
    // content is already a JSON envelope from cmd_send; newlines are escaped by serde.
    w.write_all(format!("{content}\n").as_bytes()).await?;

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    // Broker may return an error envelope instead of a broadcast echo.
    let v: serde_json::Value = serde_json::from_str(line.trim())
        .map_err(|e| anyhow::anyhow!("broker returned invalid JSON: {e}: {:?}", line.trim()))?;
    if v["type"] == "error" {
        let code = v["code"].as_str().unwrap_or("unknown");
        if code == "invalid_token" {
            anyhow::bail!("invalid token — run: room join {}", socket_path.display());
        }
        anyhow::bail!("broker error: {code}");
    }
    let msg: Message = serde_json::from_value(v)
        .map_err(|e| anyhow::anyhow!("broker returned unexpected JSON: {e}"))?;
    Ok(msg)
}

/// Register a username with the broker and obtain a session token.
///
/// The broker checks for username collisions. On success it returns a token
/// envelope; on collision it returns an error envelope.
pub async fn join_session(socket_path: &Path, username: &str) -> anyhow::Result<(String, String)> {
    let stream = UnixStream::connect(socket_path).await.map_err(|e| {
        anyhow::anyhow!("cannot connect to broker at {}: {e}", socket_path.display())
    })?;
    let (r, mut w) = stream.into_split();
    w.write_all(format!("JOIN:{username}\n").as_bytes()).await?;

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let v: serde_json::Value = serde_json::from_str(line.trim())
        .map_err(|e| anyhow::anyhow!("broker returned invalid JSON: {e}: {:?}", line.trim()))?;
    if v["type"] == "error" {
        let code = v["code"].as_str().unwrap_or("unknown");
        if code == "username_taken" {
            anyhow::bail!("username '{}' is already in use in this room", username);
        }
        anyhow::bail!("broker error: {code}");
    }
    let token = v["token"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("broker response missing 'token' field"))?
        .to_owned();
    let returned_user = v["username"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("broker response missing 'username' field"))?
        .to_owned();
    Ok((returned_user, token))
}
