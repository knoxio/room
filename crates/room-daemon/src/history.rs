use std::{
    io::Write,
    path::{Path, PathBuf},
};

use room_protocol::Message;

pub fn default_chat_path(room_id: &str) -> PathBuf {
    PathBuf::from(format!("/tmp/{room_id}.chat"))
}

/// Read all messages from the NDJSON file, skipping malformed lines.
pub async fn load(path: &Path) -> anyhow::Result<Vec<Message>> {
    if !path.exists() {
        return Ok(vec![]);
    }
    let path = path.to_owned();
    let raw = tokio::task::spawn_blocking(move || std::fs::read_to_string(&path))
        .await
        .map_err(|e| anyhow::anyhow!("blocking file read cancelled: {e}"))??;

    let mut messages = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Message>(trimmed) {
            Ok(msg) => messages.push(msg),
            Err(e) => eprintln!("history: skipping malformed line: {e}"),
        }
    }
    Ok(messages)
}

/// Return the last `n` messages from the NDJSON file, skipping malformed lines.
///
/// Returns all messages when the file has fewer than `n` entries.
/// Returns an empty vec if the file does not exist.
pub async fn tail(path: &Path, n: usize) -> anyhow::Result<Vec<Message>> {
    let all = load(path).await?;
    let start = all.len().saturating_sub(n);
    Ok(all[start..].to_vec())
}

/// Append a single message as a JSON line to the NDJSON file.
///
/// Uses `spawn_blocking` + `std::fs::OpenOptions` directly to avoid the
/// `tokio::fs` abstraction layer which can fail with "background task failed"
/// when the runtime's blocking thread pool is under pressure.
pub async fn append(path: &Path, msg: &Message) -> anyhow::Result<()> {
    let line = format!("{}\n", serde_json::to_string(msg)?);
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        file.write_all(line.as_bytes())?;
        file.flush()
    })
    .await
    .map_err(|e| anyhow::anyhow!("blocking file write cancelled: {e}"))??;
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use room_protocol::{make_join, make_leave, make_message};
    use tempfile::NamedTempFile;

    /// Write messages via `append`, read them back via `load`, assert equality.
    #[tokio::test]
    async fn append_then_load_round_trips_all_variants() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path();

        let msgs = vec![
            make_join("r", "alice"),
            make_message("r", "alice", "hello"),
            make_leave("r", "alice"),
        ];

        for msg in &msgs {
            append(path, msg).await.unwrap();
        }

        let loaded = load(path).await.unwrap();
        assert_eq!(loaded.len(), msgs.len());
        for (orig, loaded) in msgs.iter().zip(loaded.iter()) {
            assert_eq!(orig, loaded);
        }
    }

    #[tokio::test]
    async fn load_nonexistent_returns_empty() {
        let path = PathBuf::from("/tmp/__room_test_nonexistent_file_xyz.chat");
        let result = load(&path).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn load_empty_file_returns_empty() {
        let tmp = NamedTempFile::new().unwrap();
        let result = load(tmp.path()).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn load_skips_malformed_lines_and_returns_valid_ones() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path();

        let good = make_message("r", "bob", "valid message");

        // Write one good line, one garbage line, another good line
        let raw = format!(
            "{}\n{{not valid json}}\n{}\n",
            serde_json::to_string(&good).unwrap(),
            serde_json::to_string(&good).unwrap(),
        );
        tokio::fs::write(path, raw.as_bytes()).await.unwrap();

        let loaded = load(path).await.unwrap();
        assert_eq!(loaded.len(), 2, "malformed line should be silently skipped");
        assert_eq!(loaded[0], good);
        assert_eq!(loaded[1], good);
    }

    #[tokio::test]
    async fn append_creates_file_if_not_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.chat");
        assert!(!path.exists());

        let msg = make_join("r", "alice");
        append(&path, &msg).await.unwrap();

        assert!(path.exists());
        let loaded = load(&path).await.unwrap();
        assert_eq!(loaded.len(), 1);
    }

    #[tokio::test]
    async fn append_is_incremental_not_overwriting() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path();

        for i in 0..5 {
            append(path, &make_message("r", "u", format!("msg {i}")))
                .await
                .unwrap();
        }

        let loaded = load(path).await.unwrap();
        assert_eq!(loaded.len(), 5);
    }

    #[tokio::test]
    async fn load_preserves_message_order() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path();

        let contents: Vec<&str> = vec!["first", "second", "third"];
        for c in &contents {
            append(path, &make_message("r", "u", *c)).await.unwrap();
        }

        let loaded = load(path).await.unwrap();
        let loaded_contents: Vec<&str> = loaded
            .iter()
            .filter_map(|m| {
                if let Message::Message { content, .. } = m {
                    Some(content.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(loaded_contents, contents);
    }
}
