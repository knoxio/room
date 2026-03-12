use std::path::PathBuf;

use chrono::DateTime;
use clap::{Parser, Subcommand};
use regex::Regex;
use room_cli::{
    broker::daemon::{is_pid_alive, DaemonConfig, DaemonState},
    client::Client,
    message::parse_message_id,
    oneshot::{self, QueryOptions},
    paths,
    query::{has_narrowing_filter, QueryFilter},
};

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Register a username with the daemon and receive a global session token.
    ///
    /// Writes the token to `~/.room/state/room-<username>.token`. The token is
    /// global — use `room subscribe <room>` to join specific rooms.
    /// Returns the existing token if the username is already registered.
    Join {
        username: String,
        /// Override the broker socket path (default: auto-discover daemon or per-room socket)
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// One-shot send a message to a room (requires a running broker).
    ///
    /// The broker resolves the sender's identity from the token issued by `room join`.
    /// Prints the broadcast message JSON and exits.
    Send {
        room_id: String,
        /// Session token from `room join` (required)
        #[arg(short = 't', long)]
        token: String,
        /// Recipient username for a direct message
        #[arg(long)]
        to: Option<String>,
        /// Override the broker socket path (default: auto-discover daemon or per-room socket)
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Message content; all remaining tokens are joined with spaces
        #[arg(trailing_var_arg = true, num_args = 1..)]
        message: Vec<String>,
    },
    /// Query message history with optional filters.
    ///
    /// Without flags, returns all messages (newest-first). Use `--new` to return
    /// only messages since the last poll (advancing the cursor). Use `--wait` to
    /// block until at least one new foreign message arrives.
    ///
    /// Flags compose freely: `-r dev -n 20 --user alice --new` returns the 20 most
    /// recent messages from alice in the dev room that arrived since your last poll.
    Query {
        /// Single room ID (omit when using -r/--room or --all)
        room_id: Option<String>,
        /// Session token from `room join` (required)
        #[arg(short = 't', long)]
        token: String,
        /// Filter by room IDs — comma-separated or repeated (overrides positional room_id)
        #[arg(short = 'r', long = "room", value_delimiter = ',')]
        rooms: Vec<String>,
        /// Query all daemon-managed rooms (auto-discovered). Implicit when --new or --wait
        /// is used without -r.
        #[arg(long)]
        all: bool,
        /// Only include messages sent by this user
        #[arg(long)]
        user: Option<String>,
        /// Only messages after this position — format `<room>:<seq>` (exclusive)
        #[arg(long)]
        from: Option<String>,
        /// Only messages at or before this position — format `<room>:<seq>` (inclusive)
        #[arg(long)]
        to: Option<String>,
        /// Only messages after this timestamp (ISO 8601, e.g. `2026-03-01T00:00:00Z`)
        #[arg(long)]
        since: Option<String>,
        /// Only messages before this timestamp (ISO 8601)
        #[arg(long)]
        until: Option<String>,
        /// Limit output to N messages
        #[arg(short = 'n')]
        count: Option<usize>,
        /// Only messages that @mention the caller
        #[arg(short = 'm', long = "mentions-only")]
        mentions_only: bool,
        /// Substring content search (case-sensitive)
        #[arg(short = 's', long = "search")]
        search: Option<String>,
        /// Regex content search
        #[arg(long)]
        regex: Option<String>,
        /// Return only new messages since last poll (advances cursor)
        #[arg(long)]
        new: bool,
        /// Block until at least one new message arrives (implies --new)
        #[arg(long)]
        wait: bool,
        /// Sort oldest-first (default when --new is used)
        #[arg(long, conflicts_with = "desc")]
        asc: bool,
        /// Sort newest-first (default for history queries)
        #[arg(long, conflicts_with = "asc")]
        desc: bool,
        /// Poll interval in seconds when --wait is used (default: 5)
        #[arg(long, default_value_t = 5)]
        interval: u64,
        /// Bypass subscription filter — query any room regardless of subscription.
        ///
        /// Must be combined with at least one narrowing filter (-n, -r, --user, --from,
        /// --to, --since, --until, --id, --all, --new, --wait, -s, --regex, or -m).
        /// DM privacy is still enforced regardless of this flag.
        #[arg(short = 'p', long = "public")]
        public: bool,
        /// Look up a single message by ID — format `<room>:<seq>` (e.g. `dev:42`).
        ///
        /// Returns the message with that exact sequence number, or an error if not found.
        #[arg(long)]
        id: Option<String>,
    },
    /// Poll for new messages — alias for `room query --new`.
    ///
    /// Updates a per-user cursor file so subsequent calls return only unseen messages.
    /// Use `--rooms r1,r2` (comma-separated or repeated) to poll multiple rooms at once;
    /// messages are merged by timestamp and each carries its `room` field.
    Poll {
        /// Single room ID (omit when using --rooms)
        room_id: Option<String>,
        /// Session token from `room join` (required)
        #[arg(short = 't', long)]
        token: String,
        /// Return only messages after this message ID (overrides stored cursor; single-room only)
        #[arg(long)]
        since: Option<String>,
        /// Poll multiple rooms (comma-separated or repeated). Merges messages by timestamp.
        #[arg(long, value_delimiter = ',')]
        rooms: Vec<String>,
        /// Only return messages that @mention the caller's username
        #[arg(long)]
        mentions_only: bool,
    },
    /// Fetch the last N messages from history without updating the poll cursor.
    ///
    /// Reads the NDJSON chat file directly — no broker connection required.
    /// Useful for agents that need to re-read recent context after a context reset.
    Pull {
        room_id: String,
        /// Session token from `room join` (required)
        #[arg(short = 't', long)]
        token: String,
        /// Number of messages to return (default: 20, max: 200)
        #[arg(short = 'n', default_value_t = 20)]
        count: usize,
    },
    /// Watch for new messages — alias for `room query --new --wait`.
    ///
    /// Polls the chat file on a configurable interval. Shares the cursor file with
    /// `room poll` and `room query --new` so no messages are re-delivered. Exits
    /// after printing the first batch of foreign messages as NDJSON.
    ///
    /// When no room ID or `--rooms` is given, watches all rooms on the daemon.
    Watch {
        /// Single room ID (omit to watch all daemon rooms)
        room_id: Option<String>,
        /// Session token from `room join` (required)
        #[arg(short = 't', long)]
        token: String,
        /// Watch multiple rooms (comma-separated or repeated). Merges messages by timestamp.
        #[arg(long, value_delimiter = ',')]
        rooms: Vec<String>,
        /// Poll interval in seconds (default: 5)
        #[arg(long, default_value_t = 5)]
        interval: u64,
    },
    /// Set your subscription tier for a room.
    ///
    /// Sends `/subscribe [tier]` to the broker and prints the broker confirmation.
    /// Valid tiers: `full` (default, receive all messages) or `mentions_only`
    /// (receive only messages that @mention you).
    Subscribe {
        room_id: String,
        /// Session token from `room join` (required)
        #[arg(short = 't', long)]
        token: String,
        /// Subscription tier: `full` or `mentions_only` (default: full)
        #[arg(default_value = "full")]
        tier: String,
        /// Override the broker socket path (default: auto-discover daemon or per-room socket)
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Query who is online and their status.
    ///
    /// Sends `/who` to the broker and prints the member list with statuses.
    /// With `--json`, prints the raw JSON response instead of human-readable text.
    Who {
        room_id: String,
        /// Session token from `room join` (required)
        #[arg(short = 't', long)]
        token: String,
        /// Print raw JSON instead of human-readable text
        #[arg(long)]
        json: bool,
        /// Override the broker socket path (default: auto-discover daemon or per-room socket)
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Send a direct message to a user, creating the DM room if needed.
    ///
    /// Computes the canonical DM room ID (`dm-<sorted_a>-<sorted_b>`) and sends
    /// the message. The caller's username is resolved from the token.
    /// Prints the broadcast message JSON and exits.
    Dm {
        /// Recipient username
        user: String,
        /// Session token from `room join` (required)
        #[arg(short = 't', long)]
        token: String,
        /// Override the broker socket path (default: auto-discover daemon or per-room socket)
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Message content; all remaining tokens are joined with spaces
        #[arg(trailing_var_arg = true, num_args = 1..)]
        message: Vec<String>,
    },
    /// Create a room in a running daemon.
    ///
    /// Connects to the daemon socket and requests room creation. The room is
    /// immediately available for `join`, `send`, and `poll`.
    Create {
        /// Room ID to create
        room_id: String,
        /// Session token from `room join` (required)
        #[arg(short = 't', long)]
        token: String,
        /// Override the daemon socket path (default: auto-discover)
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Room visibility: public, private, or dm (default: public)
        #[arg(long, default_value = "public")]
        visibility: String,
        /// Invite list — usernames allowed to join (comma-separated or repeated).
        /// Required for dm visibility (exactly 2 users).
        #[arg(long, value_delimiter = ',')]
        invite: Vec<String>,
    },
    /// Destroy a room in a running daemon.
    ///
    /// Signals shutdown to all connected clients and removes the room from the
    /// daemon's map. Chat files are preserved on disk.
    Destroy {
        /// Room ID to destroy
        room_id: String,
        /// Session token from `room join` (required)
        #[arg(short = 't', long)]
        token: String,
        /// Override the daemon socket path (default: auto-discover)
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// List active rooms with running brokers.
    ///
    /// Scans `/tmp` for `room-*.sock` files and probes each to verify the broker
    /// is alive. Prints one NDJSON line per active room. No token required.
    List,
    /// Start a multi-room daemon that manages N rooms in a single process.
    ///
    /// Listens on a single UDS socket (default: platform-native temp dir) and
    /// dispatches connections to rooms based on the `ROOM:<room_id>:` handshake prefix.
    /// Rooms can be created dynamically via `room create` or the REST API.
    Daemon {
        /// Path to the daemon UDS socket (default: $TMPDIR/roomd.sock on macOS,
        /// $XDG_RUNTIME_DIR/room/roomd.sock on Linux)
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Directory for chat files (default: ~/.room/data/)
        #[arg(long)]
        data_dir: Option<PathBuf>,
        /// Directory for state files — tokens, cursors (default: ~/.room/state/)
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Enable WebSocket/REST transport on this port
        #[arg(long)]
        ws_port: Option<u16>,
        /// Room IDs to create on startup (can be repeated)
        #[arg(long = "room")]
        rooms: Vec<String>,
        /// Seconds to wait after the last connection closes before shutting down.
        /// Set to 0 for immediate shutdown. Default: 30.
        /// Ignored when --persistent is set.
        #[arg(long, default_value_t = 30)]
        grace_period: u64,
        /// Keep the daemon running indefinitely after the last connection closes.
        /// Equivalent to --grace-period with the maximum possible value.
        /// Mutually exclusive with --grace-period.
        #[arg(long, conflicts_with = "grace_period")]
        persistent: bool,
        /// Start an isolated daemon in a private temp directory for testing.
        ///
        /// When set, the daemon:
        /// - Creates a temporary directory and uses it for all state (socket, data, tokens).
        /// - Does NOT touch the shared PID file or well-known socket path.
        /// - Prints connection info to stdout before starting:
        ///   `{"socket":"/tmp/.room-isolated-XXXX/roomd.sock","pid":12345}`
        /// - Cleans up the temp directory on exit.
        ///
        /// Callers pass the printed socket path via `--socket` or `ROOM_SOCKET=<path>`
        /// to subsequent commands to target the isolated instance.
        #[arg(long)]
        isolated: bool,
    },
}

#[derive(Parser, Debug)]
#[command(
    name = "room",
    version,
    disable_version_flag = true,
    about = "Multi-user chat room for agent/human coordination"
)]
struct Args {
    /// Print version and exit
    #[arg(short = 'v', long = "version", action = clap::ArgAction::Version)]
    _version: (),

    /// Room identifier (required when no subcommand is given)
    room_id: Option<String>,

    /// Your username (defaults to $USER when no subcommand is given)
    username: Option<String>,

    /// Number of history messages to replay on join
    #[arg(short = 'n', default_value_t = 20)]
    history_lines: usize,

    /// Chat file path (only used when creating a new room)
    #[arg(short = 'f')]
    chat_file: Option<PathBuf>,

    /// Non-interactive agent mode: read JSON from stdin, write JSON to stdout
    #[arg(long)]
    agent: bool,

    /// Enable WebSocket/REST transport on this port (e.g. --ws-port 4200)
    #[arg(long)]
    ws_port: Option<u16>,

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    match args.command {
        Some(Cmd::Join { username, socket }) => {
            // Auto-start the daemon for commands that require a live broker
            // connection (join, send, who, dm). Read-only commands (poll, pull,
            // watch, query) read the chat file directly and work without a running
            // daemon, so they do not trigger auto-start.
            if socket.is_none() {
                oneshot::ensure_daemon_running().await?;
            }
            oneshot::cmd_join(&username, socket.as_deref()).await?;
        }
        Some(Cmd::Send {
            room_id,
            token,
            to,
            socket,
            message,
        }) => {
            if socket.is_none() {
                oneshot::ensure_daemon_running().await?;
            }
            let content = message.join(" ");
            oneshot::cmd_send(&room_id, &token, to.as_deref(), &content, socket.as_deref()).await?;
        }
        Some(Cmd::Query {
            room_id,
            token,
            rooms,
            all,
            user,
            from,
            to,
            since,
            until,
            count,
            mentions_only,
            search,
            regex,
            new,
            wait,
            asc,
            desc,
            interval,
            public,
            id,
        }) => {
            // Resolve the effective room list.
            // --all or implicit --all (--new/--wait without -r) auto-discovers.
            let use_all = all || ((new || wait) && rooms.is_empty() && room_id.is_none());
            let effective_rooms: Vec<String> = if !rooms.is_empty() {
                if room_id.is_some() {
                    anyhow::bail!(
                        "cannot specify both positional room_id and -r/--room; use one or the other"
                    );
                }
                rooms
            } else if let Some(id) = room_id {
                vec![id]
            } else if use_all {
                let username = oneshot::username_from_token(&token)?;
                let discovered = oneshot::discover_joined_rooms(&username);
                if discovered.is_empty() {
                    anyhow::bail!(
                        "no rooms found — ensure the daemon is running and you have joined at least one room"
                    );
                }
                discovered
            } else {
                anyhow::bail!(
                    "room_id is required — pass it as a positional argument, use -r/--room, or --all"
                );
            };

            // Parse --from / --to as room:seq.
            let after_seq = from
                .as_deref()
                .map(|s| {
                    parse_message_id(s).map_err(|e| anyhow::anyhow!("invalid --from value: {e}"))
                })
                .transpose()?;

            let before_seq = to
                .as_deref()
                .map(|s| {
                    parse_message_id(s)
                        // --to is inclusive: convert to exclusive by adding 1.
                        .map(|(room, seq)| (room, seq.saturating_add(1)))
                        .map_err(|e| anyhow::anyhow!("invalid --to value: {e}"))
                })
                .transpose()?;

            // Parse --since / --until as ISO 8601 timestamps.
            let after_ts = since
                .as_deref()
                .map(|s| {
                    DateTime::parse_from_rfc3339(s)
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                        .map_err(|e| {
                            anyhow::anyhow!("invalid --since value (expected ISO 8601): {e}")
                        })
                })
                .transpose()?;

            let before_ts = until
                .as_deref()
                .map(|s| {
                    DateTime::parse_from_rfc3339(s)
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                        .map_err(|e| {
                            anyhow::anyhow!("invalid --until value (expected ISO 8601): {e}")
                        })
                })
                .transpose()?;

            // Parse --id as room:seq.
            let target_id = id
                .as_deref()
                .map(|s| {
                    parse_message_id(s).map_err(|e| anyhow::anyhow!("invalid --id value: {e}"))
                })
                .transpose()?;

            // Default sort: ascending when --new/--wait, descending otherwise.
            let ascending = if asc {
                true
            } else if desc {
                false
            } else {
                new || wait
            };

            let compiled_regex = regex
                .map(|pat| {
                    Regex::new(&pat).map_err(|e| anyhow::anyhow!("invalid --regex pattern: {e}"))
                })
                .transpose()?;

            let filter = QueryFilter {
                rooms: effective_rooms.clone(),
                users: user.map(|u| vec![u]).unwrap_or_default(),
                content_search: search,
                content_regex: compiled_regex,
                after_seq,
                before_seq,
                after_ts,
                before_ts,
                limit: count,
                ascending,
                public_only: public,
                target_id,
                ..Default::default()
            };

            // -p/--public requires at least one narrowing filter.
            if public && !has_narrowing_filter(&filter, new || wait) {
                anyhow::bail!(
                    "-p/--public requires at least one narrowing filter (-n, -r, --user, \
                     --from, --to, --since, --until, --id, --all, --new, --wait, -s, --regex, or -m)"
                );
            }

            let opts = QueryOptions {
                new_only: new || wait,
                wait,
                interval_secs: interval,
                mentions_only,
                since_uuid: None,
            };

            oneshot::cmd_query(&effective_rooms, &token, filter, opts).await?;
        }
        Some(Cmd::Poll {
            room_id,
            token,
            since,
            rooms,
            mentions_only,
        }) => {
            // Alias for `room query --new`. Delegates to cmd_query.
            let effective_rooms: Vec<String> = if !rooms.is_empty() {
                if since.is_some() {
                    anyhow::bail!("--since is not supported with --rooms (use per-room cursors)");
                }
                rooms
            } else if let Some(id) = room_id {
                vec![id]
            } else {
                // Auto-discover rooms the user has joined.
                let username = oneshot::username_from_token(&token)?;
                let discovered = oneshot::discover_joined_rooms(&username);
                if discovered.is_empty() {
                    anyhow::bail!(
                        "no rooms found — specify a room ID, use --rooms, or ensure the daemon is running and you have joined at least one room"
                    );
                }
                discovered
            };

            let filter = QueryFilter {
                rooms: effective_rooms.clone(),
                ..Default::default()
            };
            let opts = QueryOptions {
                new_only: true,
                wait: false,
                interval_secs: 5,
                mentions_only,
                since_uuid: since,
            };
            oneshot::cmd_query(&effective_rooms, &token, filter, opts).await?;
        }
        Some(Cmd::Pull {
            room_id,
            token,
            count,
        }) => {
            oneshot::cmd_pull(&room_id, &token, count).await?;
        }
        Some(Cmd::Watch {
            room_id,
            token,
            rooms,
            interval,
        }) => {
            // Alias for `room query --new --wait`. Delegates to cmd_query.
            let effective_rooms: Vec<String> = if !rooms.is_empty() {
                rooms
            } else if let Some(id) = room_id {
                vec![id]
            } else {
                // Auto-discover rooms the user has joined.
                let username = oneshot::username_from_token(&token)?;
                let discovered = oneshot::discover_joined_rooms(&username);
                if discovered.is_empty() {
                    anyhow::bail!(
                        "no rooms found — specify a room ID, use --rooms, or ensure the daemon is running and you have joined at least one room"
                    );
                }
                discovered
            };
            let filter = QueryFilter {
                rooms: effective_rooms.clone(),
                ascending: true,
                ..Default::default()
            };
            let opts = QueryOptions {
                new_only: true,
                wait: true,
                interval_secs: interval,
                mentions_only: false,
                since_uuid: None,
            };
            oneshot::cmd_query(&effective_rooms, &token, filter, opts).await?;
        }
        Some(Cmd::Subscribe {
            room_id,
            token,
            tier,
            socket,
        }) => {
            if socket.is_none() {
                oneshot::ensure_daemon_running().await?;
            }
            oneshot::cmd_subscribe(&room_id, &token, &tier, socket.as_deref()).await?;
        }
        Some(Cmd::Who {
            room_id,
            token,
            json,
            socket,
        }) => {
            if socket.is_none() {
                oneshot::ensure_daemon_running().await?;
            }
            oneshot::cmd_who(&room_id, &token, json, socket.as_deref()).await?;
        }
        Some(Cmd::Dm {
            user,
            token,
            socket,
            message,
        }) => {
            if socket.is_none() {
                oneshot::ensure_daemon_running().await?;
            }
            let content = message.join(" ");
            oneshot::cmd_dm(&user, &token, &content, socket.as_deref()).await?;
        }
        Some(Cmd::Create {
            room_id,
            token,
            socket,
            visibility,
            invite,
        }) => {
            oneshot::cmd_create(&room_id, socket.as_deref(), &visibility, &invite, &token).await?;
        }
        Some(Cmd::Destroy {
            room_id,
            token,
            socket,
        }) => {
            oneshot::cmd_destroy(&room_id, socket.as_deref(), &token).await?;
        }
        Some(Cmd::List) => {
            oneshot::cmd_list().await?;
        }
        Some(Cmd::Daemon {
            socket,
            data_dir,
            state_dir,
            ws_port,
            rooms,
            grace_period,
            persistent,
            isolated,
        }) => {
            let effective_grace = if persistent { u64::MAX } else { grace_period };

            // When --isolated: create a private temp dir for all state.
            // `_isolated_tmp` is kept alive until run_daemon returns, then dropped
            // (which deletes the temp directory and the socket inside it).
            let _isolated_tmp: Option<tempfile::TempDir>;
            let (effective_socket, effective_data, effective_state) = if isolated {
                let tmp = tempfile::Builder::new()
                    .prefix(".room-isolated-")
                    .tempdir()
                    .map_err(|e| anyhow::anyhow!("--isolated: failed to create temp dir: {e}"))?;
                let sock = tmp.path().join("roomd.sock");
                let data = tmp.path().join("data");
                let state_d = tmp.path().join("state");
                std::fs::create_dir_all(&data)?;
                std::fs::create_dir_all(&state_d)?;
                // Print connection info before blocking in run_daemon so the caller
                // can read the socket path from stdout.
                println!(
                    "{}",
                    serde_json::json!({
                        "socket": sock.to_string_lossy(),
                        "pid": std::process::id()
                    })
                );
                _isolated_tmp = Some(tmp);
                (sock, data, state_d)
            } else {
                _isolated_tmp = None;
                // Resolution order: --socket flag > ROOM_SOCKET env > platform default.
                (
                    paths::effective_socket_path(socket.as_deref()),
                    data_dir.unwrap_or_else(paths::room_data_dir),
                    state_dir.unwrap_or_else(paths::room_state_dir),
                )
            };

            run_daemon(
                effective_socket,
                effective_data,
                effective_state,
                ws_port,
                rooms,
                effective_grace,
            )
            .await?;
        }
        None => {
            let room_id = args.room_id.unwrap_or_else(|| {
                eprintln!("error: room_id is required when no subcommand is given");
                std::process::exit(1);
            });
            let username = args
                .username
                .or_else(room_cli::client::default_username)
                .unwrap_or_else(|| {
                    eprintln!("error: username is required — set $USER or pass it as an argument");
                    std::process::exit(1);
                });
            run_join(
                room_id,
                username,
                args.history_lines,
                args.chat_file,
                args.agent,
                args.ws_port,
            )
            .await?;
        }
    }

    Ok(())
}

async fn run_join(
    room_id: String,
    username: String,
    history_lines: usize,
    _chat_file: Option<PathBuf>,
    agent: bool,
    _ws_port: Option<u16>,
) -> anyhow::Result<()> {
    paths::ensure_room_dirs().map_err(|e| anyhow::anyhow!("cannot create ~/.room dirs: {e}"))?;

    // All rooms go through the daemon. Auto-start it if not running.
    room_cli::oneshot::transport::ensure_daemon_running().await?;
    let daemon_socket = paths::effective_socket_path(None);

    // Read the user's token so we can authenticate the CREATE request.
    // The token file is written by `room join <username>` or by Client::ensure_token.
    let token_val = {
        let path = paths::global_token_path(&username);
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|data| serde_json::from_str::<serde_json::Value>(data.trim()).ok())
            .and_then(|v| v["token"].as_str().map(|s| s.to_owned()))
    };
    let config_json = match token_val {
        Some(tok) => room_cli::oneshot::transport::inject_token_into_config(
            r#"{"visibility":"public"}"#,
            &tok,
        ),
        None => r#"{"visibility":"public"}"#.to_owned(),
    };

    // Create the room on the daemon (ignore "already exists").
    match room_cli::oneshot::transport::create_room(&daemon_socket, &room_id, &config_json).await {
        Ok(_) => eprintln!("[room] created room '{room_id}' on daemon"),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("already exists") || msg.contains("room_exists") {
                eprintln!("[room] room '{room_id}' already exists on daemon");
            } else {
                return Err(e);
            }
        }
    }

    eprintln!("[room] connecting to room '{room_id}' via daemon");
    let client = Client {
        socket_path: daemon_socket,
        room_id,
        username,
        agent_mode: agent,
        history_lines,
        daemon_mode: true,
    };
    client.run().await?;

    if agent {
        tokio::signal::ctrl_c().await.ok();
    }

    Ok(())
}

async fn run_daemon(
    socket: PathBuf,
    data_dir: PathBuf,
    state_dir: PathBuf,
    ws_port: Option<u16>,
    rooms: Vec<String>,
    grace_period_secs: u64,
) -> anyhow::Result<()> {
    paths::ensure_room_dirs().map_err(|e| anyhow::anyhow!("cannot create ~/.room dirs: {e}"))?;

    // Stale PID check: guard against starting a second daemon over the system
    // daemon.  Only applies when using the default socket path — daemons with
    // explicit socket overrides (tests, CI, non-system instances) are independent
    // and must not interfere with the system PID file.
    //
    // We also skip the check when the PID file already holds our own PID.  That
    // happens when `ensure_daemon_running` wrote our PID before we started (the
    // auto-start path) — in that case there is no conflict.
    if socket == paths::room_socket_path() {
        let pid_path = paths::room_pid_path();
        if pid_path.exists() {
            let file_pid = std::fs::read_to_string(&pid_path)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok());
            let is_own = file_pid == Some(std::process::id());
            if !is_own {
                if is_pid_alive(&pid_path) {
                    anyhow::bail!(
                        "daemon already running (PID file: {}). \
                         Stop the existing daemon or remove the PID file manually.",
                        pid_path.display()
                    );
                }
                eprintln!(
                    "[daemon] stale PID file at {} (process gone), cleaning up",
                    pid_path.display()
                );
                let _ = std::fs::remove_file(&pid_path);
            }
        }
    }

    let config = DaemonConfig {
        socket_path: socket,
        data_dir,
        state_dir,
        ws_port,
        grace_period_secs,
    };

    let daemon = DaemonState::new(config);

    // Create initial rooms.
    for room_id in &rooms {
        match daemon.create_room(room_id).await {
            Ok(_) => eprintln!("[daemon] created room: {room_id}"),
            Err(e) => eprintln!("[daemon] failed to create room {room_id}: {e}"),
        }
    }

    // Set up signal handling for graceful shutdown.
    let shutdown = daemon.shutdown_handle();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        eprintln!("[daemon] caught SIGINT, shutting down");
        let _ = shutdown.send(true);
    });

    daemon.run().await
}
