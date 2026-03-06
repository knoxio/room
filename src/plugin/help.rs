use super::{BoxFuture, CommandContext, CommandInfo, Plugin, PluginResult};

/// Built-in `/help` plugin. Lists all available commands or shows details
/// for a specific command. Dogfoods the plugin API — uses
/// `ctx.available_commands` to enumerate the registry without holding a
/// reference to it.
pub struct HelpPlugin;

impl Plugin for HelpPlugin {
    fn name(&self) -> &str {
        "help"
    }

    fn commands(&self) -> Vec<CommandInfo> {
        vec![CommandInfo {
            name: "help".to_owned(),
            description: "List available commands or get help for a specific command".to_owned(),
            usage: "/help [command]".to_owned(),
            completions: vec![],
        }]
    }

    fn handle(&self, ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>> {
        Box::pin(async move {
            if let Some(target) = ctx.params.first() {
                // Show help for a specific command
                let target = target.strip_prefix('/').unwrap_or(target);
                if let Some(cmd) = ctx.available_commands.iter().find(|c| c.name == target) {
                    let text = format!("{}\n  {}", cmd.usage, cmd.description);
                    return Ok(PluginResult::Reply(text));
                }
                // Also check built-in commands
                let builtin = builtin_help(target);
                if let Some(text) = builtin {
                    return Ok(PluginResult::Reply(text));
                }
                return Ok(PluginResult::Reply(format!("unknown command: /{target}")));
            }

            // List all commands: built-ins first, then plugins
            let mut lines = vec![
                "available commands:".to_owned(),
                "  /who — show online users".to_owned(),
                "  /set_status <status> — set your status".to_owned(),
                "  /kick <user> — kick a user (host only)".to_owned(),
                "  /reauth <user> — clear a user's token (host only)".to_owned(),
                "  /clear-tokens — clear all tokens (host only)".to_owned(),
                "  /clear — clear chat history (host only)".to_owned(),
                "  /exit — shut down the room (host only)".to_owned(),
            ];
            for cmd in &ctx.available_commands {
                lines.push(format!("  {} — {}", cmd.usage, cmd.description));
            }

            Ok(PluginResult::Reply(lines.join("\n")))
        })
    }
}

fn builtin_help(cmd: &str) -> Option<String> {
    match cmd {
        "who" => Some("/who\n  Show online users and their status".to_owned()),
        "set_status" => {
            Some("/set_status <status>\n  Set your status (visible in /who)".to_owned())
        }
        "kick" => Some(
            "/kick <username>\n  Kick a user and invalidate their token (host only)".to_owned(),
        ),
        "reauth" => Some(
            "/reauth <username>\n  Clear a user's token so they can rejoin (host only)".to_owned(),
        ),
        "clear-tokens" => {
            Some("/clear-tokens\n  Clear all tokens; all users must rejoin (host only)".to_owned())
        }
        "clear" => Some("/clear\n  Truncate chat history (host only)".to_owned()),
        "exit" => Some("/exit\n  Shut down the room (host only)".to_owned()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{ChatWriter, Completion, HistoryReader, RoomMetadata, UserInfo};
    use chrono::Utc;
    use std::sync::{atomic::AtomicU64, Arc};
    use tempfile::NamedTempFile;
    use tokio::sync::Mutex;

    fn make_test_context(params: Vec<String>, commands: Vec<CommandInfo>) -> CommandContext {
        let tmp = NamedTempFile::new().unwrap();
        let clients = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let chat_path = Arc::new(tmp.path().to_path_buf());
        let room_id = Arc::new("test".to_owned());
        let seq = Arc::new(AtomicU64::new(0));

        CommandContext {
            command: "help".to_owned(),
            params,
            sender: "alice".to_owned(),
            room_id: "test".to_owned(),
            message_id: "msg-1".to_owned(),
            timestamp: Utc::now(),
            history: HistoryReader::new(tmp.path(), "alice"),
            writer: ChatWriter::new(&clients, &chat_path, &room_id, &seq, "help"),
            metadata: RoomMetadata {
                online_users: vec![UserInfo {
                    username: "alice".to_owned(),
                    status: String::new(),
                }],
                host: Some("alice".to_owned()),
                message_count: 0,
            },
            available_commands: commands,
        }
    }

    #[tokio::test]
    async fn help_no_args_lists_all_commands() {
        let commands = vec![
            CommandInfo {
                name: "stats".to_owned(),
                description: "Show stats".to_owned(),
                usage: "/stats [N]".to_owned(),
                completions: vec![],
            },
            CommandInfo {
                name: "help".to_owned(),
                description: "Show help".to_owned(),
                usage: "/help [cmd]".to_owned(),
                completions: vec![],
            },
        ];
        let ctx = make_test_context(vec![], commands);
        let result = HelpPlugin.handle(ctx).await.unwrap();
        let PluginResult::Reply(text) = result else {
            panic!("expected Reply");
        };
        assert!(text.contains("available commands:"));
        assert!(text.contains("/who"));
        assert!(text.contains("/stats"));
        assert!(text.contains("/help"));
    }

    #[tokio::test]
    async fn help_specific_plugin_command() {
        let commands = vec![CommandInfo {
            name: "stats".to_owned(),
            description: "Show stats".to_owned(),
            usage: "/stats [N]".to_owned(),
            completions: vec![],
        }];
        let ctx = make_test_context(vec!["stats".to_owned()], commands);
        let result = HelpPlugin.handle(ctx).await.unwrap();
        let PluginResult::Reply(text) = result else {
            panic!("expected Reply");
        };
        assert!(text.contains("/stats [N]"));
        assert!(text.contains("Show stats"));
    }

    #[tokio::test]
    async fn help_specific_builtin_command() {
        let ctx = make_test_context(vec!["who".to_owned()], vec![]);
        let result = HelpPlugin.handle(ctx).await.unwrap();
        let PluginResult::Reply(text) = result else {
            panic!("expected Reply");
        };
        assert!(text.contains("/who"));
        assert!(text.contains("Show online users"));
    }

    #[tokio::test]
    async fn help_unknown_command() {
        let ctx = make_test_context(vec!["nonexistent".to_owned()], vec![]);
        let result = HelpPlugin.handle(ctx).await.unwrap();
        let PluginResult::Reply(text) = result else {
            panic!("expected Reply");
        };
        assert!(text.contains("unknown command: /nonexistent"));
    }

    #[tokio::test]
    async fn help_strips_leading_slash_from_arg() {
        let commands = vec![CommandInfo {
            name: "stats".to_owned(),
            description: "Show stats".to_owned(),
            usage: "/stats [N]".to_owned(),
            completions: vec![Completion {
                position: 0,
                values: vec!["10".to_owned()],
            }],
        }];
        let ctx = make_test_context(vec!["/stats".to_owned()], commands);
        let result = HelpPlugin.handle(ctx).await.unwrap();
        let PluginResult::Reply(text) = result else {
            panic!("expected Reply");
        };
        assert!(
            text.contains("/stats [N]"),
            "should find stats even with leading /"
        );
    }
}
