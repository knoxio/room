use room_protocol::make_system;
use tokio::sync::Mutex;

use crate::broker::{fanout::broadcast_and_persist, state::RoomState};
use crate::registry::UserRegistry;

use super::CommandResult;

/// Operation-specific text for [`team_mutate`].
struct MutateLabels {
    usage: &'static str,
    success_verb: &'static str,
    noop_msg: &'static str,
}

/// Shared helper for `/team join` and `/team leave`.
///
/// Both arms follow the same pattern: extract team_name, resolve target_user,
/// lock the registry, call the mutation, and translate the result. The only
/// differences are the usage string, the registry method, the success verb,
/// and the "no-op" message — all passed via [`MutateLabels`] and `op`.
async fn team_mutate<F>(
    params: &[String],
    username: &str,
    state: &RoomState,
    registry_lock: &Mutex<UserRegistry>,
    labels: MutateLabels,
    op: F,
) -> anyhow::Result<CommandResult>
where
    F: FnOnce(&mut UserRegistry, &str, &str) -> Result<bool, String>,
{
    let team_name = match params.get(1) {
        Some(t) => t.as_str(),
        None => {
            let sys = make_system(&state.room_id, "broker", labels.usage.to_owned());
            let json = serde_json::to_string(&sys)?;
            return Ok(CommandResult::Reply(json));
        }
    };
    let target_user = params.get(2).map(|s| s.as_str()).unwrap_or(username);
    let mut reg = registry_lock.lock().await;
    match op(&mut reg, team_name, target_user) {
        Ok(true) => {
            drop(reg);
            let content = format!("{target_user} {} team {team_name}", labels.success_verb);
            let sys = make_system(&state.room_id, "broker", content);
            let seq_msg =
                broadcast_and_persist(&sys, &state.clients, &state.chat_path, &state.seq_counter)
                    .await?;
            let json = serde_json::to_string(&seq_msg)?;
            Ok(CommandResult::HandledWithReply(json))
        }
        Ok(false) => {
            let sys = make_system(
                &state.room_id,
                "broker",
                format!("{target_user} {} {team_name}", labels.noop_msg),
            );
            let json = serde_json::to_string(&sys)?;
            Ok(CommandResult::Reply(json))
        }
        Err(e) => {
            let sys = make_system(&state.room_id, "broker", format!("error: {e}"));
            let json = serde_json::to_string(&sys)?;
            Ok(CommandResult::Reply(json))
        }
    }
}

/// Handle `/team <action> [args...]` — manage daemon-level teams.
///
/// Subcommands:
/// - `/team join <team> [user]` — add yourself or named user to a team (creates if needed)
/// - `/team leave <team> [user]` — remove yourself or named user (deletes if empty)
/// - `/team list` — show all teams and members
/// - `/team show <team>` — show members of a specific team
pub(super) async fn handle_team(
    params: &[String],
    username: &str,
    state: &RoomState,
) -> anyhow::Result<CommandResult> {
    let action = params.first().map(String::as_str).unwrap_or("");
    let registry_lock = match state.auth.registry.get() {
        Some(r) => r,
        None => {
            let sys = make_system(
                &state.room_id,
                "broker",
                "teams require daemon mode".to_owned(),
            );
            let json = serde_json::to_string(&sys)?;
            return Ok(CommandResult::Reply(json));
        }
    };

    match action {
        "join" => {
            team_mutate(
                params,
                username,
                state,
                registry_lock,
                MutateLabels {
                    usage: "usage: /team join <team> [user]",
                    success_verb: "joined",
                    noop_msg: "is already in team",
                },
                |reg, team, user| reg.join_team(team, user),
            )
            .await
        }
        "leave" => {
            team_mutate(
                params,
                username,
                state,
                registry_lock,
                MutateLabels {
                    usage: "usage: /team leave <team> [user]",
                    success_verb: "left",
                    noop_msg: "is not in team",
                },
                |reg, team, user| reg.leave_team(team, user),
            )
            .await
        }
        "list" => {
            let reg = registry_lock.lock().await;
            let teams = reg.list_teams();
            let content = if teams.is_empty() {
                "no teams".to_owned()
            } else {
                teams
                    .iter()
                    .map(|t| {
                        let members: Vec<&str> = t.members.iter().map(String::as_str).collect();
                        format!("{}: {}", t.name, members.join(", "))
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let sys = make_system(&state.room_id, "broker", content);
            let json = serde_json::to_string(&sys)?;
            Ok(CommandResult::Reply(json))
        }
        "show" => {
            let team_name = match params.get(1) {
                Some(t) => t.as_str(),
                None => {
                    let sys = make_system(
                        &state.room_id,
                        "broker",
                        "usage: /team show <team>".to_owned(),
                    );
                    let json = serde_json::to_string(&sys)?;
                    return Ok(CommandResult::Reply(json));
                }
            };
            let reg = registry_lock.lock().await;
            let content = match reg.get_team(team_name) {
                Some(team) => {
                    let members: Vec<&str> = team.members.iter().map(String::as_str).collect();
                    format!("team {}: {}", team.name, members.join(", "))
                }
                None => format!("team not found: {team_name}"),
            };
            let sys = make_system(&state.room_id, "broker", content);
            let json = serde_json::to_string(&sys)?;
            Ok(CommandResult::Reply(json))
        }
        "" => {
            let sys = make_system(
                &state.room_id,
                "broker",
                "usage: /team <join|leave|list|show> [args...]".to_owned(),
            );
            let json = serde_json::to_string(&sys)?;
            Ok(CommandResult::Reply(json))
        }
        other => {
            let sys = make_system(
                &state.room_id,
                "broker",
                format!("unknown action: {other}. use: join, leave, list, show"),
            );
            let json = serde_json::to_string(&sys)?;
            Ok(CommandResult::Reply(json))
        }
    }
}
