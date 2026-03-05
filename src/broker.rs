use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{broadcast, Mutex},
};

use crate::{
    history,
    message::{make_join, make_leave, make_system, parse_client_line, Message},
};

type ClientMap = Arc<Mutex<HashMap<u64, broadcast::Sender<String>>>>;
type StatusMap = Arc<Mutex<HashMap<String, String>>>;

pub struct Broker {
    room_id: String,
    chat_path: PathBuf,
    socket_path: PathBuf,
}

impl Broker {
    pub fn new(room_id: &str, chat_path: PathBuf, socket_path: PathBuf) -> Self {
        Self {
            room_id: room_id.to_owned(),
            chat_path,
            socket_path,
        }
    }

    pub async fn run(self) -> anyhow::Result<()> {
        // Remove stale socket synchronously — using tokio::fs here is dangerous
        // because the blocking thread pool may be shutting down if the broker
        // is starting up inside a dying process.
        if self.socket_path.exists() {
            std::fs::remove_file(&self.socket_path)?;
        }

        let listener = UnixListener::bind(&self.socket_path)?;
        eprintln!("[broker] listening on {}", self.socket_path.display());

        let clients: ClientMap = Arc::new(Mutex::new(HashMap::new()));
        let status_map: StatusMap = Arc::new(Mutex::new(HashMap::new()));
        let chat_path = Arc::new(self.chat_path.clone());
        let room_id = Arc::new(self.room_id.clone());
        let mut next_id: u64 = 0;

        loop {
            let (stream, _) = listener.accept().await?;
            next_id += 1;
            let cid = next_id;

            let (tx, _) = broadcast::channel::<String>(256);
            clients.lock().await.insert(cid, tx.clone());

            let clients_clone = clients.clone();
            let status_map_clone = status_map.clone();
            let chat_path_clone = chat_path.clone();
            let room_id_clone = room_id.clone();

            tokio::spawn(async move {
                if let Err(e) = handle_client(
                    cid,
                    stream,
                    tx,
                    clients_clone.clone(),
                    status_map_clone,
                    chat_path_clone,
                    room_id_clone,
                )
                .await
                {
                    eprintln!("[broker] client {cid} error: {e:#}");
                }
                clients_clone.lock().await.remove(&cid);
            });
        }
    }
}

async fn handle_client(
    cid: u64,
    stream: UnixStream,
    own_tx: broadcast::Sender<String>,
    clients: ClientMap,
    status_map: StatusMap,
    chat_path: Arc<PathBuf>,
    room_id: Arc<String>,
) -> anyhow::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // First line: username handshake
    let mut username = String::new();
    reader.read_line(&mut username).await?;
    let username = username.trim().to_owned();
    if username.is_empty() {
        return Ok(());
    }

    eprintln!("[broker] {username} joined (cid={cid})");

    // Track this user in the status map (empty status by default)
    status_map.lock().await.insert(username.clone(), String::new());

    // Subscribe before sending history so we don't miss concurrent messages
    let mut rx = own_tx.subscribe();

    // Send chat history directly to this client's socket.
    // If the client disconnects mid-replay, treat it as a clean exit.
    let history = history::load(&chat_path).await.unwrap_or_default();
    for msg in &history {
        let line = format!("{}\n", serde_json::to_string(msg)?);
        if write_half.write_all(line.as_bytes()).await.is_err() {
            return Ok(());
        }
    }

    // Broadcast join event (also persists it)
    let join_msg = make_join(room_id.as_str(), &username);
    if let Err(e) = broadcast_and_persist(&join_msg, &clients, &chat_path).await {
        eprintln!("[broker] broadcast_and_persist(join) failed: {e:#}");
        return Ok(());
    }

    // Wrap write half in Arc<Mutex> for shared use by outbound and inbound tasks
    let write_half = Arc::new(Mutex::new(write_half));

    // Outbound: receive from broadcast channel, forward to client socket
    let write_half_out = write_half.clone();
    let outbound = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(line) => {
                    let mut wh = write_half_out.lock().await;
                    if wh.write_all(line.as_bytes()).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("[broker] cid={cid} lagged by {n}");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Inbound: read lines from client socket, parse and broadcast
    let username_in = username.clone();
    let room_id_in = room_id.clone();
    let clients_in = clients.clone();
    let status_map_in = status_map.clone();
    let chat_path_in = chat_path.clone();
    let write_half_in = write_half.clone();
    let inbound = tokio::spawn(async move {
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
                    match parse_client_line(trimmed, &room_id_in, &username_in) {
                        Ok(msg) => {
                            if let Message::Command { ref cmd, ref params, .. } = msg {
                                if cmd == "set_status" {
                                    let status = params.first().cloned().unwrap_or_default();
                                    status_map_in.lock().await.insert(username_in.clone(), status.clone());
                                    let display = if status.is_empty() {
                                        format!("{username_in} cleared their status")
                                    } else {
                                        format!("{username_in} set status: {status}")
                                    };
                                    let sys = make_system(&room_id_in, "broker", display);
                                    if let Err(e) = broadcast_and_persist(&sys, &clients_in, &chat_path_in).await {
                                        eprintln!("[broker] persist error: {e:#}");
                                    }
                                    continue;
                                } else if cmd == "who" {
                                    let map = status_map_in.lock().await;
                                    let mut entries: Vec<String> = map
                                        .iter()
                                        .map(|(u, s)| {
                                            if s.is_empty() {
                                                u.clone()
                                            } else {
                                                format!("{u}: {s}")
                                            }
                                        })
                                        .collect();
                                    entries.sort();
                                    drop(map);
                                    let content = if entries.is_empty() {
                                        "no users online".to_owned()
                                    } else {
                                        format!("online — {}", entries.join(", "))
                                    };
                                    let sys = make_system(&room_id_in, "broker", content);
                                    if let Ok(json) = serde_json::to_string(&sys) {
                                        let _ = write_half_in
                                            .lock()
                                            .await
                                            .write_all(format!("{json}\n").as_bytes())
                                            .await;
                                    }
                                    continue;
                                }
                            }
                            if let Err(e) =
                                broadcast_and_persist(&msg, &clients_in, &chat_path_in).await
                            {
                                eprintln!("[broker] persist error: {e:#}");
                            }
                        }
                        Err(e) => eprintln!("[broker] bad message from {username_in}: {e}"),
                    }
                }
                Err(_) => break,
            }
        }
    });

    tokio::select! {
        _ = outbound => {},
        _ = inbound => {},
    }

    // Remove user from status map on disconnect
    status_map.lock().await.remove(&username);

    // Broadcast leave event
    let leave_msg = make_leave(room_id.as_str(), &username);
    let _ = broadcast_and_persist(&leave_msg, &clients, &chat_path).await;
    eprintln!("[broker] {username} left (cid={cid})");

    Ok(())
}

async fn broadcast_and_persist(
    msg: &Message,
    clients: &ClientMap,
    chat_path: &Path,
) -> anyhow::Result<()> {
    history::append(chat_path, msg).await?;

    let line = format!("{}\n", serde_json::to_string(msg)?);
    let map = clients.lock().await;
    for tx in map.values() {
        let _ = tx.send(line.clone());
    }
    Ok(())
}
