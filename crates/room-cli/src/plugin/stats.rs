use std::collections::HashMap;

use crate::message::Message;

use super::{BoxFuture, CommandContext, CommandInfo, ParamSchema, ParamType, Plugin, PluginResult};

/// Example `/stats` plugin. Shows a statistical summary of recent chat
/// activity: message count, participant count, time range, most active user.
///
/// `/summarise` is reserved for LLM-powered v2 — the core binary does not
/// depend on any LLM SDK.
pub struct StatsPlugin;

impl Plugin for StatsPlugin {
    fn name(&self) -> &str {
        "stats"
    }

    fn commands(&self) -> Vec<CommandInfo> {
        vec![CommandInfo {
            name: "stats".to_owned(),
            description: "Show statistical summary of recent chat activity".to_owned(),
            usage: "/stats [last N messages, default 50]".to_owned(),
            params: vec![ParamSchema {
                name: "count".to_owned(),
                param_type: ParamType::Choice(vec![
                    "10".to_owned(),
                    "25".to_owned(),
                    "50".to_owned(),
                    "100".to_owned(),
                ]),
                required: false,
                description: "Number of recent messages to analyze".to_owned(),
            }],
        }]
    }

    fn handle(&self, ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>> {
        Box::pin(async move {
            let n: usize = ctx
                .params
                .first()
                .and_then(|s| s.parse().ok())
                .unwrap_or(50);

            let messages = ctx.history.tail(n).await?;
            let summary = build_summary(&messages);
            ctx.writer.broadcast(&summary).await?;
            Ok(PluginResult::Handled)
        })
    }
}

fn build_summary(messages: &[Message]) -> String {
    if messages.is_empty() {
        return "stats: no messages in the requested range".to_owned();
    }

    let total = messages.len();

    // Count messages per user (only Message variants, not join/leave/system)
    let mut user_counts: HashMap<&str, usize> = HashMap::new();
    for msg in messages {
        if matches!(msg, Message::Message { .. } | Message::DirectMessage { .. }) {
            *user_counts.entry(msg.user()).or_insert(0) += 1;
        }
    }
    let participant_count = user_counts.len();

    let most_active = user_counts
        .iter()
        .max_by_key(|(_, count)| *count)
        .map(|(user, count)| format!("{user} ({count} msgs)"))
        .unwrap_or_else(|| "none".to_owned());

    let time_range = match (messages.first(), messages.last()) {
        (Some(first), Some(last)) => {
            format!(
                "{} to {}",
                first.ts().format("%H:%M UTC"),
                last.ts().format("%H:%M UTC")
            )
        }
        _ => "unknown".to_owned(),
    };

    format!(
        "stats (last {total} events): {participant_count} participants, \
         most active: {most_active}, time range: {time_range}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{make_join, make_message, make_system};

    #[test]
    fn build_summary_empty() {
        let summary = build_summary(&[]);
        assert!(summary.contains("no messages"));
    }

    #[test]
    fn build_summary_counts_only_chat_messages() {
        let msgs = vec![
            make_join("r", "alice"),
            make_message("r", "alice", "hello"),
            make_message("r", "bob", "hi"),
            make_message("r", "alice", "how are you"),
            make_system("r", "broker", "system notice"),
        ];
        let summary = build_summary(&msgs);
        // 5 events total, but only 2 participants (alice, bob) in chat messages
        assert!(summary.contains("5 events"));
        assert!(summary.contains("2 participants"));
        assert!(summary.contains("alice (2 msgs)"));
    }

    #[test]
    fn build_summary_single_user() {
        let msgs = vec![
            make_message("r", "alice", "one"),
            make_message("r", "alice", "two"),
        ];
        let summary = build_summary(&msgs);
        assert!(summary.contains("1 participants"));
        assert!(summary.contains("alice (2 msgs)"));
    }

    #[tokio::test]
    async fn stats_plugin_broadcasts_summary() {
        use crate::plugin::{ChatWriter, HistoryReader, RoomMetadata, UserInfo};
        use chrono::Utc;
        use std::collections::HashMap;
        use std::sync::{atomic::AtomicU64, Arc};
        use tempfile::NamedTempFile;
        use tokio::sync::{broadcast, Mutex};

        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path();

        // Write some messages
        for i in 0..3 {
            crate::history::append(path, &make_message("r", "alice", format!("msg {i}")))
                .await
                .unwrap();
        }

        let (tx, mut rx) = broadcast::channel::<String>(64);
        let mut client_map = HashMap::new();
        client_map.insert(1u64, ("alice".to_owned(), tx));
        let clients = Arc::new(Mutex::new(client_map));
        let chat_path = Arc::new(path.to_path_buf());
        let room_id = Arc::new("r".to_owned());
        let seq = Arc::new(AtomicU64::new(0));

        let ctx = super::super::CommandContext {
            command: "stats".to_owned(),
            params: vec!["10".to_owned()],
            sender: "alice".to_owned(),
            room_id: "r".to_owned(),
            message_id: "msg-1".to_owned(),
            timestamp: Utc::now(),
            history: Box::new(HistoryReader::new(path, "alice")),
            writer: Box::new(ChatWriter::new(
                &clients, &chat_path, &room_id, &seq, "stats",
            )),
            metadata: RoomMetadata {
                online_users: vec![UserInfo {
                    username: "alice".to_owned(),
                    status: String::new(),
                }],
                host: Some("alice".to_owned()),
                message_count: 3,
            },
            available_commands: vec![],
        };

        let result = StatsPlugin.handle(ctx).await.unwrap();
        assert!(matches!(result, PluginResult::Handled));

        // The broadcast should have sent a message
        let broadcast_msg = rx.try_recv().unwrap();
        assert!(broadcast_msg.contains("stats"));
        assert!(broadcast_msg.contains("alice"));
    }
}
