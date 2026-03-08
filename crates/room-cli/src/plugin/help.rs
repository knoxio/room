use super::{BoxFuture, CommandContext, CommandInfo, ParamType, Plugin, PluginResult};

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
            params: vec![super::ParamSchema {
                name: "command".to_owned(),
                param_type: ParamType::Text,
                required: false,
                description: "Command name to get help for".to_owned(),
            }],
        }]
    }

    fn handle(&self, ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>> {
        Box::pin(async move {
            if let Some(target) = ctx.params.first() {
                // Show help for a specific command
                let target = target.strip_prefix('/').unwrap_or(target);

                // Check plugin/available commands first
                if let Some(cmd) = ctx.available_commands.iter().find(|c| c.name == target) {
                    return Ok(PluginResult::Reply(format_command_help(cmd)));
                }
                // Also check built-in commands
                let builtins = super::builtin_command_infos();
                if let Some(cmd) = builtins.iter().find(|c| c.name == target) {
                    return Ok(PluginResult::Reply(format_command_help(cmd)));
                }
                return Ok(PluginResult::Reply(format!("unknown command: /{target}")));
            }

            // List all commands: built-ins first, then plugins
            let builtins = super::builtin_command_infos();
            let mut lines = vec!["available commands:".to_owned()];
            for cmd in &builtins {
                lines.push(format!("  {} — {}", cmd.usage, cmd.description));
            }
            for cmd in &ctx.available_commands {
                lines.push(format!("  {} — {}", cmd.usage, cmd.description));
            }

            Ok(PluginResult::Reply(lines.join("\n")))
        })
    }
}

/// Format detailed help for a single command, including typed parameter info.
fn format_command_help(cmd: &CommandInfo) -> String {
    let mut lines = vec![cmd.usage.clone(), format!("  {}", cmd.description)];
    if !cmd.params.is_empty() {
        lines.push("  parameters:".to_owned());
        for p in &cmd.params {
            let req = if p.required { "required" } else { "optional" };
            let type_hint = match &p.param_type {
                ParamType::Text => "text".to_owned(),
                ParamType::Username => "username".to_owned(),
                ParamType::Number { min, max } => match (min, max) {
                    (Some(lo), Some(hi)) => format!("number ({lo}..{hi})"),
                    (Some(lo), None) => format!("number ({lo}..)"),
                    (None, Some(hi)) => format!("number (..{hi})"),
                    (None, None) => "number".to_owned(),
                },
                ParamType::Choice(values) => {
                    format!("one of: {}", values.join(", "))
                }
            };
            lines.push(format!(
                "    <{}> — {} [{}] {}",
                p.name, p.description, req, type_hint
            ));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{
        ChatWriter, HistoryReader, ParamSchema, ParamType, RoomMetadata, UserInfo,
    };
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
                params: vec![],
            },
            CommandInfo {
                name: "help".to_owned(),
                description: "Show help".to_owned(),
                usage: "/help [cmd]".to_owned(),
                params: vec![],
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
            params: vec![],
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
        assert!(text.contains("List users in the room"));
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
            params: vec![ParamSchema {
                name: "count".to_owned(),
                param_type: ParamType::Number {
                    min: Some(1),
                    max: None,
                },
                required: false,
                description: "Number of messages".to_owned(),
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

    #[tokio::test]
    async fn help_specific_command_shows_param_info() {
        let commands = vec![CommandInfo {
            name: "stats".to_owned(),
            description: "Show stats".to_owned(),
            usage: "/stats [N]".to_owned(),
            params: vec![ParamSchema {
                name: "count".to_owned(),
                param_type: ParamType::Choice(vec![
                    "10".to_owned(),
                    "25".to_owned(),
                    "50".to_owned(),
                ]),
                required: false,
                description: "Number of messages".to_owned(),
            }],
        }];
        let ctx = make_test_context(vec!["stats".to_owned()], commands);
        let result = HelpPlugin.handle(ctx).await.unwrap();
        let PluginResult::Reply(text) = result else {
            panic!("expected Reply");
        };
        assert!(text.contains("parameters:"), "should show param section");
        assert!(text.contains("<count>"), "should show param name");
        assert!(text.contains("optional"), "should show required flag");
        assert!(text.contains("one of:"), "should show choices");
    }

    #[tokio::test]
    async fn help_builtin_command_shows_param_info() {
        // /kick is a built-in with a Username param
        let ctx = make_test_context(vec!["kick".to_owned()], vec![]);
        let result = HelpPlugin.handle(ctx).await.unwrap();
        let PluginResult::Reply(text) = result else {
            panic!("expected Reply");
        };
        assert!(text.contains("parameters:"));
        assert!(text.contains("username"));
        assert!(text.contains("required"));
    }
}
