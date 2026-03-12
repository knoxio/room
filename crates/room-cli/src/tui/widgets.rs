use crate::plugin::{CommandInfo, ParamType};

// ── Command palette ───────────────────────────────────────────────────────────

/// A single entry in the command palette.
///
/// Constructed from [`CommandInfo`] schemas via [`CommandPalette::from_commands`],
/// or directly for testing. Owns its strings so the palette can be built
/// dynamically at runtime from the plugin registry.
pub(super) struct PaletteItem {
    pub(super) cmd: String,
    pub(super) usage: String,
    pub(super) description: String,
    /// Typed parameter schemas — drives argument-level autocomplete.
    pub(super) params: Vec<crate::plugin::ParamSchema>,
}

pub(super) struct CommandPalette {
    pub(super) active: bool,
    pub(super) selected: usize,
    /// Indices into `commands` that match the current query.
    pub(super) filtered: Vec<usize>,
    /// The command list this palette draws from.
    pub(super) commands: Vec<PaletteItem>,
}

impl CommandPalette {
    /// Build a palette from a list of [`CommandInfo`] schemas.
    pub(super) fn from_commands(infos: Vec<CommandInfo>) -> Self {
        let commands: Vec<PaletteItem> = infos
            .into_iter()
            .map(|c| PaletteItem {
                cmd: c.name,
                usage: c.usage,
                description: c.description,
                params: c.params,
            })
            .collect();
        let filtered = (0..commands.len()).collect();
        Self {
            active: false,
            selected: 0,
            filtered,
            commands,
        }
    }

    pub(super) fn activate(&mut self) {
        self.active = true;
        self.selected = 0;
        self.filtered = (0..self.commands.len()).collect();
    }

    pub(super) fn deactivate(&mut self) {
        self.active = false;
    }

    /// Update the filtered list based on the query typed after the trigger character.
    ///
    /// Command-name prefix matches are ranked above description-only matches so
    /// that e.g. `/help` appears before `/who` when the user types "he".
    pub(super) fn update_filter(&mut self, query: &str) {
        let q = query.to_ascii_lowercase();
        let mut prefix_matches: Vec<usize> = Vec::new();
        let mut desc_matches: Vec<usize> = Vec::new();
        for (i, item) in self.commands.iter().enumerate() {
            if item.cmd.starts_with(q.as_str()) {
                prefix_matches.push(i);
            } else if item.description.to_ascii_lowercase().contains(q.as_str()) {
                desc_matches.push(i);
            }
        }
        prefix_matches.extend(desc_matches);
        self.filtered = prefix_matches;
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }

    pub(super) fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub(super) fn move_down(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = (self.selected + 1).min(self.filtered.len() - 1);
        }
    }

    /// The full usage string (e.g. `/dm <user>`) of the selected entry.
    pub(super) fn selected_usage(&self) -> Option<&str> {
        self.filtered
            .get(self.selected)
            .map(|&i| self.commands[i].usage.as_str())
    }

    /// Look up a command by name and return completions for the given argument
    /// position. Returns `Choice` values or an empty vec.
    pub(super) fn completions_at(&self, cmd_name: &str, arg_pos: usize) -> Vec<String> {
        self.commands
            .iter()
            .find(|c| c.cmd == cmd_name)
            .and_then(|c| c.params.get(arg_pos))
            .map(|p| match &p.param_type {
                ParamType::Choice(values) => values.clone(),
                _ => vec![],
            })
            .unwrap_or_default()
    }

    /// Check if the parameter at `arg_pos` for `cmd_name` is a `Username` type
    /// (for triggering the mention picker instead of a choice picker).
    pub(super) fn is_username_param(&self, cmd_name: &str, arg_pos: usize) -> bool {
        self.commands
            .iter()
            .find(|c| c.cmd == cmd_name)
            .and_then(|c| c.params.get(arg_pos))
            .map(|p| matches!(p.param_type, ParamType::Username))
            .unwrap_or(false)
    }
}

// ── Mention picker ────────────────────────────────────────────────────────────

/// Autocomplete popup for `@username` mentions.
pub(super) struct MentionPicker {
    pub(super) active: bool,
    pub(super) selected: usize,
    /// Prefix-filtered list of matching online usernames.
    pub(super) filtered: Vec<String>,
    /// Byte index of the `@` character in the input buffer that opened this picker.
    pub(super) at_byte: usize,
}

impl MentionPicker {
    pub(super) fn new() -> Self {
        Self {
            active: false,
            selected: 0,
            filtered: Vec::new(),
            at_byte: 0,
        }
    }

    pub(super) fn activate(&mut self, at_byte: usize, online_users: &[String], query: &str) {
        self.active = true;
        self.selected = 0;
        self.at_byte = at_byte;
        self.update_filter(online_users, query);
    }

    pub(super) fn deactivate(&mut self) {
        self.active = false;
    }

    pub(super) fn update_filter(&mut self, online_users: &[String], query: &str) {
        let q = query.to_ascii_lowercase();
        self.filtered = online_users
            .iter()
            .filter(|u| u.to_ascii_lowercase().starts_with(q.as_str()))
            .cloned()
            .collect();
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }

    pub(super) fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub(super) fn move_down(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = (self.selected + 1).min(self.filtered.len() - 1);
        }
    }

    pub(super) fn selected_user(&self) -> Option<&str> {
        self.filtered.get(self.selected).map(|s| s.as_str())
    }
}

// ── Choice picker ────────────────────────────────────────────────────────────

/// Autocomplete popup for `ParamType::Choice` parameters.
///
/// Mirrors [`MentionPicker`] but shows a filtered list of predefined choice
/// values instead of online usernames. Activated when the user selects a
/// command whose first parameter is `Choice` from the command palette.
pub(super) struct ChoicePicker {
    pub(super) active: bool,
    pub(super) selected: usize,
    /// Prefix-filtered list of matching choices.
    pub(super) filtered: Vec<String>,
    /// The full set of valid choices for the current parameter.
    all_choices: Vec<String>,
    /// The command name this picker is completing for.
    pub(super) cmd_name: String,
    /// Byte index in the input buffer where the choice value starts.
    pub(super) value_start: usize,
}

impl ChoicePicker {
    pub(super) fn new() -> Self {
        Self {
            active: false,
            selected: 0,
            filtered: Vec::new(),
            all_choices: Vec::new(),
            cmd_name: String::new(),
            value_start: 0,
        }
    }

    pub(super) fn activate(
        &mut self,
        cmd_name: &str,
        choices: Vec<String>,
        value_start: usize,
        query: &str,
    ) {
        self.active = true;
        self.selected = 0;
        self.cmd_name = cmd_name.to_owned();
        self.all_choices = choices;
        self.value_start = value_start;
        self.update_filter(query);
    }

    pub(super) fn deactivate(&mut self) {
        self.active = false;
    }

    pub(super) fn update_filter(&mut self, query: &str) {
        let q = query.to_ascii_lowercase();
        self.filtered = self
            .all_choices
            .iter()
            .filter(|c| c.to_ascii_lowercase().starts_with(q.as_str()))
            .cloned()
            .collect();
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }

    pub(super) fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub(super) fn move_down(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = (self.selected + 1).min(self.filtered.len() - 1);
        }
    }

    pub(super) fn selected_value(&self) -> Option<&str> {
        self.filtered.get(self.selected).map(|s| s.as_str())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::all_known_commands;

    fn make_palette() -> CommandPalette {
        CommandPalette::from_commands(all_known_commands())
    }

    // ── CommandPalette unit tests ─────────────────────────────────────────────

    #[test]
    fn palette_starts_inactive() {
        let p = make_palette();
        assert!(!p.active);
        assert_eq!(p.filtered.len(), p.commands.len());
    }

    #[test]
    fn palette_activate_resets_state() {
        let mut p = make_palette();
        p.selected = 2;
        p.filtered = vec![1];
        p.activate();
        assert!(p.active);
        assert_eq!(p.selected, 0);
        assert_eq!(p.filtered.len(), p.commands.len());
    }

    #[test]
    fn palette_deactivate_clears_active() {
        let mut p = make_palette();
        p.activate();
        p.deactivate();
        assert!(!p.active);
    }

    #[test]
    fn palette_filter_by_cmd_prefix() {
        let mut p = make_palette();
        p.update_filter("dm");
        assert_eq!(p.filtered.len(), 1);
        assert_eq!(p.commands[p.filtered[0]].cmd, "dm");
    }

    #[test]
    fn palette_filter_dm_exact() {
        let mut p = make_palette();
        p.update_filter("dm");
        assert_eq!(p.filtered.len(), 1);
        assert_eq!(p.commands[p.filtered[0]].cmd, "dm");
    }

    #[test]
    fn palette_filter_empty_query_shows_all() {
        let mut p = make_palette();
        p.update_filter("");
        assert_eq!(p.filtered.len(), p.commands.len());
    }

    #[test]
    fn palette_filter_no_match_returns_empty() {
        let mut p = make_palette();
        p.update_filter("zzz_no_match");
        assert!(p.filtered.is_empty());
    }

    #[test]
    fn palette_filter_by_description_keyword() {
        let mut p = make_palette();
        p.update_filter("private");
        assert!(p.filtered.iter().any(|&i| p.commands[i].cmd == "dm"));
    }

    #[test]
    fn palette_move_up_clamps_at_zero() {
        let mut p = make_palette();
        p.activate();
        p.move_up();
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn palette_move_down_clamps_at_end() {
        let mut p = make_palette();
        p.activate();
        for _ in 0..100 {
            p.move_down();
        }
        assert_eq!(p.selected, p.commands.len() - 1);
    }

    #[test]
    fn palette_move_up_down_navigate() {
        let mut p = make_palette();
        p.activate();
        p.move_down();
        p.move_down();
        assert_eq!(p.selected, 2);
        p.move_up();
        assert_eq!(p.selected, 1);
    }

    #[test]
    fn palette_selected_usage_returns_usage_string() {
        let mut p = make_palette();
        p.activate();
        let usage = p.selected_usage().unwrap();
        assert!(usage.starts_with('/'));
    }

    #[test]
    fn palette_selected_usage_empty_when_no_filtered() {
        let mut p = make_palette();
        p.filtered.clear();
        assert!(p.selected_usage().is_none());
    }

    #[test]
    fn palette_selected_clamps_after_filter_narrows() {
        let mut p = make_palette();
        p.activate();
        for _ in 0..100 {
            p.move_down();
        }
        assert_eq!(p.selected, p.commands.len() - 1);
        p.update_filter("dm");
        assert_eq!(p.filtered.len(), 1);
        assert_eq!(p.selected, 0);
    }

    // ── Completeness tests ───────────────────────────────────────────────────

    #[test]
    fn palette_commands_contains_set_status() {
        let p = make_palette();
        assert!(
            p.commands.iter().any(|c| c.cmd == "set_status"),
            "palette must include set_status"
        );
    }

    #[test]
    fn palette_filter_set_status() {
        let mut p = make_palette();
        p.update_filter("set");
        assert!(
            p.filtered
                .iter()
                .any(|&i| p.commands[i].cmd == "set_status"),
            "filter 'set' must match set_status"
        );
    }

    #[test]
    fn palette_contains_all_admin_commands() {
        let p = make_palette();
        let cmds: Vec<&str> = p.commands.iter().map(|c| c.cmd.as_str()).collect();
        assert!(cmds.contains(&"kick"));
        assert!(cmds.contains(&"reauth"));
        assert!(cmds.contains(&"clear-tokens"));
        assert!(cmds.contains(&"exit"));
        assert!(cmds.contains(&"clear"));
    }

    #[test]
    fn admin_usages_use_slash_prefix() {
        let p = make_palette();
        let admin_cmds = ["kick", "reauth", "clear-tokens", "exit", "clear"];
        for item in &p.commands {
            if admin_cmds.contains(&item.cmd.as_str()) {
                assert!(
                    item.usage.starts_with('/'),
                    "admin command '{}' usage must start with /",
                    item.cmd
                );
            }
        }
    }

    #[test]
    fn palette_filter_kick() {
        let mut p = make_palette();
        p.update_filter("ki");
        assert_eq!(p.filtered.len(), 1);
        assert_eq!(p.commands[p.filtered[0]].cmd, "kick");
    }

    #[test]
    fn admin_selected_usage_slash() {
        let mut p = make_palette();
        p.activate();
        let usage = p.selected_usage().unwrap();
        assert!(usage.starts_with('/'));
    }

    // ── Filter ranking tests (#172) ────────────────────────────────────────────

    #[test]
    fn palette_filter_ranks_prefix_before_description() {
        let mut p = make_palette();
        p.update_filter("se");
        assert!(
            p.filtered.len() >= 2,
            "expected at least 2 matches for 'se', got {}",
            p.filtered.len()
        );
        let first_cmd = &p.commands[p.filtered[0]].cmd;
        assert_eq!(
            first_cmd, "set_status",
            "first match should be 'set_status' (prefix), not '{first_cmd}'"
        );
        let prefix_count = p
            .filtered
            .iter()
            .filter(|&&i| p.commands[i].cmd.starts_with("se"))
            .count();
        for (pos, &i) in p.filtered.iter().enumerate() {
            if !p.commands[i].cmd.starts_with("se") {
                assert!(
                    pos >= prefix_count,
                    "description-only match '{}' at position {} should come after {} prefix matches",
                    p.commands[i].cmd,
                    pos,
                    prefix_count
                );
            }
        }
    }

    #[test]
    fn palette_filter_description_match_appears_after_all_prefix_matches() {
        let mut p = make_palette();
        p.update_filter("re");
        let prefix_count = p
            .filtered
            .iter()
            .filter(|&&i| p.commands[i].cmd.starts_with("re"))
            .count();
        assert!(prefix_count >= 2, "expected at least reply + reauth");
        for (pos, &i) in p.filtered.iter().enumerate() {
            if !p.commands[i].cmd.starts_with("re") {
                assert!(
                    pos >= prefix_count,
                    "description-only match '{}' at position {} should come after {} prefix matches",
                    p.commands[i].cmd,
                    pos,
                    prefix_count
                );
            }
        }
    }

    // ── completions_at / is_username_param tests ──────────────────────────────

    #[test]
    fn completions_at_returns_choice_values() {
        let p = make_palette();
        let completions = p.completions_at("stats", 0);
        assert!(
            completions.contains(&"10".to_owned()),
            "stats param 0 should include '10'"
        );
        assert!(
            completions.contains(&"50".to_owned()),
            "stats param 0 should include '50'"
        );
    }

    #[test]
    fn completions_at_returns_empty_for_text_param() {
        let p = make_palette();
        // set_status param is Text — no completions
        assert!(p.completions_at("set_status", 0).is_empty());
    }

    #[test]
    fn completions_at_returns_empty_for_unknown_command() {
        let p = make_palette();
        assert!(p.completions_at("nonexistent", 0).is_empty());
    }

    #[test]
    fn is_username_param_true_for_dm_first_arg() {
        let p = make_palette();
        assert!(p.is_username_param("dm", 0));
    }

    #[test]
    fn is_username_param_true_for_kick() {
        let p = make_palette();
        assert!(p.is_username_param("kick", 0));
    }

    #[test]
    fn is_username_param_false_for_text_param() {
        let p = make_palette();
        assert!(!p.is_username_param("set_status", 0));
    }

    #[test]
    fn is_username_param_false_for_unknown_command() {
        let p = make_palette();
        assert!(!p.is_username_param("nonexistent", 0));
    }

    // ── MentionPicker tests ───────────────────────────────────────────────────

    #[test]
    fn mention_picker_shows_user_added_from_message_sender() {
        let mut online_users: Vec<String> = Vec::new();
        let user = "r2d2".to_owned();
        if !online_users.contains(&user) {
            online_users.push(user);
        }
        let mut picker = MentionPicker::new();
        picker.activate(0, &online_users, "");
        assert!(picker.active);
        assert_eq!(picker.filtered, vec!["r2d2".to_owned()]);
    }

    #[test]
    fn message_sender_not_duplicated_if_already_online() {
        let mut online_users = vec!["alice".to_owned()];
        let user = "alice".to_owned();
        if !online_users.contains(&user) {
            online_users.push(user);
        }
        assert_eq!(online_users.len(), 1);
    }

    // ── ChoicePicker tests ──────────────────────────────────────────────────

    #[test]
    fn choice_picker_starts_inactive() {
        let p = ChoicePicker::new();
        assert!(!p.active);
        assert!(p.filtered.is_empty());
    }

    #[test]
    fn choice_picker_activate_shows_all_choices() {
        let mut p = ChoicePicker::new();
        let choices = vec!["full".to_owned(), "mentions_only".to_owned()];
        p.activate("subscribe", choices.clone(), 12, "");
        assert!(p.active);
        assert_eq!(p.filtered, choices);
        assert_eq!(p.cmd_name, "subscribe");
        assert_eq!(p.value_start, 12);
    }

    #[test]
    fn choice_picker_filters_by_prefix() {
        let mut p = ChoicePicker::new();
        let choices = vec!["full".to_owned(), "mentions_only".to_owned()];
        p.activate("subscribe", choices, 12, "f");
        assert_eq!(p.filtered, vec!["full"]);
    }

    #[test]
    fn choice_picker_filter_case_insensitive() {
        let mut p = ChoicePicker::new();
        let choices = vec!["Full".to_owned(), "MentionsOnly".to_owned()];
        p.activate("subscribe", choices, 12, "m");
        assert_eq!(p.filtered, vec!["MentionsOnly"]);
    }

    #[test]
    fn choice_picker_filter_no_match() {
        let mut p = ChoicePicker::new();
        let choices = vec!["full".to_owned(), "mentions_only".to_owned()];
        p.activate("subscribe", choices, 12, "zzz");
        assert!(p.filtered.is_empty());
    }

    #[test]
    fn choice_picker_navigate_up_down() {
        let mut p = ChoicePicker::new();
        let choices = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        p.activate("test", choices, 0, "");
        assert_eq!(p.selected, 0);
        p.move_down();
        assert_eq!(p.selected, 1);
        p.move_down();
        assert_eq!(p.selected, 2);
        p.move_down(); // clamps
        assert_eq!(p.selected, 2);
        p.move_up();
        assert_eq!(p.selected, 1);
        p.move_up();
        assert_eq!(p.selected, 0);
        p.move_up(); // clamps
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn choice_picker_selected_value() {
        let mut p = ChoicePicker::new();
        let choices = vec!["full".to_owned(), "mentions_only".to_owned()];
        p.activate("subscribe", choices, 0, "");
        assert_eq!(p.selected_value(), Some("full"));
        p.move_down();
        assert_eq!(p.selected_value(), Some("mentions_only"));
    }

    #[test]
    fn choice_picker_selected_clamps_after_filter() {
        let mut p = ChoicePicker::new();
        let choices = vec!["alpha".to_owned(), "beta".to_owned(), "gamma".to_owned()];
        p.activate("test", choices, 0, "");
        p.move_down();
        p.move_down();
        assert_eq!(p.selected, 2);
        p.update_filter("a"); // only "alpha"
        assert_eq!(p.filtered.len(), 1);
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn choice_picker_deactivate() {
        let mut p = ChoicePicker::new();
        let choices = vec!["a".to_owned()];
        p.activate("test", choices, 0, "");
        assert!(p.active);
        p.deactivate();
        assert!(!p.active);
    }

    #[test]
    fn choice_picker_empty_choices() {
        let mut p = ChoicePicker::new();
        p.activate("test", vec![], 0, "");
        assert!(p.filtered.is_empty());
        assert_eq!(p.selected_value(), None);
    }
}
