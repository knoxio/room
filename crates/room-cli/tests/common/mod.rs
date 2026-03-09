/// Shared test helpers for broker lifecycle management.
///
/// Provides reusable utilities for test fixtures that spawn brokers:
/// - Port allocation
/// - Socket/TCP readiness polling
/// - Stale file cleanup
/// - TestBroker / TestClient — single-room broker fixture
/// - TestDaemon — multi-room daemon fixture
/// - WebSocket helpers — ws_connect, ws_recv_json, ws_recv_until
/// - Daemon helpers — daemon_connect, daemon_join, daemon_send, daemon_create, daemon_destroy
/// - REST helpers — rest_join, rest_send
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use room_cli::{
    broker::{
        daemon::{DaemonConfig, DaemonState},
        Broker,
    },
    message::Message,
};
use room_protocol::RoomConfig;
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    time::timeout,
};
use tokio_tungstenite::{connect_async, tungstenite::Message as TungsteniteMsg};

// ── Port / socket utilities ───────────────────────────────────────────────────

/// Find a free ephemeral port by binding to port 0 and releasing.
pub fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Poll until a Unix socket file appears on disk, or panic after `timeout`.
pub async fn wait_for_socket(path: &Path, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    while !path.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "socket did not appear at {} within {:?}",
            path.display(),
            timeout
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Poll until a TCP connection to `port` succeeds, or panic after `timeout`.
pub async fn wait_for_tcp(port: u16, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "TCP port {port} not ready within {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Remove stale files from a previous test run. Ignores missing files.
pub fn cleanup_stale_files(paths: &[&str]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

// ── Single-room broker fixture ────────────────────────────────────────────────

pub struct TestBroker {
    pub socket_path: PathBuf,
    pub chat_path: PathBuf,
    /// Keep TempDir alive for the duration of the test.
    pub _dir: TempDir,
}

impl TestBroker {
    /// Start a broker and wait until the socket is ready.
    pub async fn start(room_id: &str) -> Self {
        Self::start_inner(room_id, None).await
    }

    /// Start a broker with both UDS and WebSocket/REST transport.
    /// Returns (TestBroker, ws_port).
    pub async fn start_with_ws(room_id: &str) -> (Self, u16) {
        let port = free_port();
        let broker = Self::start_inner(room_id, Some(port)).await;
        wait_for_tcp(port, Duration::from_secs(1)).await;
        (broker, port)
    }

    async fn start_inner(room_id: &str, ws_port: Option<u16>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join(format!("{room_id}.sock"));
        let chat_path = dir.path().join(format!("{room_id}.chat"));

        let broker = Broker::new(
            room_id,
            chat_path.clone(),
            chat_path.with_extension("tokens"),
            chat_path.with_extension("subscriptions"),
            socket_path.clone(),
            ws_port,
        );
        tokio::spawn(async move {
            broker.run().await.ok();
        });

        wait_for_socket(&socket_path, Duration::from_secs(1)).await;

        Self {
            socket_path,
            chat_path,
            _dir: dir,
        }
    }
}

pub struct TestClient {
    pub reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    pub writer: tokio::net::unix::OwnedWriteHalf,
}

impl TestClient {
    pub async fn connect(socket_path: &PathBuf, username: &str) -> Self {
        let stream = UnixStream::connect(socket_path)
            .await
            .expect("client could not connect to broker socket");
        let (r, mut w) = stream.into_split();
        w.write_all(format!("{username}\n").as_bytes())
            .await
            .unwrap();
        Self {
            reader: BufReader::new(r),
            writer: w,
        }
    }

    /// Read the next JSON line from the broker. Fails the test after 1 s.
    pub async fn recv(&mut self) -> Message {
        let mut line = String::new();
        timeout(Duration::from_secs(1), self.reader.read_line(&mut line))
            .await
            .expect("timed out waiting for message")
            .expect("read error");
        serde_json::from_str(line.trim()).expect("broker sent invalid JSON")
    }

    /// Drain messages until the predicate matches, or fail after 2 s.
    pub async fn recv_until<F: Fn(&Message) -> bool>(&mut self, pred: F) -> Message {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let remaining = deadline
                .checked_duration_since(tokio::time::Instant::now())
                .unwrap_or_default();
            if remaining.is_zero() {
                panic!("timed out waiting for expected message");
            }
            let mut line = String::new();
            timeout(remaining, self.reader.read_line(&mut line))
                .await
                .expect("timed out")
                .expect("read error");
            if let Ok(msg) = serde_json::from_str::<Message>(line.trim()) {
                if pred(&msg) {
                    return msg;
                }
            }
        }
    }

    /// Send a plain-text message.
    pub async fn send_text(&mut self, text: &str) {
        self.writer
            .write_all(format!("{text}\n").as_bytes())
            .await
            .unwrap();
    }

    /// Send a JSON envelope.
    pub async fn send_json(&mut self, json: &str) {
        self.writer
            .write_all(format!("{json}\n").as_bytes())
            .await
            .unwrap();
    }
}

// ── Multi-room daemon fixture ─────────────────────────────────────────────────

pub struct TestDaemon {
    pub socket_path: PathBuf,
    pub state: Arc<DaemonState>,
    pub _dir: TempDir,
}

impl TestDaemon {
    /// Start a daemon with configured rooms (room_id, optional RoomConfig).
    pub async fn start_with_configs(rooms: Vec<(&str, Option<RoomConfig>)>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("roomd.sock");

        let config = DaemonConfig {
            socket_path: socket_path.clone(),
            data_dir: dir.path().to_owned(),
            state_dir: dir.path().to_owned(),
            ws_port: None,
            grace_period_secs: 30,
        };

        let daemon = Arc::new(DaemonState::new(config));
        for (room_id, room_config) in rooms {
            match room_config {
                Some(cfg) => daemon.create_room_with_config(room_id, cfg).await.unwrap(),
                None => daemon.create_room(room_id).await.unwrap(),
            }
        }

        let daemon_run = daemon.clone();
        tokio::spawn(async move {
            daemon_run.run().await.ok();
        });

        wait_for_socket(&socket_path, Duration::from_secs(1)).await;

        Self {
            socket_path,
            state: daemon,
            _dir: dir,
        }
    }

    /// Start a daemon with WS/REST support and configured rooms.
    /// Returns (TestDaemon, ws_port).
    pub async fn start_with_ws_configs(rooms: Vec<(&str, Option<RoomConfig>)>) -> (Self, u16) {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("roomd.sock");
        let port = free_port();

        let config = DaemonConfig {
            socket_path: socket_path.clone(),
            data_dir: dir.path().to_owned(),
            state_dir: dir.path().to_owned(),
            ws_port: Some(port),
            grace_period_secs: 30,
        };

        let daemon = Arc::new(DaemonState::new(config));
        for (room_id, room_config) in rooms {
            match room_config {
                Some(cfg) => daemon.create_room_with_config(room_id, cfg).await.unwrap(),
                None => daemon.create_room(room_id).await.unwrap(),
            }
        }

        let daemon_run = daemon.clone();
        tokio::spawn(async move {
            daemon_run.run().await.ok();
        });

        wait_for_socket(&socket_path, Duration::from_secs(1)).await;
        wait_for_tcp(port, Duration::from_secs(1)).await;

        (
            Self {
                socket_path,
                state: daemon,
                _dir: dir,
            },
            port,
        )
    }

    pub async fn start(rooms: &[&str]) -> Self {
        let rooms_with_config: Vec<(&str, Option<RoomConfig>)> =
            rooms.iter().map(|id| (*id, None)).collect();
        Self::start_with_configs(rooms_with_config).await
    }
}

// ── Daemon protocol helpers ───────────────────────────────────────────────────

/// Connect to the daemon socket and perform a ROOM:-prefixed handshake.
pub async fn daemon_connect(
    socket_path: &PathBuf,
    room_id: &str,
    username: &str,
) -> (
    BufReader<tokio::net::unix::OwnedReadHalf>,
    tokio::net::unix::OwnedWriteHalf,
) {
    let stream = UnixStream::connect(socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(format!("ROOM:{room_id}:{username}\n").as_bytes())
        .await
        .unwrap();
    (BufReader::new(r), w)
}

/// One-shot join via the daemon protocol, returns the token UUID.
pub async fn daemon_join(socket_path: &PathBuf, room_id: &str, username: &str) -> String {
    let stream = UnixStream::connect(socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(format!("ROOM:{room_id}:JOIN:{username}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["type"], "token", "expected token response: {v}");
    v["token"].as_str().unwrap().to_owned()
}

/// One-shot send via the daemon protocol, returns the broadcast JSON.
pub async fn daemon_send(
    socket_path: &PathBuf,
    room_id: &str,
    token: &str,
    content: &str,
) -> serde_json::Value {
    let stream = UnixStream::connect(socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(format!("ROOM:{room_id}:TOKEN:{token}\n").as_bytes())
        .await
        .unwrap();
    w.write_all(format!("{content}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    serde_json::from_str(line.trim()).unwrap()
}

/// Send a CREATE:<room_id> request to the daemon socket, returns the response JSON.
pub async fn daemon_create(
    socket_path: &PathBuf,
    room_id: &str,
    config_json: &str,
) -> serde_json::Value {
    let stream = UnixStream::connect(socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(format!("CREATE:{room_id}\n").as_bytes())
        .await
        .unwrap();
    w.write_all(format!("{config_json}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    serde_json::from_str(line.trim()).unwrap()
}

/// Send a DESTROY:<room_id> request to the daemon socket, returns the response JSON.
pub async fn daemon_destroy(socket_path: &PathBuf, room_id: &str) -> serde_json::Value {
    let stream = UnixStream::connect(socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(format!("DESTROY:{room_id}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    serde_json::from_str(line.trim()).unwrap()
}

// ── WebSocket helpers ─────────────────────────────────────────────────────────

pub type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    TungsteniteMsg,
>;

pub type WsStream = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

/// Connect a WebSocket client and send the first handshake frame.
/// Returns the split (sink, stream).
pub async fn ws_connect(port: u16, room_id: &str, first_frame: &str) -> (WsSink, WsStream) {
    let url = format!("ws://127.0.0.1:{port}/ws/{room_id}");
    let (ws, _) = connect_async(&url).await.expect("WS connect failed");
    let (mut tx, rx) = ws.split();
    tx.send(TungsteniteMsg::Text(first_frame.into()))
        .await
        .unwrap();
    (tx, rx)
}

/// Read the next text frame as raw JSON Value.
pub async fn ws_recv_json(rx: &mut WsStream) -> serde_json::Value {
    let deadline = Duration::from_secs(2);
    match timeout(deadline, rx.next()).await {
        Ok(Some(Ok(TungsteniteMsg::Text(text)))) => {
            serde_json::from_str(&text).expect("WS broker sent invalid JSON value")
        }
        Ok(Some(Ok(other))) => panic!("unexpected WS frame: {other:?}"),
        Ok(Some(Err(e))) => panic!("WS read error: {e}"),
        Ok(None) => panic!("WS stream ended unexpectedly"),
        Err(_) => panic!("timed out waiting for WS message"),
    }
}

/// Drain WS frames until predicate matches a Message, or panic after 2s.
pub async fn ws_recv_until<F: Fn(&Message) -> bool>(rx: &mut WsStream, pred: F) -> Message {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or_default();
        if remaining.is_zero() {
            panic!("timed out waiting for expected WS message");
        }
        match timeout(remaining, rx.next()).await {
            Ok(Some(Ok(TungsteniteMsg::Text(text)))) => {
                if let Ok(msg) = serde_json::from_str::<Message>(&text) {
                    if pred(&msg) {
                        return msg;
                    }
                }
            }
            Ok(Some(Ok(_))) => continue,
            _ => panic!("WS stream ended or errored while waiting for message"),
        }
    }
}

// ── REST helpers ──────────────────────────────────────────────────────────────

/// Join a room via REST and return the token string.
pub async fn rest_join(
    client: &reqwest::Client,
    base: &str,
    room_id: &str,
    username: &str,
) -> String {
    let resp = client
        .post(format!("{base}/api/{room_id}/join"))
        .json(&serde_json::json!({"username": username}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "rest_join failed for {username}");
    let body: serde_json::Value = resp.json().await.unwrap();
    body["token"].as_str().unwrap().to_owned()
}

/// Send a message via REST and return the response body.
pub async fn rest_send(
    client: &reqwest::Client,
    base: &str,
    room_id: &str,
    token: &str,
    content: &str,
) -> serde_json::Value {
    let resp = client
        .post(format!("{base}/api/{room_id}/send"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"content": content}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "rest_send failed for content={content}");
    resp.json().await.unwrap()
}
