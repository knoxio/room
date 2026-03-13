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
        if msg.is_visible_to(username, host_name) {
            let _ = tx.send(line.clone());
        }
    }
    Ok(msg)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{make_dm, make_message};
    use std::collections::HashMap;
    use tokio::sync::{broadcast, Mutex};

    fn make_clients() -> ClientMap {
        Arc::new(Mutex::new(HashMap::new()))
    }

    fn make_seq() -> Arc<AtomicU64> {
        Arc::new(AtomicU64::new(0))
    }

    fn make_host(name: Option<&str>) -> HostUser {
        Arc::new(Mutex::new(name.map(|s| s.to_owned())))
    }

    /// Insert a fake client into the client map and return the receiver.
    async fn add_client(
        clients: &ClientMap,
        id: u64,
        username: &str,
    ) -> broadcast::Receiver<String> {
        let (tx, rx) = broadcast::channel(16);
        clients.lock().await.insert(id, (username.to_owned(), tx));
        rx
    }

    // ── broadcast_and_persist ─────────────────────────────────────────────

    #[tokio::test]
    async fn broadcast_increments_seq_counter() {
        let dir = tempfile::tempdir().unwrap();
        let chat = dir.path().join("test.chat");
        let clients = make_clients();
        let seq = make_seq();

        let msg = make_message("r", "alice", "hello");
        let out1 = broadcast_and_persist(&msg, &clients, &chat, &seq)
            .await
            .unwrap();
        let out2 = broadcast_and_persist(&msg, &clients, &chat, &seq)
            .await
            .unwrap();

        // seq should be 1 and 2, monotonically increasing
        assert_eq!(out1.seq(), Some(1));
        assert_eq!(out2.seq(), Some(2));
        assert_eq!(seq.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn broadcast_empty_clients_still_persists() {
        let dir = tempfile::tempdir().unwrap();
        let chat = dir.path().join("test.chat");
        let clients = make_clients(); // no clients
        let seq = make_seq();

        let msg = make_message("r", "alice", "persisted but nobody home");
        broadcast_and_persist(&msg, &clients, &chat, &seq)
            .await
            .unwrap();

        // File should still contain the message
        let loaded = crate::history::load(&chat).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].content(), Some("persisted but nobody home"));
    }

    #[tokio::test]
    async fn broadcast_delivers_to_all_clients() {
        let dir = tempfile::tempdir().unwrap();
        let chat = dir.path().join("test.chat");
        let clients = make_clients();
        let seq = make_seq();

        let mut rx_alice = add_client(&clients, 1, "alice").await;
        let mut rx_bob = add_client(&clients, 2, "bob").await;
        let mut rx_carol = add_client(&clients, 3, "carol").await;

        let msg = make_message("r", "alice", "hello all");
        broadcast_and_persist(&msg, &clients, &chat, &seq)
            .await
            .unwrap();

        // All three clients should receive the message
        let line_alice = rx_alice.try_recv().unwrap();
        let line_bob = rx_bob.try_recv().unwrap();
        let line_carol = rx_carol.try_recv().unwrap();

        assert!(line_alice.contains("hello all"));
        assert!(line_bob.contains("hello all"));
        assert!(line_carol.contains("hello all"));

        // All three should have received the same serialized line
        assert_eq!(line_alice, line_bob);
        assert_eq!(line_bob, line_carol);
    }

    // ── dm_and_persist ───────────────────────────────────────────────────

    #[tokio::test]
    async fn dm_delivers_only_to_sender_recipient_and_host() {
        let dir = tempfile::tempdir().unwrap();
        let chat = dir.path().join("test.chat");
        let clients = make_clients();
        let seq = make_seq();
        let host = make_host(Some("host-user"));

        let mut rx_alice = add_client(&clients, 1, "alice").await; // sender
        let mut rx_bob = add_client(&clients, 2, "bob").await; // recipient
        let mut rx_host = add_client(&clients, 3, "host-user").await; // host
        let mut rx_eve = add_client(&clients, 4, "eve").await; // bystander

        let dm = make_dm("r", "alice", "bob", "secret");
        dm_and_persist(&dm, &host, &clients, &chat, &seq)
            .await
            .unwrap();

        // Sender, recipient, and host should receive it
        assert!(rx_alice.try_recv().is_ok());
        assert!(rx_bob.try_recv().is_ok());
        assert!(rx_host.try_recv().is_ok());

        // Bystander should NOT receive the DM
        assert!(
            rx_eve.try_recv().is_err(),
            "eve must not see a DM between alice and bob"
        );
    }

    #[tokio::test]
    async fn dm_offline_recipient_still_persists() {
        let dir = tempfile::tempdir().unwrap();
        let chat = dir.path().join("test.chat");
        let clients = make_clients();
        let seq = make_seq();
        let host = make_host(None);

        // Only the sender is online — recipient is offline
        let mut rx_alice = add_client(&clients, 1, "alice").await;

        let dm = make_dm("r", "alice", "bob", "you there?");
        let result = dm_and_persist(&dm, &host, &clients, &chat, &seq).await;

        // Should succeed without error
        assert!(result.is_ok());

        // Sender still gets their own echo
        assert!(rx_alice.try_recv().is_ok());

        // Message should be persisted on disk for when bob comes online
        let loaded = crate::history::load(&chat).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].content(), Some("you there?"));
    }

    #[tokio::test]
    async fn dm_host_sees_all_dms() {
        let dir = tempfile::tempdir().unwrap();
        let chat = dir.path().join("test.chat");
        let clients = make_clients();
        let seq = make_seq();
        let host = make_host(Some("admin"));

        let mut rx_admin = add_client(&clients, 1, "admin").await;
        let _rx_alice = add_client(&clients, 2, "alice").await;
        let _rx_bob = add_client(&clients, 3, "bob").await;

        // DM between alice and bob — admin is neither sender nor recipient
        let dm = make_dm("r", "alice", "bob", "private chat");
        dm_and_persist(&dm, &host, &clients, &chat, &seq)
            .await
            .unwrap();

        // Host should still see it
        let line = rx_admin.try_recv().unwrap();
        assert!(line.contains("private chat"));
    }
}
