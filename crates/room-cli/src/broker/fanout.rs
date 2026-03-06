use std::{
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use crate::{history, message::Message};

use super::state::{ClientMap, HostUser};

/// Assign the next sequence number, persist a message, and fan it out to all connected clients.
///
/// Returns the message with its `seq` field populated so callers can echo it to one-shot senders.
pub(crate) async fn broadcast_and_persist(
    msg: &Message,
    clients: &ClientMap,
    chat_path: &Path,
    seq_counter: &Arc<AtomicU64>,
) -> anyhow::Result<Message> {
    let seq = seq_counter.fetch_add(1, Ordering::SeqCst) + 1;
    let mut msg = msg.clone();
    msg.set_seq(seq);

    history::append(chat_path, &msg).await?;

    let line = format!("{}\n", serde_json::to_string(&msg)?);
    let map = clients.lock().await;
    for (_, tx) in map.values() {
        let _ = tx.send(line.clone());
    }
    Ok(msg)
}

/// Assign the next sequence number, persist a DM, and deliver it only to the sender,
/// the recipient, and the host.
/// If the recipient is offline the message is still persisted and the sender
/// receives their own echo; no error is returned.
pub(crate) async fn dm_and_persist(
    msg: &Message,
    sender: &str,
    recipient: &str,
    host_user: &HostUser,
    clients: &ClientMap,
    chat_path: &Path,
    seq_counter: &Arc<AtomicU64>,
) -> anyhow::Result<Message> {
    let seq = seq_counter.fetch_add(1, Ordering::SeqCst) + 1;
    let mut msg = msg.clone();
    msg.set_seq(seq);

    history::append(chat_path, &msg).await?;

    let line = format!("{}\n", serde_json::to_string(&msg)?);
    let host = host_user.lock().await;
    let host_name = host.as_deref();
    let map = clients.lock().await;
    for (username, tx) in map.values() {
        if username == sender || username == recipient || host_name == Some(username.as_str()) {
            let _ = tx.send(line.clone());
        }
    }
    Ok(msg)
}
