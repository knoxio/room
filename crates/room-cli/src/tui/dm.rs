use std::collections::HashMap;

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
};

use super::input::InputState;
use super::parse::build_payload;
use super::RoomTab;
use crate::message::Message;

/// Read the global token for `username` from the token file on disk.
///
/// Returns `None` if the file doesn't exist or can't be parsed.
pub(super) fn read_user_token(username: &str) -> Option<String> {
    let path = crate::paths::global_token_path(username);
    let data = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(data.trim()).ok()?;
    v["token"].as_str().map(|s| s.to_owned())
}

/// Create or reuse a DM room and return a connected `RoomTab`.
///
/// 1. Sends `CREATE:<dm_room_id>` to the daemon socket. If the room already
///    exists, the daemon returns `room_already_exists` — this is fine, we proceed.
/// 2. Connects to the daemon with `ROOM:<dm_room_id>:<username>` for an
///    interactive session.
/// 3. Spawns a reader task and returns a `RoomTab` ready for the tab list.
pub(super) async fn open_dm_tab(
    socket_path: &std::path::Path,
    dm_room_id: &str,
    username: &str,
    target_user: &str,
    history_lines: usize,
) -> anyhow::Result<RoomTab> {
    use tokio::net::UnixStream;

    // Step 1: Create the DM room (idempotent — ignore "already exists").
    // Read the user's token from the token file for authentication.
    let token = read_user_token(username).unwrap_or_default();
    let config = room_protocol::RoomConfig::dm(username, target_user);
    let config_json = serde_json::to_string(&config)?;
    let authed_config = crate::oneshot::transport::inject_token_into_config(&config_json, &token);
    match crate::oneshot::transport::create_room(socket_path, dm_room_id, &authed_config).await {
        Ok(_) => {}
        Err(e) if e.to_string().contains("already exists") => {}
        Err(e) => return Err(e),
    }

    // Step 2: Join the DM room via the daemon socket.
    let stream = UnixStream::connect(socket_path).await?;
    let (read_half, mut write_half) = stream.into_split();
    let handshake = format!("ROOM:{dm_room_id}:{username}\n");
    write_half.write_all(handshake.as_bytes()).await?;

    // Step 3: Spawn reader task for the new tab.
    let (tx, rx) = mpsc::unbounded_channel::<Message>();
    let username_owned = username.to_owned();
    let reader = BufReader::new(read_half);

    tokio::spawn(async move {
        let mut reader = reader;
        let mut history_buf: Vec<Message> = Vec::new();
        let mut joined = false;
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
                    let Ok(msg) = serde_json::from_str::<Message>(trimmed) else {
                        continue;
                    };

                    if joined {
                        let _ = tx.send(msg);
                    } else {
                        let is_own_join =
                            matches!(&msg, Message::Join { user, .. } if user == &username_owned);
                        if is_own_join {
                            joined = true;
                            let start = history_buf.len().saturating_sub(history_lines);
                            for h in history_buf.drain(start..) {
                                let _ = tx.send(h);
                            }
                            let _ = tx.send(msg);
                        } else {
                            history_buf.push(msg);
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Request /who to seed the member panel.
    let who_payload = build_payload("/who");
    write_half
        .write_all(format!("{who_payload}\n").as_bytes())
        .await?;

    Ok(RoomTab {
        room_id: dm_room_id.to_owned(),
        messages: Vec::new(),
        online_users: Vec::new(),
        user_statuses: HashMap::new(),
        subscription_tiers: HashMap::new(),
        unread_count: 0,
        scroll_offset: 0,
        msg_rx: rx,
        write_half,
    })
}

/// Switch the active tab and sync scroll state between input and the new tab.
pub(super) fn switch_to_tab(
    tabs: &mut [RoomTab],
    active_tab: &mut usize,
    input_state: &mut InputState,
    idx: usize,
) {
    tabs[*active_tab].scroll_offset = input_state.scroll_offset;
    *active_tab = idx;
    tabs[*active_tab].unread_count = 0;
    input_state.scroll_offset = tabs[*active_tab].scroll_offset;
}

/// Write a newline-terminated payload to a write half.
pub(super) async fn write_payload_to_tab(
    write_half: &mut tokio::net::unix::OwnedWriteHalf,
    payload: &str,
) -> anyhow::Result<()> {
    write_half
        .write_all(format!("{payload}\n").as_bytes())
        .await
        .map_err(Into::into)
}

/// Parameters for opening a DM room tab. Bundles the session-scoped constants
/// so `handle_dm_action` stays under the `too-many-arguments` threshold.
pub(super) struct DmTabConfig<'a> {
    pub(super) socket_path: &'a std::path::Path,
    pub(super) username: &'a str,
    pub(super) history_lines: usize,
}

/// Handle `Action::DmRoom` — open or reuse a DM tab and send the first message.
///
/// Returns `Ok(())` on success. On `Err`, the caller should set `result` and
/// `break 'main` to exit the event loop cleanly.
pub(super) async fn handle_dm_action(
    tabs: &mut Vec<RoomTab>,
    active_tab: &mut usize,
    input_state: &mut InputState,
    cfg: &DmTabConfig<'_>,
    target_user: String,
    content: String,
) -> anyhow::Result<()> {
    let fallback = serde_json::json!({
        "type": "dm",
        "to": target_user,
        "content": content
    })
    .to_string();

    let Ok(dm_id) = room_protocol::dm_room_id(cfg.username, &target_user) else {
        // Same user — send as intra-room DM; the broker will reject it cleanly.
        return write_payload_to_tab(&mut tabs[*active_tab].write_half, &fallback).await;
    };

    if let Some(idx) = tabs.iter().position(|t| t.room_id == dm_id) {
        // Tab already open — switch and send.
        switch_to_tab(tabs, active_tab, input_state, idx);
        return write_payload_to_tab(&mut tabs[*active_tab].write_half, &build_payload(&content))
            .await;
    }

    // Create the DM room and open a new tab.
    match open_dm_tab(
        cfg.socket_path,
        &dm_id,
        cfg.username,
        &target_user,
        cfg.history_lines,
    )
    .await
    {
        Ok(new_tab) => {
            tabs.push(new_tab);
            tabs[*active_tab].scroll_offset = input_state.scroll_offset;
            *active_tab = tabs.len() - 1;
            input_state.scroll_offset = 0;
            write_payload_to_tab(&mut tabs[*active_tab].write_half, &build_payload(&content)).await
        }
        Err(_) => {
            // DM room creation failed — fall back to intra-room DM.
            write_payload_to_tab(&mut tabs[*active_tab].write_half, &fallback).await
        }
    }
}
