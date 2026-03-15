mod cli;
mod cmd_daemon;

use std::path::PathBuf;

use chrono::DateTime;
use clap::Parser;
use regex::Regex;
use room_cli::{
    client::Client,
    message::parse_message_id,
    oneshot::{self, QueryOptions},
    paths,
    query::{has_narrowing_filter, QueryFilter},
};

use cli::{AgentAction, Args, Cmd, PluginAction};

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
            events,
            socket,
        }) => {
            if socket.is_none() {
                oneshot::ensure_daemon_running().await?;
            }
            oneshot::cmd_subscribe(
                &room_id,
                &token,
                &tier,
                events.as_deref(),
                socket.as_deref(),
            )
            .await?;
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
        Some(Cmd::Agent {
            room_id,
            token,
            socket,
            action,
        }) => {
            if socket.is_none() {
                oneshot::ensure_daemon_running().await?;
            }
            match action {
                AgentAction::Spawn {
                    username,
                    model,
                    issue,
                    personality,
                } => {
                    oneshot::cmd_agent_spawn(
                        &room_id,
                        &token,
                        &username,
                        model.as_deref(),
                        issue.as_deref(),
                        personality.as_deref(),
                        socket.as_deref(),
                    )
                    .await?;
                }
                AgentAction::List => {
                    oneshot::cmd_agent_list(&room_id, &token, socket.as_deref()).await?;
                }
                AgentAction::Stop { username } => {
                    oneshot::cmd_agent_stop(&room_id, &token, &username, socket.as_deref()).await?;
                }
                AgentAction::Logs { username, tail } => {
                    oneshot::cmd_agent_logs(&room_id, &token, &username, tail, socket.as_deref())
                        .await?;
                }
            }
        }
        Some(Cmd::Plugin { action }) => {
            let plugins_dir = paths::room_plugins_dir();
            match action {
                PluginAction::Install { name, version } => {
                    room_cli::plugin_cmd::cmd_install(&plugins_dir, &name, version.as_deref())?;
                }
                PluginAction::List => {
                    room_cli::plugin_cmd::cmd_list(&plugins_dir)?;
                }
                PluginAction::Remove { name } => {
                    room_cli::plugin_cmd::cmd_remove(&plugins_dir, &name)?;
                }
                PluginAction::Update { name, version } => {
                    room_cli::plugin_cmd::cmd_update(&plugins_dir, &name, version.as_deref())?;
                }
            }
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

            cmd_daemon::run_daemon(
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
    // If no token exists yet, auto-join to register the user first.
    let token_val = {
        let path = paths::global_token_path(&username);
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|data| serde_json::from_str::<serde_json::Value>(data.trim()).ok())
            .and_then(|v| v["token"].as_str().map(|s| s.to_owned()))
    };
    let token_val = match token_val {
        Some(t) => t,
        None => {
            let (_, token) =
                room_cli::oneshot::transport::global_join_session(&daemon_socket, &username)
                    .await?;
            let token_data = serde_json::json!({"username": &username, "token": &token});
            let token_path = paths::global_token_path(&username);
            std::fs::write(&token_path, format!("{token_data}\n"))?;
            token
        }
    };
    let config_json = room_cli::oneshot::transport::inject_token_into_config(
        r#"{"visibility":"public"}"#,
        &token_val,
    );

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
