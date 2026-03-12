use super::{BoxFuture, CommandContext, CommandInfo, ParamSchema, ParamType, Plugin, PluginResult};

/// `/set_status` plugin — sets the sender's presence status.
///
/// The plugin returns [`PluginResult::SetStatus`] which the broker interprets
/// to update the `StatusMap` and broadcast the announcement. The plugin itself
/// never touches broker state directly.
pub struct StatusPlugin;

impl Plugin for StatusPlugin {
    fn name(&self) -> &str {
        "status"
    }

    fn commands(&self) -> Vec<CommandInfo> {
        vec![CommandInfo {
            name: "set_status".to_owned(),
            description: "Set your presence status".to_owned(),
            usage: "/set_status <status>".to_owned(),
            params: vec![ParamSchema {
                name: "status".to_owned(),
                param_type: ParamType::Text,
                required: false,
                description: "Status text (omit to clear)".to_owned(),
            }],
        }]
    }

    fn handle(&self, ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>> {
        Box::pin(async move {
            let status = ctx.params.join(" ");
            Ok(PluginResult::SetStatus(status))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{ChatWriter, HistoryReader, RoomMetadata, UserInfo};
    use chrono::Utc;
    use std::collections::HashMap;
    use std::sync::{atomic::AtomicU64, Arc};
    use tempfile::NamedTempFile;
    use tokio::sync::Mutex;

    fn make_ctx(params: Vec<String>, path: &std::path::Path) -> CommandContext {
        let clients = Arc::new(Mutex::new(HashMap::new()));
        let chat_path = Arc::new(path.to_path_buf());
        let room_id = Arc::new("test-room".to_owned());
        let seq = Arc::new(AtomicU64::new(0));

        CommandContext {
            command: "set_status".to_owned(),
            params,
            sender: "alice".to_owned(),
            room_id: "test-room".to_owned(),
            message_id: "msg-1".to_owned(),
            timestamp: Utc::now(),
            history: HistoryReader::new(path, "alice"),
            writer: ChatWriter::new(&clients, &chat_path, &room_id, &seq, "status"),
            metadata: RoomMetadata {
                online_users: vec![UserInfo {
                    username: "alice".to_owned(),
                    status: String::new(),
                }],
                host: Some("alice".to_owned()),
                message_count: 0,
            },
            available_commands: vec![],
        }
    }

    #[tokio::test]
    async fn set_status_single_word() {
        let tmp = NamedTempFile::new().unwrap();
        let ctx = make_ctx(vec!["busy".to_owned()], tmp.path());
        let result = StatusPlugin.handle(ctx).await.unwrap();
        let PluginResult::SetStatus(status) = result else {
            panic!("expected SetStatus");
        };
        assert_eq!(status, "busy");
    }

    #[tokio::test]
    async fn set_status_multi_word() {
        let tmp = NamedTempFile::new().unwrap();
        let ctx = make_ctx(
            vec!["reviewing".to_owned(), "PR".to_owned(), "#42".to_owned()],
            tmp.path(),
        );
        let result = StatusPlugin.handle(ctx).await.unwrap();
        let PluginResult::SetStatus(status) = result else {
            panic!("expected SetStatus");
        };
        assert_eq!(status, "reviewing PR #42");
    }

    #[tokio::test]
    async fn set_status_empty_clears() {
        let tmp = NamedTempFile::new().unwrap();
        let ctx = make_ctx(vec![], tmp.path());
        let result = StatusPlugin.handle(ctx).await.unwrap();
        let PluginResult::SetStatus(status) = result else {
            panic!("expected SetStatus");
        };
        assert_eq!(status, "");
    }

    #[test]
    fn status_plugin_commands() {
        let cmds = StatusPlugin.commands();
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].name, "set_status");
        assert!(!cmds[0].params[0].required);
    }
}
