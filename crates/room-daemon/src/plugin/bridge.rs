//! Bridge between the abstract plugin framework (room-protocol) and the
//! concrete broker internals (room-cli). This module provides the concrete
//! [`ChatWriter`] and [`HistoryReader`] types that implement the
//! [`MessageWriter`] and [`HistoryAccess`] traits respectively.
//!
//! **This is the only plugin submodule that imports from `crate::broker`.**
//! Plugin authors never use these types directly — they receive trait objects
//! via [`CommandContext`].

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use room_protocol::plugin::{BoxFuture, HistoryAccess, MessageWriter, RoomMetadata, UserInfo};

use room_protocol::{make_event, make_system, Message};

use crate::{
    broker::{
        fanout::broadcast_and_persist,
        state::{ClientMap, StatusMap},
    },
    history,
};

// ── HistoryReader ───────────────────────────────────────────────────────────

/// Scoped read-only handle to a room's chat history.
///
/// Respects DM visibility — a plugin invoked by user X will not see DMs
/// between Y and Z.
///
/// Implements [`HistoryAccess`] so it can be passed as
/// `Box<dyn HistoryAccess>` in [`CommandContext`].
pub struct HistoryReader {
    chat_path: PathBuf,
    viewer: String,
}

impl HistoryReader {
    pub(crate) fn new(chat_path: &Path, viewer: &str) -> Self {
        Self {
            chat_path: chat_path.to_owned(),
            viewer: viewer.to_owned(),
        }
    }

    fn filter_dms(&self, messages: Vec<Message>) -> Vec<Message> {
        messages
            .into_iter()
            .filter(|m| match m {
                Message::DirectMessage { user, to, .. } => {
                    user == &self.viewer || to == &self.viewer
                }
                _ => true,
            })
            .collect()
    }
}

impl HistoryAccess for HistoryReader {
    fn all(&self) -> BoxFuture<'_, anyhow::Result<Vec<Message>>> {
        Box::pin(async {
            let all = history::load(&self.chat_path).await?;
            Ok(self.filter_dms(all))
        })
    }

    fn tail(&self, n: usize) -> BoxFuture<'_, anyhow::Result<Vec<Message>>> {
        Box::pin(async move {
            let all = history::tail(&self.chat_path, n).await?;
            Ok(self.filter_dms(all))
        })
    }

    fn since(&self, message_id: &str) -> BoxFuture<'_, anyhow::Result<Vec<Message>>> {
        let message_id = message_id.to_owned();
        Box::pin(async move {
            let all = history::load(&self.chat_path).await?;
            let start = all
                .iter()
                .position(|m| m.id() == message_id)
                .map(|i| i + 1)
                .unwrap_or(0);
            Ok(self.filter_dms(all[start..].to_vec()))
        })
    }

    fn count(&self) -> BoxFuture<'_, anyhow::Result<usize>> {
        Box::pin(async {
            let all = history::load(&self.chat_path).await?;
            Ok(all.len())
        })
    }
}

// ── ChatWriter ──────────────────────────────────────────────────────────────

/// Short-lived scoped handle for a plugin to write messages to the chat.
///
/// Posts as `plugin:<name>` — plugins cannot impersonate users. The writer
/// is valid only for the duration of [`Plugin::handle`].
///
/// Implements [`MessageWriter`] so it can be passed as
/// `Box<dyn MessageWriter>` in [`CommandContext`].
pub struct ChatWriter {
    clients: ClientMap,
    chat_path: Arc<PathBuf>,
    room_id: Arc<String>,
    seq_counter: Arc<AtomicU64>,
    /// Identity the writer posts as (e.g. `"plugin:stats"`).
    identity: String,
}

impl ChatWriter {
    pub(crate) fn new(
        clients: &ClientMap,
        chat_path: &Arc<PathBuf>,
        room_id: &Arc<String>,
        seq_counter: &Arc<AtomicU64>,
        plugin_name: &str,
    ) -> Self {
        Self {
            clients: clients.clone(),
            chat_path: chat_path.clone(),
            room_id: room_id.clone(),
            seq_counter: seq_counter.clone(),
            identity: format!("plugin:{plugin_name}"),
        }
    }
}

impl MessageWriter for ChatWriter {
    fn broadcast(&self, content: &str) -> BoxFuture<'_, anyhow::Result<()>> {
        let msg = make_system(&self.room_id, &self.identity, content);
        Box::pin(async move {
            broadcast_and_persist(&msg, &self.clients, &self.chat_path, &self.seq_counter).await?;
            Ok(())
        })
    }

    fn reply_to(&self, username: &str, content: &str) -> BoxFuture<'_, anyhow::Result<()>> {
        let msg = make_system(&self.room_id, &self.identity, content);
        let username = username.to_owned();
        Box::pin(async move {
            let seq = self.seq_counter.fetch_add(1, Ordering::SeqCst) + 1;
            let mut msg = msg;
            msg.set_seq(seq);
            history::append(&self.chat_path, &msg).await?;

            let line = format!("{}\n", serde_json::to_string(&msg)?);
            let map = self.clients.lock().await;
            for (uname, tx) in map.values() {
                if *uname == username {
                    let _ = tx.send(line.clone());
                }
            }
            Ok(())
        })
    }

    fn emit_event(
        &self,
        event_type: room_protocol::EventType,
        content: &str,
        params: Option<serde_json::Value>,
    ) -> BoxFuture<'_, anyhow::Result<()>> {
        let msg = make_event(&self.room_id, &self.identity, event_type, content, params);
        Box::pin(async move {
            broadcast_and_persist(&msg, &self.clients, &self.chat_path, &self.seq_counter).await?;
            Ok(())
        })
    }
}

// ── RoomMetadata factory ────────────────────────────────────────────────────

/// Build a [`RoomMetadata`] snapshot from live broker state.
pub(crate) async fn snapshot_metadata(
    status_map: &StatusMap,
    host_user: &Arc<tokio::sync::Mutex<Option<String>>>,
    chat_path: &Path,
) -> RoomMetadata {
    let map = status_map.lock().await;
    let online_users: Vec<UserInfo> = map
        .iter()
        .map(|(u, s)| UserInfo {
            username: u.clone(),
            status: s.clone(),
        })
        .collect();
    drop(map);

    let host = host_user.lock().await.clone();

    let message_count = history::load(chat_path)
        .await
        .map(|msgs| msgs.len())
        .unwrap_or(0);

    RoomMetadata {
        online_users,
        host,
        message_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn history_reader_filters_dms() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();

        let dm = room_protocol::make_dm("r", "alice", "bob", "secret");
        let public = room_protocol::make_message("r", "carol", "hello all");
        history::append(path, &dm).await.unwrap();
        history::append(path, &public).await.unwrap();

        let reader_alice = HistoryReader::new(path, "alice");
        let msgs = reader_alice.all().await.unwrap();
        assert_eq!(msgs.len(), 2);

        let reader_carol = HistoryReader::new(path, "carol");
        let msgs = reader_carol.all().await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].user(), "carol");
    }

    #[tokio::test]
    async fn history_reader_tail_and_count() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();

        for i in 0..5 {
            history::append(
                path,
                &room_protocol::make_message("r", "u", format!("msg {i}")),
            )
            .await
            .unwrap();
        }

        let reader = HistoryReader::new(path, "u");
        assert_eq!(reader.count().await.unwrap(), 5);

        let tail = reader.tail(3).await.unwrap();
        assert_eq!(tail.len(), 3);
    }

    #[tokio::test]
    async fn history_reader_since() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();

        let msg1 = room_protocol::make_message("r", "u", "first");
        let msg2 = room_protocol::make_message("r", "u", "second");
        let msg3 = room_protocol::make_message("r", "u", "third");
        let id1 = msg1.id().to_owned();
        history::append(path, &msg1).await.unwrap();
        history::append(path, &msg2).await.unwrap();
        history::append(path, &msg3).await.unwrap();

        let reader = HistoryReader::new(path, "u");
        let since = reader.since(&id1).await.unwrap();
        assert_eq!(since.len(), 2);
    }
}
