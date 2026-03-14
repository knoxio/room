#[cfg(test)]
mod tests {
    use super::super::info::{handle_info, handle_room_info, handle_user_info};
    use super::super::{route_command, CommandResult};
    use crate::broker::{
        persistence::{
            load_event_filter_map, load_subscription_map, save_event_filter_map,
            save_subscription_map,
        },
        state::RoomState,
    };
    use room_protocol::SubscriptionTier;
    use room_protocol::{make_command, make_dm, make_message};
    use std::{collections::HashMap, sync::Arc};
    use tempfile::NamedTempFile;
    use tokio::sync::Mutex;

    fn make_state(chat_path: std::path::PathBuf) -> Arc<RoomState> {
        let token_map_path = chat_path.with_extension("tokens");
        let subscription_map_path = chat_path.with_extension("subscriptions");
        RoomState::new(
            "test-room".to_owned(),
            chat_path,
            token_map_path,
            subscription_map_path,
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
            None,
        )
        .unwrap()
    }

    // ── route_command: passthrough ─────────────────────────────────────────

    #[tokio::test]
    async fn route_command_regular_message_is_passthrough() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_message("test-room", "alice", "hello");
        let result = route_command(msg, "alice", &state).await.unwrap();
        assert!(matches!(result, CommandResult::Passthrough(_)));
    }

    #[tokio::test]
    async fn route_command_dm_message_is_passthrough() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_dm("test-room", "alice", "bob", "secret");
        let result = route_command(msg, "alice", &state).await.unwrap();
        assert!(matches!(result, CommandResult::Passthrough(_)));
    }

    // ── route_command: set_status ──────────────────────────────────────────

    #[tokio::test]
    async fn route_command_set_status_returns_handled_with_reply_and_updates_map() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "set_status", vec!["busy".to_owned()]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply, got Handled or other");
        };
        assert!(
            json.contains("set status"),
            "reply JSON should contain status announcement"
        );
        assert!(
            json.contains("busy"),
            "reply JSON should contain the status text"
        );
        assert_eq!(
            state
                .status_map
                .lock()
                .await
                .get("alice")
                .map(String::as_str),
            Some("busy")
        );
    }

    #[tokio::test]
    async fn route_command_set_status_empty_params_clears_status() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        state
            .status_map
            .lock()
            .await
            .insert("alice".to_owned(), "busy".to_owned());
        let msg = make_command("test-room", "alice", "set_status", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        assert!(matches!(result, CommandResult::HandledWithReply(_)));
        assert_eq!(
            state
                .status_map
                .lock()
                .await
                .get("alice")
                .map(String::as_str),
            Some("")
        );
    }

    #[tokio::test]
    async fn route_command_set_status_multi_word_joins_all_params() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command(
            "test-room",
            "alice",
            "set_status",
            vec!["reviewing".to_owned(), "PR".to_owned(), "#42".to_owned()],
        );
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply");
        };
        assert!(
            json.contains("reviewing PR #42"),
            "broadcast must contain full multi-word status, got: {json}"
        );
        assert_eq!(
            state
                .status_map
                .lock()
                .await
                .get("alice")
                .map(String::as_str),
            Some("reviewing PR #42")
        );
    }

    // ── route_command: who ─────────────────────────────────────────────────

    #[tokio::test]
    async fn route_command_who_with_online_user_in_reply() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        state
            .status_map
            .lock()
            .await
            .insert("alice".to_owned(), String::new());
        let msg = make_command("test-room", "alice", "who", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply");
        };
        assert!(json.contains("alice"), "reply should list alice");
    }

    #[tokio::test]
    async fn route_command_who_empty_room_says_no_users_online() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "who", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply");
        };
        assert!(json.contains("no users online"));
    }

    #[tokio::test]
    async fn route_command_who_shows_status_alongside_name() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        state
            .status_map
            .lock()
            .await
            .insert("alice".to_owned(), "reviewing PR".to_owned());
        let msg = make_command("test-room", "alice", "who", vec![]);
        let CommandResult::Reply(json) = route_command(msg, "alice", &state).await.unwrap() else {
            panic!("expected Reply");
        };
        assert!(json.contains("reviewing PR"));
    }

    #[tokio::test]
    async fn route_command_who_sanitizes_commas_in_status() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        {
            let mut map = state.status_map.lock().await;
            map.insert("alice".to_owned(), "PR #630 merged, #636 filed".to_owned());
            map.insert("bob".to_owned(), String::new());
        }
        let msg = make_command("test-room", "alice", "who", vec![]);
        let CommandResult::Reply(json) = route_command(msg, "alice", &state).await.unwrap() else {
            panic!("expected Reply");
        };
        // The comma in the status must be replaced so the TUI parser
        // doesn't treat "#636 filed" as a separate username (#656).
        assert!(
            !json.contains("PR #630 merged, #636"),
            "raw comma must be sanitized: {json}"
        );
        assert!(
            json.contains("PR #630 merged; #636 filed"),
            "comma should be replaced with semicolon: {json}"
        );
        // bob should still appear as a separate entry
        assert!(json.contains("bob"), "bob should be listed: {json}");
    }

    // ── route_command: admin permission gating ────────────────────────────

    #[tokio::test]
    async fn route_command_admin_as_non_host_gets_permission_denied_reply() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("host-user".to_owned());
        let msg = make_command("test-room", "alice", "kick", vec!["bob".to_owned()]);
        let CommandResult::Reply(json) = route_command(msg, "alice", &state).await.unwrap() else {
            panic!("expected Reply");
        };
        assert!(json.contains("permission denied"));
    }

    #[tokio::test]
    async fn route_command_admin_when_no_host_set_gets_permission_denied() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        // host_user is None
        let msg = make_command("test-room", "alice", "exit", vec![]);
        let CommandResult::Reply(json) = route_command(msg, "alice", &state).await.unwrap() else {
            panic!("expected Reply");
        };
        assert!(json.contains("permission denied"));
    }

    // ── route_command: admin commands as host ─────────────────────────────

    #[tokio::test]
    async fn route_command_kick_as_host_returns_handled_and_invalidates_token() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        state
            .auth
            .token_map
            .lock()
            .await
            .insert("some-uuid".to_owned(), "bob".to_owned());
        let msg = make_command("test-room", "alice", "kick", vec!["bob".to_owned()]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        assert!(matches!(result, CommandResult::Handled));
        let guard = state.auth.token_map.lock().await;
        assert!(
            !guard.contains_key("some-uuid"),
            "original token must be revoked"
        );
        assert_eq!(
            guard.get("KICKED:bob").map(String::as_str),
            Some("bob"),
            "KICKED sentinel must be inserted"
        );
    }

    #[tokio::test]
    async fn route_command_exit_as_host_returns_shutdown() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        let msg = make_command("test-room", "alice", "exit", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        assert!(matches!(result, CommandResult::Shutdown));
    }

    // ── route_command: built-in param validation ────────────────────────

    #[tokio::test]
    async fn route_command_kick_missing_user_gets_validation_error() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        let msg = make_command("test-room", "alice", "kick", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply with validation error");
        };
        assert!(
            json.contains("missing required"),
            "should report missing param"
        );
        assert!(json.contains("<user>"), "should name the missing param");
    }

    #[tokio::test]
    async fn route_command_reauth_missing_user_gets_validation_error() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        let msg = make_command("test-room", "alice", "reauth", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply with validation error");
        };
        assert!(json.contains("missing required"));
    }

    #[tokio::test]
    async fn route_command_kick_with_valid_params_passes_validation() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        // Kick with valid username — should not be rejected by validation.
        let msg = make_command("test-room", "alice", "kick", vec!["bob".to_owned()]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        // kick succeeds (Handled), not a validation error Reply
        assert!(matches!(result, CommandResult::Handled));
    }

    #[tokio::test]
    async fn route_command_who_no_params_passes_validation() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        // /who has no required params — should always pass validation
        let msg = make_command("test-room", "alice", "who", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        assert!(matches!(result, CommandResult::Reply(_)));
    }

    #[tokio::test]
    async fn route_command_reply_missing_params_gets_validation_error() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        // /reply requires both id and message
        let msg = make_command("test-room", "alice", "reply", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply with validation error");
        };
        assert!(json.contains("missing required"));
    }

    #[tokio::test]
    async fn route_command_nonbuiltin_command_skips_validation() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        // A command not in builtin_command_infos — no schema to validate against
        let msg = make_command("test-room", "alice", "unknown_cmd", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        // Falls through to Passthrough (no schema, no handler)
        assert!(matches!(result, CommandResult::Passthrough(_)));
    }

    // ── validate_params tests ─────────────────────────────────────────────

    mod validation_tests {
        use super::super::super::validate::validate_params;
        use crate::plugin::{CommandInfo, ParamSchema, ParamType};

        fn cmd_with_params(params: Vec<ParamSchema>) -> CommandInfo {
            CommandInfo {
                name: "test".to_owned(),
                description: "test".to_owned(),
                usage: "/test".to_owned(),
                params,
            }
        }

        #[test]
        fn validate_empty_schema_always_passes() {
            let cmd = cmd_with_params(vec![]);
            assert!(validate_params(&[], &cmd).is_ok());
            assert!(validate_params(&["extra".to_owned()], &cmd).is_ok());
        }

        #[test]
        fn validate_required_param_missing() {
            let cmd = cmd_with_params(vec![ParamSchema {
                name: "user".to_owned(),
                param_type: ParamType::Text,
                required: true,
                description: "target user".to_owned(),
            }]);
            let err = validate_params(&[], &cmd).unwrap_err();
            assert!(err.contains("missing required"));
            assert!(err.contains("<user>"));
        }

        #[test]
        fn validate_required_param_present() {
            let cmd = cmd_with_params(vec![ParamSchema {
                name: "user".to_owned(),
                param_type: ParamType::Text,
                required: true,
                description: "target user".to_owned(),
            }]);
            assert!(validate_params(&["alice".to_owned()], &cmd).is_ok());
        }

        #[test]
        fn validate_optional_param_missing_is_ok() {
            let cmd = cmd_with_params(vec![ParamSchema {
                name: "count".to_owned(),
                param_type: ParamType::Number {
                    min: None,
                    max: None,
                },
                required: false,
                description: "count".to_owned(),
            }]);
            assert!(validate_params(&[], &cmd).is_ok());
        }

        #[test]
        fn validate_choice_valid_value() {
            let cmd = cmd_with_params(vec![ParamSchema {
                name: "color".to_owned(),
                param_type: ParamType::Choice(vec!["red".to_owned(), "blue".to_owned()]),
                required: true,
                description: "pick a color".to_owned(),
            }]);
            assert!(validate_params(&["red".to_owned()], &cmd).is_ok());
            assert!(validate_params(&["blue".to_owned()], &cmd).is_ok());
        }

        #[test]
        fn validate_choice_invalid_value() {
            let cmd = cmd_with_params(vec![ParamSchema {
                name: "color".to_owned(),
                param_type: ParamType::Choice(vec!["red".to_owned(), "blue".to_owned()]),
                required: true,
                description: "pick a color".to_owned(),
            }]);
            let err = validate_params(&["green".to_owned()], &cmd).unwrap_err();
            assert!(err.contains("must be one of"));
            assert!(err.contains("red"));
            assert!(err.contains("blue"));
        }

        #[test]
        fn validate_number_valid() {
            let cmd = cmd_with_params(vec![ParamSchema {
                name: "count".to_owned(),
                param_type: ParamType::Number {
                    min: Some(1),
                    max: Some(100),
                },
                required: true,
                description: "count".to_owned(),
            }]);
            assert!(validate_params(&["50".to_owned()], &cmd).is_ok());
            assert!(validate_params(&["1".to_owned()], &cmd).is_ok());
            assert!(validate_params(&["100".to_owned()], &cmd).is_ok());
        }

        #[test]
        fn validate_number_not_a_number() {
            let cmd = cmd_with_params(vec![ParamSchema {
                name: "count".to_owned(),
                param_type: ParamType::Number {
                    min: None,
                    max: None,
                },
                required: true,
                description: "count".to_owned(),
            }]);
            let err = validate_params(&["abc".to_owned()], &cmd).unwrap_err();
            assert!(err.contains("must be a number"));
        }

        #[test]
        fn validate_number_below_min() {
            let cmd = cmd_with_params(vec![ParamSchema {
                name: "count".to_owned(),
                param_type: ParamType::Number {
                    min: Some(10),
                    max: None,
                },
                required: true,
                description: "count".to_owned(),
            }]);
            let err = validate_params(&["5".to_owned()], &cmd).unwrap_err();
            assert!(err.contains("must be >= 10"));
        }

        #[test]
        fn validate_number_above_max() {
            let cmd = cmd_with_params(vec![ParamSchema {
                name: "count".to_owned(),
                param_type: ParamType::Number {
                    min: None,
                    max: Some(50),
                },
                required: true,
                description: "count".to_owned(),
            }]);
            let err = validate_params(&["100".to_owned()], &cmd).unwrap_err();
            assert!(err.contains("must be <= 50"));
        }

        #[test]
        fn validate_text_always_passes() {
            let cmd = cmd_with_params(vec![ParamSchema {
                name: "msg".to_owned(),
                param_type: ParamType::Text,
                required: true,
                description: "message".to_owned(),
            }]);
            assert!(validate_params(&["anything at all".to_owned()], &cmd).is_ok());
        }

        #[test]
        fn validate_username_always_passes() {
            let cmd = cmd_with_params(vec![ParamSchema {
                name: "user".to_owned(),
                param_type: ParamType::Username,
                required: true,
                description: "user".to_owned(),
            }]);
            assert!(validate_params(&["alice".to_owned()], &cmd).is_ok());
        }

        #[test]
        fn validate_multiple_params() {
            let cmd = cmd_with_params(vec![
                ParamSchema {
                    name: "user".to_owned(),
                    param_type: ParamType::Username,
                    required: true,
                    description: "target".to_owned(),
                },
                ParamSchema {
                    name: "count".to_owned(),
                    param_type: ParamType::Number {
                        min: Some(1),
                        max: Some(100),
                    },
                    required: false,
                    description: "count".to_owned(),
                },
            ]);
            // Both present and valid
            assert!(validate_params(&["alice".to_owned(), "50".to_owned()], &cmd).is_ok());
            // First present, second omitted (optional)
            assert!(validate_params(&["alice".to_owned()], &cmd).is_ok());
            // First missing (required)
            assert!(validate_params(&[], &cmd).is_err());
        }

        #[test]
        fn validate_choice_optional_missing_is_ok() {
            let cmd = cmd_with_params(vec![ParamSchema {
                name: "level".to_owned(),
                param_type: ParamType::Choice(vec!["low".to_owned(), "high".to_owned()]),
                required: false,
                description: "level".to_owned(),
            }]);
            assert!(validate_params(&[], &cmd).is_ok());
        }
    }

    // ── room management commands ──────────────────────────────────────────

    fn make_state_with_config(
        chat_path: std::path::PathBuf,
        config: room_protocol::RoomConfig,
    ) -> Arc<RoomState> {
        let token_map_path = chat_path.with_extension("tokens");
        let subscription_map_path = chat_path.with_extension("subscriptions");
        RoomState::new(
            "test-room".to_owned(),
            chat_path,
            token_map_path,
            subscription_map_path,
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
            Some(config),
        )
        .unwrap()
    }

    // ── /info and /room-info ─────────────────────────────────────────────

    #[tokio::test]
    async fn room_info_no_config() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let result = handle_room_info(&state).await;
        assert!(result.contains("legacy"));
        assert!(result.contains("test-room"));
    }

    #[tokio::test]
    async fn room_info_includes_host() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        let result = handle_room_info(&state).await;
        assert!(result.contains("host: alice"), "got: {result}");
    }

    #[tokio::test]
    async fn room_info_with_config() {
        let tmp = NamedTempFile::new().unwrap();
        let config = room_protocol::RoomConfig::dm("alice", "bob");
        let state = make_state_with_config(tmp.path().to_path_buf(), config);
        let result = handle_room_info(&state).await;
        assert!(result.contains("dm"));
        assert!(result.contains("alice"));
    }

    #[tokio::test]
    async fn info_no_args_shows_room_info() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let result = handle_info(&[], &state).await;
        assert!(result.contains("test-room"), "got: {result}");
        assert!(result.contains("legacy"), "got: {result}");
    }

    #[tokio::test]
    async fn info_with_username_shows_user_info() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        state.set_status("bob", "coding".to_owned()).await;
        state.set_subscription("bob", SubscriptionTier::Full).await;
        let result = handle_info(&["bob".to_owned()], &state).await;
        assert!(result.contains("user: bob"), "got: {result}");
        assert!(result.contains("online (coding)"), "got: {result}");
        assert!(result.contains("subscription: full"), "got: {result}");
    }

    #[tokio::test]
    async fn info_strips_at_prefix() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        state.set_status("carol", String::new()).await;
        let result = handle_info(&["@carol".to_owned()], &state).await;
        assert!(result.contains("user: carol"), "got: {result}");
    }

    #[tokio::test]
    async fn user_info_offline_user() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let result = handle_user_info("ghost", &state).await;
        assert!(result.contains("user: ghost"), "got: {result}");
        assert!(result.contains("offline"), "got: {result}");
        assert!(result.contains("subscription: none"), "got: {result}");
    }

    #[tokio::test]
    async fn user_info_online_no_status() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        state.set_status("alice", String::new()).await;
        let result = handle_user_info("alice", &state).await;
        assert!(result.contains("online"), "got: {result}");
        assert!(!result.contains("offline"), "got: {result}");
    }

    #[tokio::test]
    async fn user_info_shows_host_flag() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        state.set_status("alice", "hosting".to_owned()).await;
        let result = handle_user_info("alice", &state).await;
        assert!(result.contains("host: yes"), "got: {result}");
    }

    #[tokio::test]
    async fn user_info_non_host_omits_host_flag() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        state.set_status("bob", String::new()).await;
        let result = handle_user_info("bob", &state).await;
        assert!(!result.contains("host"), "got: {result}");
    }

    #[tokio::test]
    async fn route_command_info_returns_reply() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "info", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        assert!(matches!(result, CommandResult::Reply(_)));
    }

    #[tokio::test]
    async fn route_command_room_info_alias_returns_reply() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "room-info", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        assert!(matches!(result, CommandResult::Reply(_)));
    }

    #[tokio::test]
    async fn route_command_info_with_user_returns_reply() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        state.set_status("bob", "busy".to_owned()).await;
        let msg = make_command("test-room", "alice", "info", vec!["bob".to_owned()]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        match result {
            CommandResult::Reply(json) => {
                assert!(json.contains("user: bob"), "got: {json}");
                assert!(json.contains("online (busy)"), "got: {json}");
            }
            _ => panic!("expected Reply"),
        }
    }

    // ── subscription commands ──────────────────────────────────────────────

    #[tokio::test]
    async fn set_subscription_alias_works() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command(
            "test-room",
            "alice",
            "set_subscription",
            vec!["full".to_owned()],
        );
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply");
        };
        assert!(json.contains("subscribed"));
        assert!(json.contains("full"));
        assert_eq!(
            *state
                .filters
                .subscription_map
                .lock()
                .await
                .get("alice")
                .unwrap(),
            SubscriptionTier::Full
        );
    }

    #[tokio::test]
    async fn set_subscription_alias_mentions_only() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command(
            "test-room",
            "bob",
            "set_subscription",
            vec!["mentions_only".to_owned()],
        );
        let result = route_command(msg, "bob", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply");
        };
        assert!(json.contains("subscribed"));
        assert_eq!(
            *state
                .filters
                .subscription_map
                .lock()
                .await
                .get("bob")
                .unwrap(),
            SubscriptionTier::MentionsOnly
        );
    }

    #[tokio::test]
    async fn subscribe_default_tier_is_full() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "subscribe", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply");
        };
        assert!(json.contains("subscribed"));
        assert!(json.contains("full"));
        assert_eq!(
            *state
                .filters
                .subscription_map
                .lock()
                .await
                .get("alice")
                .unwrap(),
            SubscriptionTier::Full
        );
    }

    #[tokio::test]
    async fn subscribe_explicit_mentions_only() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command(
            "test-room",
            "bob",
            "subscribe",
            vec!["mentions_only".to_owned()],
        );
        let result = route_command(msg, "bob", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply");
        };
        assert!(json.contains("mentions_only"));
        assert_eq!(
            *state
                .filters
                .subscription_map
                .lock()
                .await
                .get("bob")
                .unwrap(),
            SubscriptionTier::MentionsOnly
        );
    }

    #[tokio::test]
    async fn subscribe_overwrites_previous_tier() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg1 = make_command("test-room", "alice", "subscribe", vec!["full".to_owned()]);
        route_command(msg1, "alice", &state).await.unwrap();
        let msg2 = make_command(
            "test-room",
            "alice",
            "subscribe",
            vec!["mentions_only".to_owned()],
        );
        route_command(msg2, "alice", &state).await.unwrap();
        assert_eq!(
            *state
                .filters
                .subscription_map
                .lock()
                .await
                .get("alice")
                .unwrap(),
            SubscriptionTier::MentionsOnly,
            "second subscribe should overwrite the first"
        );
    }

    #[tokio::test]
    async fn unsubscribe_sets_tier_to_unsubscribed() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        // Subscribe first
        let msg = make_command("test-room", "alice", "subscribe", vec!["full".to_owned()]);
        route_command(msg, "alice", &state).await.unwrap();
        // Then unsubscribe
        let msg = make_command("test-room", "alice", "unsubscribe", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply");
        };
        assert!(json.contains("unsubscribed"));
        assert_eq!(
            *state
                .filters
                .subscription_map
                .lock()
                .await
                .get("alice")
                .unwrap(),
            SubscriptionTier::Unsubscribed
        );
    }

    #[tokio::test]
    async fn unsubscribe_without_prior_subscription() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "unsubscribe", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        // Should still work — sets to Unsubscribed even without prior entry
        assert!(matches!(result, CommandResult::HandledWithReply(_)));
        assert_eq!(
            *state
                .filters
                .subscription_map
                .lock()
                .await
                .get("alice")
                .unwrap(),
            SubscriptionTier::Unsubscribed
        );
    }

    #[tokio::test]
    async fn subscriptions_empty() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "subscriptions", vec![]);
        let CommandResult::Reply(json) = route_command(msg, "alice", &state).await.unwrap() else {
            panic!("expected Reply");
        };
        assert!(json.contains("no subscriptions"));
    }

    #[tokio::test]
    async fn subscriptions_lists_all_sorted() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        {
            let mut map = state.filters.subscription_map.lock().await;
            map.insert("zara".to_owned(), SubscriptionTier::Full);
            map.insert("alice".to_owned(), SubscriptionTier::MentionsOnly);
        }
        let msg = make_command("test-room", "alice", "subscriptions", vec![]);
        let CommandResult::Reply(json) = route_command(msg, "alice", &state).await.unwrap() else {
            panic!("expected Reply");
        };
        assert!(json.contains("alice: mentions_only"));
        assert!(json.contains("zara: full"));
        // Verify sorted order
        let alice_pos = json.find("alice: mentions_only").unwrap();
        let zara_pos = json.find("zara: full").unwrap();
        assert!(
            alice_pos < zara_pos,
            "subscriptions should be sorted by username"
        );
    }

    #[tokio::test]
    async fn subscribe_invalid_tier_returns_error() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "subscribe", vec!["banana".to_owned()]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply for invalid tier");
        };
        assert!(json.contains("must be one of"));
        // Should not have stored anything
        assert!(state
            .filters
            .subscription_map
            .lock()
            .await
            .get("alice")
            .is_none());
    }

    #[tokio::test]
    async fn subscribe_broadcasts_system_message() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "subscribe", vec![]);
        route_command(msg, "alice", &state).await.unwrap();
        // Verify the broadcast was persisted to chat history
        let history = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(history.contains("subscribed"));
        assert!(history.contains("alice"));
    }

    #[tokio::test]
    async fn unsubscribe_broadcasts_system_message() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "unsubscribe", vec![]);
        route_command(msg, "alice", &state).await.unwrap();
        let history = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(history.contains("unsubscribed"));
        assert!(history.contains("alice"));
    }

    // ── subscribe join-permission guard (#491) ─────────────────────────

    #[tokio::test]
    async fn subscribe_dm_room_participant_allowed() {
        let tmp = NamedTempFile::new().unwrap();
        let config = room_protocol::RoomConfig::dm("alice", "bob");
        let state = make_state_with_config(tmp.path().to_path_buf(), config);
        let msg = make_command("test-room", "alice", "subscribe", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        assert!(
            matches!(result, CommandResult::HandledWithReply(_)),
            "DM participant should be allowed to subscribe"
        );
        assert_eq!(
            state
                .filters
                .subscription_map
                .lock()
                .await
                .get("alice")
                .copied(),
            Some(SubscriptionTier::Full),
        );
    }

    #[tokio::test]
    async fn subscribe_dm_room_non_participant_rejected() {
        let tmp = NamedTempFile::new().unwrap();
        let config = room_protocol::RoomConfig::dm("alice", "bob");
        let state = make_state_with_config(tmp.path().to_path_buf(), config);
        let msg = make_command("test-room", "eve", "subscribe", vec![]);
        let result = route_command(msg, "eve", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply (rejection) for non-participant");
        };
        assert!(
            json.contains("permission denied"),
            "should contain permission denied, got: {json}"
        );
        assert!(
            state
                .filters
                .subscription_map
                .lock()
                .await
                .get("eve")
                .is_none(),
            "non-participant must not get a subscription entry"
        );
    }

    #[tokio::test]
    async fn subscribe_private_room_non_invited_rejected() {
        let tmp = NamedTempFile::new().unwrap();
        let config = room_protocol::RoomConfig {
            visibility: room_protocol::RoomVisibility::Private,
            max_members: None,
            invite_list: ["alice".to_owned()].into(),
            created_by: "owner".to_owned(),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
        };
        let state = make_state_with_config(tmp.path().to_path_buf(), config);
        let msg = make_command("test-room", "stranger", "subscribe", vec![]);
        let result = route_command(msg, "stranger", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply (rejection)");
        };
        assert!(json.contains("permission denied"));
        assert!(state
            .filters
            .subscription_map
            .lock()
            .await
            .get("stranger")
            .is_none());
    }

    #[tokio::test]
    async fn subscribe_public_room_always_allowed() {
        let tmp = NamedTempFile::new().unwrap();
        let config = room_protocol::RoomConfig::public("owner");
        let state = make_state_with_config(tmp.path().to_path_buf(), config);
        let msg = make_command("test-room", "anyone", "subscribe", vec![]);
        let result = route_command(msg, "anyone", &state).await.unwrap();
        assert!(
            matches!(result, CommandResult::HandledWithReply(_)),
            "public room subscribe should succeed"
        );
    }

    #[tokio::test]
    async fn set_subscription_alias_dm_guard_applies() {
        let tmp = NamedTempFile::new().unwrap();
        let config = room_protocol::RoomConfig::dm("alice", "bob");
        let state = make_state_with_config(tmp.path().to_path_buf(), config);
        // set_subscription is an alias for subscribe — guard must apply
        let msg = make_command(
            "test-room",
            "eve",
            "set_subscription",
            vec!["full".to_owned()],
        );
        let result = route_command(msg, "eve", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply (rejection) for non-participant via alias");
        };
        assert!(json.contains("permission denied"));
    }

    // ── subscription persistence ─────────────────────────────────────────

    #[test]
    fn load_subscription_map_missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.subscriptions");
        let map = load_subscription_map(&path);
        assert!(map.is_empty());
    }

    #[test]
    fn save_and_load_subscription_map_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.subscriptions");

        let mut original = HashMap::new();
        original.insert("alice".to_owned(), SubscriptionTier::Full);
        original.insert("bob".to_owned(), SubscriptionTier::MentionsOnly);
        original.insert("carol".to_owned(), SubscriptionTier::Unsubscribed);

        save_subscription_map(&original, &path).unwrap();
        let loaded = load_subscription_map(&path);
        assert_eq!(loaded, original);
    }

    #[test]
    fn load_subscription_map_corrupt_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.subscriptions");
        std::fs::write(&path, "not json{{{").unwrap();
        let map = load_subscription_map(&path);
        assert!(map.is_empty());
    }

    #[tokio::test]
    async fn subscribe_persists_to_disk() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "subscribe", vec![]);
        route_command(msg, "alice", &state).await.unwrap();

        let loaded = load_subscription_map(&state.filters.subscription_map_path);
        assert_eq!(loaded.get("alice"), Some(&SubscriptionTier::Full));
    }

    #[tokio::test]
    async fn unsubscribe_persists_to_disk() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());

        // Subscribe first, then unsubscribe.
        let msg = make_command("test-room", "alice", "subscribe", vec![]);
        route_command(msg, "alice", &state).await.unwrap();
        let msg = make_command("test-room", "alice", "unsubscribe", vec![]);
        route_command(msg, "alice", &state).await.unwrap();

        let loaded = load_subscription_map(&state.filters.subscription_map_path);
        assert_eq!(loaded.get("alice"), Some(&SubscriptionTier::Unsubscribed));
    }

    #[tokio::test]
    async fn subscribe_accumulates_on_disk() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());

        let msg = make_command("test-room", "alice", "subscribe", vec![]);
        route_command(msg, "alice", &state).await.unwrap();
        let msg = make_command(
            "test-room",
            "bob",
            "subscribe",
            vec!["mentions_only".to_owned()],
        );
        route_command(msg, "bob", &state).await.unwrap();

        let loaded = load_subscription_map(&state.filters.subscription_map_path);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.get("alice"), Some(&SubscriptionTier::Full));
        assert_eq!(loaded.get("bob"), Some(&SubscriptionTier::MentionsOnly));
    }

    #[tokio::test]
    async fn subscribe_survives_simulated_restart() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());

        let msg = make_command("test-room", "alice", "subscribe", vec![]);
        route_command(msg, "alice", &state).await.unwrap();

        // Simulate restart: new state, load from disk.
        let loaded = load_subscription_map(&state.filters.subscription_map_path);
        assert_eq!(loaded.get("alice"), Some(&SubscriptionTier::Full));

        // Verify it can be populated into a new RoomState.
        let state2 = RoomState::new(
            state.room_id.as_ref().clone(),
            state.chat_path.as_ref().clone(),
            state.auth.token_map_path.as_ref().clone(),
            state.filters.subscription_map_path.as_ref().clone(),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(loaded)),
            None,
        )
        .unwrap();
        assert_eq!(
            *state2
                .filters
                .subscription_map
                .lock()
                .await
                .get("alice")
                .unwrap(),
            SubscriptionTier::Full
        );
    }

    #[tokio::test]
    async fn subscribe_overwrite_persists_latest_tier() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());

        let msg = make_command("test-room", "alice", "subscribe", vec![]);
        route_command(msg, "alice", &state).await.unwrap();
        let msg = make_command(
            "test-room",
            "alice",
            "subscribe",
            vec!["mentions_only".to_owned()],
        );
        route_command(msg, "alice", &state).await.unwrap();

        let loaded = load_subscription_map(&state.filters.subscription_map_path);
        assert_eq!(loaded.get("alice"), Some(&SubscriptionTier::MentionsOnly));
    }

    // ── subscribe_events command ────────────────────────────────────────

    fn make_state_with_event_filters(chat_path: std::path::PathBuf) -> Arc<RoomState> {
        let state = make_state(chat_path.clone());
        let ef_path = chat_path.with_extension("event_filters");
        state.set_event_filter_map(Arc::new(Mutex::new(HashMap::new())), ef_path);
        state
    }

    #[tokio::test]
    async fn subscribe_events_all() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state_with_event_filters(tmp.path().to_path_buf());
        let msg = make_command(
            "test-room",
            "alice",
            "subscribe_events",
            vec!["all".to_owned()],
        );
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply");
        };
        assert!(json.contains("event filter"));
        assert!(json.contains("all"));
    }

    #[tokio::test]
    async fn subscribe_events_none() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state_with_event_filters(tmp.path().to_path_buf());
        let msg = make_command(
            "test-room",
            "alice",
            "subscribe_events",
            vec!["none".to_owned()],
        );
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply");
        };
        assert!(json.contains("event filter"));
        assert!(json.contains("none"));
    }

    #[tokio::test]
    async fn subscribe_events_csv() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state_with_event_filters(tmp.path().to_path_buf());
        let msg = make_command(
            "test-room",
            "alice",
            "subscribe_events",
            vec!["task_posted,task_finished".to_owned()],
        );
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply");
        };
        assert!(json.contains("event filter"));
        assert!(json.contains("task_finished"));
        assert!(json.contains("task_posted"));
    }

    #[tokio::test]
    async fn subscribe_events_invalid_type_returns_error() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state_with_event_filters(tmp.path().to_path_buf());
        let msg = make_command(
            "test-room",
            "alice",
            "subscribe_events",
            vec!["banana".to_owned()],
        );
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply for invalid event type");
        };
        assert!(json.contains("unknown event type"));
    }

    #[tokio::test]
    async fn subscribe_events_default_is_all() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state_with_event_filters(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "subscribe_events", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply");
        };
        assert!(json.contains("event filter"));
        assert!(json.contains("all"));
    }

    #[tokio::test]
    async fn subscribe_events_broadcasts_system_message() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state_with_event_filters(tmp.path().to_path_buf());
        let msg = make_command(
            "test-room",
            "alice",
            "subscribe_events",
            vec!["task_posted".to_owned()],
        );
        route_command(msg, "alice", &state).await.unwrap();
        let history = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(history.contains("event filter"));
        assert!(history.contains("alice"));
    }

    #[tokio::test]
    async fn subscribe_events_persists_to_disk() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state_with_event_filters(tmp.path().to_path_buf());
        let msg = make_command(
            "test-room",
            "alice",
            "subscribe_events",
            vec!["task_posted".to_owned()],
        );
        route_command(msg, "alice", &state).await.unwrap();

        let ef_path = tmp.path().with_extension("event_filters");
        let loaded = load_event_filter_map(&ef_path);
        assert!(loaded.contains_key("alice"));
    }

    #[tokio::test]
    async fn subscribe_events_overwrites_previous() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state_with_event_filters(tmp.path().to_path_buf());

        let msg1 = make_command(
            "test-room",
            "alice",
            "subscribe_events",
            vec!["task_posted".to_owned()],
        );
        route_command(msg1, "alice", &state).await.unwrap();

        let msg2 = make_command(
            "test-room",
            "alice",
            "subscribe_events",
            vec!["none".to_owned()],
        );
        route_command(msg2, "alice", &state).await.unwrap();

        let ef_path = tmp.path().with_extension("event_filters");
        let loaded = load_event_filter_map(&ef_path);
        assert_eq!(loaded.get("alice"), Some(&room_protocol::EventFilter::None));
    }

    // ── event filter persistence ─────────────────────────────────────────

    #[test]
    fn load_event_filter_map_missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.event_filters");
        let map = load_event_filter_map(&path);
        assert!(map.is_empty());
    }

    #[test]
    fn save_and_load_event_filter_map_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.event_filters");

        let mut original = HashMap::new();
        original.insert("alice".to_owned(), room_protocol::EventFilter::All);
        original.insert("bob".to_owned(), room_protocol::EventFilter::None);
        let mut types = std::collections::BTreeSet::new();
        types.insert(room_protocol::EventType::TaskPosted);
        original.insert(
            "carol".to_owned(),
            room_protocol::EventFilter::Only { types },
        );

        save_event_filter_map(&original, &path).unwrap();
        let loaded = load_event_filter_map(&path);
        assert_eq!(loaded, original);
    }

    #[test]
    fn load_event_filter_map_corrupt_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.event_filters");
        std::fs::write(&path, "not json{{{").unwrap();
        let map = load_event_filter_map(&path);
        assert!(map.is_empty());
    }

    // ── plugin broadcast returns HandledWithReply for oneshot echo ─────

    #[tokio::test]
    async fn plugin_broadcast_returns_handled_with_reply() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        // /taskboard post produces PluginResult::Broadcast — should come
        // back as HandledWithReply so oneshot senders receive the echo.
        let msg = make_command(
            "test-room",
            "alice",
            "taskboard",
            vec!["post".to_owned(), "test task description".to_owned()],
        );
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply for plugin broadcast");
        };
        assert!(
            json.contains("plugin:taskboard"),
            "reply should identify plugin source"
        );
        assert!(
            json.contains("test task description"),
            "reply should contain the task description"
        );
    }

    #[tokio::test]
    async fn plugin_reply_returns_reply_not_handled_with_reply() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        // /taskboard list with no tasks produces PluginResult::Reply.
        let msg = make_command("test-room", "alice", "taskboard", vec!["list".to_owned()]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply for plugin list");
        };
        assert!(
            json.contains("plugin:taskboard"),
            "reply should identify plugin source"
        );
    }

    // ── route_command: help ───────────────────────────────────────────────

    #[tokio::test]
    async fn help_no_args_lists_all_commands() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "help", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply for /help");
        };
        assert!(json.contains("available commands:"));
        assert!(json.contains("/who"));
        assert!(json.contains("/help"));
        // Plugin commands should also appear
        assert!(json.contains("/stats"));
    }

    #[tokio::test]
    async fn help_specific_builtin_command() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "help", vec!["who".to_owned()]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply for /help who");
        };
        assert!(json.contains("/who"));
        assert!(json.contains("List users in the room"));
    }

    #[tokio::test]
    async fn help_specific_plugin_command() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "help", vec!["stats".to_owned()]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply for /help stats");
        };
        assert!(json.contains("/stats"));
        assert!(json.contains("statistical summary"));
    }

    #[tokio::test]
    async fn help_unknown_command() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "help", vec!["nonexistent".to_owned()]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply for /help nonexistent");
        };
        assert!(json.contains("unknown command: /nonexistent"));
    }

    #[tokio::test]
    async fn help_strips_leading_slash_from_arg() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "help", vec!["/kick".to_owned()]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply for /help /kick");
        };
        assert!(json.contains("/kick"));
        assert!(json.contains("parameters:"));
        assert!(json.contains("username"));
    }

    #[tokio::test]
    async fn help_builtin_shows_param_info() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "help", vec!["kick".to_owned()]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply for /help kick");
        };
        assert!(json.contains("parameters:"));
        assert!(json.contains("username"));
        assert!(json.contains("required"));
    }

    #[tokio::test]
    async fn help_reply_comes_from_broker_not_plugin() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "help", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply");
        };
        // Should be from "broker", not "plugin:help"
        assert!(json.contains("\"user\":\"broker\""));
        assert!(!json.contains("plugin:help"));
    }

    // ── who_all ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn who_all_standalone_falls_back_to_room_users() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        // Add a user to the status map so /who_all has something to return.
        state
            .status_map
            .lock()
            .await
            .insert("alice".to_owned(), "coding".to_owned());
        let msg = make_command("test-room", "alice", "who_all", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply");
        };
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let content = v["content"].as_str().unwrap();
        assert!(content.starts_with("users_all: "));
        let users_json = content.strip_prefix("users_all: ").unwrap();
        let users: Vec<String> = serde_json::from_str(users_json).unwrap();
        assert!(users.contains(&"alice".to_owned()));
    }

    #[tokio::test]
    async fn who_all_with_registry_returns_all_registered_users() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());

        // Create a UserRegistry with multiple users.
        let reg_dir = tempfile::tempdir().unwrap();
        let mut registry = crate::registry::UserRegistry::new(reg_dir.path().to_path_buf());
        registry.register_user("alice").unwrap();
        registry.register_user("bob").unwrap();
        registry.register_user("charlie").unwrap();
        let arc_reg = Arc::new(tokio::sync::Mutex::new(registry));
        state.set_registry(arc_reg);

        let msg = make_command("test-room", "alice", "who_all", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply");
        };
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let content = v["content"].as_str().unwrap();
        let users_json = content.strip_prefix("users_all: ").unwrap();
        let users: Vec<String> = serde_json::from_str(users_json).unwrap();
        assert_eq!(users, ["alice", "bob", "charlie"]);
    }

    // ── extract_room_flag tests ──────────────────────────────────────────

    mod cross_room_tests {
        use super::super::super::plugin::extract_room_flag;

        #[test]
        fn extract_room_flag_present_at_start() {
            let params = vec![
                "--room".to_owned(),
                "other-room".to_owned(),
                "post".to_owned(),
                "hello".to_owned(),
            ];
            let (room, cleaned) = extract_room_flag(&params).unwrap();
            assert_eq!(room, "other-room");
            assert_eq!(cleaned, vec!["post", "hello"]);
        }

        #[test]
        fn extract_room_flag_present_in_middle() {
            let params = vec![
                "post".to_owned(),
                "--room".to_owned(),
                "other-room".to_owned(),
                "hello".to_owned(),
            ];
            let (room, cleaned) = extract_room_flag(&params).unwrap();
            assert_eq!(room, "other-room");
            assert_eq!(cleaned, vec!["post", "hello"]);
        }

        #[test]
        fn extract_room_flag_absent() {
            let params = vec!["post".to_owned(), "hello".to_owned()];
            assert!(extract_room_flag(&params).is_none());
        }

        #[test]
        fn extract_room_flag_no_value() {
            // --room at end without a value — not extracted
            let params = vec!["post".to_owned(), "--room".to_owned()];
            assert!(extract_room_flag(&params).is_none());
        }

        #[test]
        fn extract_room_flag_preserves_order() {
            let params = vec![
                "list".to_owned(),
                "--room".to_owned(),
                "target".to_owned(),
                "--verbose".to_owned(),
            ];
            let (room, cleaned) = extract_room_flag(&params).unwrap();
            assert_eq!(room, "target");
            assert_eq!(cleaned, vec!["list", "--verbose"]);
        }
    }

    // ── cross-room dispatch tests ────────────────────────────────────────

    #[tokio::test]
    async fn cross_room_without_resolver_returns_error() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        // No cross_room_resolver set — should get an error reply.
        let msg = make_command(
            "test-room",
            "alice",
            "taskboard",
            vec![
                "list".to_owned(),
                "--room".to_owned(),
                "other-room".to_owned(),
            ],
        );
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply with error");
        };
        assert!(
            json.contains("daemon mode"),
            "should mention daemon mode: {json}"
        );
    }

    #[tokio::test]
    async fn cross_room_nonexistent_room_returns_error() {
        use crate::broker::service::CrossRoomResolver;
        use std::pin::Pin;

        struct EmptyResolver;
        impl CrossRoomResolver for EmptyResolver {
            fn resolve_room(
                &self,
                _room_id: &str,
            ) -> Pin<Box<dyn std::future::Future<Output = Option<Arc<RoomState>>> + Send + '_>>
            {
                Box::pin(async { None })
            }
        }

        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        state.set_cross_room_resolver(Arc::new(EmptyResolver));

        let msg = make_command(
            "test-room",
            "alice",
            "taskboard",
            vec![
                "list".to_owned(),
                "--room".to_owned(),
                "nonexistent".to_owned(),
            ],
        );
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply with not-found error");
        };
        assert!(
            json.contains("room not found"),
            "should say room not found: {json}"
        );
    }

    #[tokio::test]
    async fn cross_room_resolves_and_dispatches_to_target() {
        use crate::broker::service::CrossRoomResolver;
        use std::pin::Pin;

        let tmp_source = NamedTempFile::new().unwrap();
        let source_state = make_state(tmp_source.path().to_path_buf());

        let tmp_target = NamedTempFile::new().unwrap();
        let target_state = make_state(tmp_target.path().to_path_buf());
        let target_clone = target_state.clone();

        struct OneRoomResolver(Arc<RoomState>);
        impl CrossRoomResolver for OneRoomResolver {
            fn resolve_room(
                &self,
                room_id: &str,
            ) -> Pin<Box<dyn std::future::Future<Output = Option<Arc<RoomState>>> + Send + '_>>
            {
                let result = if room_id == "target-room" {
                    Some(self.0.clone())
                } else {
                    None
                };
                Box::pin(async move { result })
            }
        }

        source_state.set_cross_room_resolver(Arc::new(OneRoomResolver(target_clone)));

        // /taskboard post --room target-room hello world
        let msg = make_command(
            "test-room",
            "alice",
            "taskboard",
            vec![
                "post".to_owned(),
                "--room".to_owned(),
                "target-room".to_owned(),
                "hello".to_owned(),
                "world".to_owned(),
            ],
        );
        let result = route_command(msg, "alice", &source_state).await.unwrap();
        // The taskboard plugin should handle the post and return a reply or broadcast
        // to the TARGET room. The exact result depends on the taskboard plugin,
        // but it should NOT be Passthrough (which would mean the command wasn't handled).
        assert!(
            !matches!(result, CommandResult::Passthrough(_)),
            "cross-room dispatch should not fall through to Passthrough"
        );
    }

    // ── Plugin panic safety (#603) ──────────────────────────────────────

    #[tokio::test]
    async fn dispatch_plugin_catches_panic() {
        use crate::plugin::{BoxFuture, CommandContext, CommandInfo, Plugin, PluginResult};

        struct PanicPlugin;

        impl Plugin for PanicPlugin {
            fn name(&self) -> &str {
                "panic-test"
            }

            fn commands(&self) -> Vec<CommandInfo> {
                vec![CommandInfo {
                    name: "boom".to_owned(),
                    description: "panics on purpose".to_owned(),
                    usage: "/boom".to_owned(),
                    params: vec![],
                }]
            }

            fn handle(&self, _ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>> {
                Box::pin(async { panic!("intentional test panic") })
            }
        }

        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "boom", vec![]);

        let result =
            super::super::plugin::dispatch_plugin(&PanicPlugin, &msg, "alice", &state).await;

        // Must not propagate the panic — should return Ok with an error reply
        let result = result.expect("dispatch_plugin should not propagate panic");
        match result {
            CommandResult::Reply(json) => {
                assert!(
                    json.contains("panicked"),
                    "reply should mention panic: {json}"
                );
                assert!(
                    json.contains("intentional test panic"),
                    "reply should include panic message: {json}"
                );
            }
            other => panic!(
                "expected CommandResult::Reply, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[tokio::test]
    async fn dispatch_plugin_catches_panic_with_string_message() {
        use crate::plugin::{BoxFuture, CommandContext, CommandInfo, Plugin, PluginResult};

        struct StringPanicPlugin;

        impl Plugin for StringPanicPlugin {
            fn name(&self) -> &str {
                "string-panic"
            }

            fn commands(&self) -> Vec<CommandInfo> {
                vec![CommandInfo {
                    name: "kaboom".to_owned(),
                    description: "panics with String".to_owned(),
                    usage: "/kaboom".to_owned(),
                    params: vec![],
                }]
            }

            fn handle(&self, _ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>> {
                Box::pin(async { panic!("{}", String::from("owned string panic")) })
            }
        }

        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "kaboom", vec![]);

        let result =
            super::super::plugin::dispatch_plugin(&StringPanicPlugin, &msg, "alice", &state)
                .await
                .expect("should not propagate panic");

        match result {
            CommandResult::Reply(json) => {
                assert!(
                    json.contains("owned string panic"),
                    "reply should include panic message: {json}"
                );
            }
            other => panic!(
                "expected CommandResult::Reply, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[tokio::test]
    async fn dispatch_plugin_normal_execution_unaffected() {
        use crate::plugin::{BoxFuture, CommandContext, CommandInfo, Plugin, PluginResult};

        struct OkPlugin;

        impl Plugin for OkPlugin {
            fn name(&self) -> &str {
                "ok-test"
            }

            fn commands(&self) -> Vec<CommandInfo> {
                vec![CommandInfo {
                    name: "greet".to_owned(),
                    description: "says hello".to_owned(),
                    usage: "/greet".to_owned(),
                    params: vec![],
                }]
            }

            fn handle(&self, _ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>> {
                Box::pin(async { Ok(PluginResult::Reply("hello".to_owned(), None)) })
            }
        }

        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "greet", vec![]);

        let result = super::super::plugin::dispatch_plugin(&OkPlugin, &msg, "alice", &state)
            .await
            .expect("normal plugin should succeed");

        match result {
            CommandResult::Reply(json) => {
                assert!(
                    json.contains("hello"),
                    "reply should contain plugin response: {json}"
                );
            }
            other => panic!(
                "expected CommandResult::Reply, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    // ── Plugin timeout (#602) ───────────────────────────────────────────

    #[tokio::test]
    async fn dispatch_plugin_times_out_slow_plugin() {
        use crate::plugin::{BoxFuture, CommandContext, CommandInfo, Plugin, PluginResult};

        struct SlowPlugin;

        impl Plugin for SlowPlugin {
            fn name(&self) -> &str {
                "slow-test"
            }

            fn commands(&self) -> Vec<CommandInfo> {
                vec![CommandInfo {
                    name: "slow".to_owned(),
                    description: "hangs forever".to_owned(),
                    usage: "/slow".to_owned(),
                    params: vec![],
                }]
            }

            fn handle(&self, _ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>> {
                Box::pin(async {
                    // Sleep longer than PLUGIN_TIMEOUT
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    Ok(PluginResult::Reply("should not reach".to_owned(), None))
                })
            }
        }

        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "slow", vec![]);

        // Override PLUGIN_TIMEOUT for test by using a short timeout directly.
        // We can't change the const, so we test the actual dispatch_plugin
        // which uses the 30s timeout. For a fast test, we test the timeout
        // machinery via tokio::time::pause.
        tokio::time::pause();

        let result =
            super::super::plugin::dispatch_plugin(&SlowPlugin, &msg, "alice", &state).await;

        let result = result.expect("dispatch_plugin should not error on timeout");
        match result {
            CommandResult::Reply(json) => {
                assert!(
                    json.contains("timed out"),
                    "reply should mention timeout: {json}"
                );
                assert!(
                    json.contains("slow-test"),
                    "reply should mention plugin name: {json}"
                );
            }
            other => panic!(
                "expected CommandResult::Reply for timeout, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }
}
