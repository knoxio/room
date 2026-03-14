use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Subcommand, Debug)]
pub enum Cmd {
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
    ///
    /// Use `--events` to filter which event types you receive (default: all).
    /// Example: `room subscribe myroom -t TOKEN full --events task_posted,task_finished`
    Subscribe {
        room_id: String,
        /// Session token from `room join` (required)
        #[arg(short = 't', long)]
        token: String,
        /// Subscription tier: `full` or `mentions_only` (default: full)
        #[arg(default_value = "full")]
        tier: String,
        /// Event type filter: `all` (default), `none`, or comma-separated event types.
        /// Valid types: task_posted, task_assigned, task_claimed, task_planned,
        /// task_approved, task_updated, task_released, task_finished, task_cancelled,
        /// status_changed, review_requested.
        #[arg(long)]
        events: Option<String>,
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
    /// Manage ralph agents via CLI (spawn, list, stop, logs).
    ///
    /// Routes commands through the broker to the AgentPlugin. Requires a running
    /// daemon with the AgentPlugin registered.
    Agent {
        /// Room ID where agents are managed
        room_id: String,
        /// Session token from `room join` (required)
        #[arg(short = 't', long)]
        token: String,
        /// Override the broker socket path (default: auto-discover daemon or per-room socket)
        #[arg(long)]
        socket: Option<PathBuf>,
        #[command(subcommand)]
        action: AgentAction,
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

/// Agent management subcommands.
#[derive(Subcommand, Debug)]
pub enum AgentAction {
    /// Spawn a new ralph agent in the room.
    Spawn {
        /// Username for the spawned agent
        username: String,
        /// Claude model to use (e.g. sonnet, opus, haiku)
        #[arg(long)]
        model: Option<String>,
        /// GitHub issue number for the agent to work on
        #[arg(long)]
        issue: Option<String>,
        /// Personality name for the agent
        #[arg(long)]
        personality: Option<String>,
    },
    /// List all agents in the room with their status.
    List,
    /// Stop a running agent.
    Stop {
        /// Username of the agent to stop
        username: String,
    },
    /// View logs for an agent.
    Logs {
        /// Username of the agent
        username: String,
        /// Number of lines to show from the end (default: 50)
        #[arg(long, default_value_t = 50)]
        tail: usize,
    },
}

#[derive(Parser, Debug)]
#[command(
    name = "room",
    version,
    disable_version_flag = true,
    about = "Multi-user chat room for agent/human coordination"
)]
pub struct Args {
    /// Print version and exit
    #[arg(short = 'v', long = "version", action = clap::ArgAction::Version)]
    pub _version: (),

    /// Room identifier (required when no subcommand is given)
    pub room_id: Option<String>,

    /// Your username (defaults to $USER when no subcommand is given)
    pub username: Option<String>,

    /// Number of history messages to replay on join
    #[arg(short = 'n', default_value_t = 20)]
    pub history_lines: usize,

    /// Chat file path (only used when creating a new room)
    #[arg(short = 'f')]
    pub chat_file: Option<PathBuf>,

    /// Non-interactive agent mode: read JSON from stdin, write JSON to stdout
    #[arg(long)]
    pub agent: bool,

    /// Enable WebSocket/REST transport on this port (e.g. --ws-port 4200)
    #[arg(long)]
    pub ws_port: Option<u16>,

    #[command(subcommand)]
    pub command: Option<Cmd>,
}
