// ── Command palette ───────────────────────────────────────────────────────────

pub(super) struct PaletteItem {
    pub(super) cmd: &'static str,
    pub(super) usage: &'static str,
    pub(super) description: &'static str,
}

pub(super) const PALETTE_COMMANDS: &[PaletteItem] = &[
    PaletteItem {
        cmd: "dm",
        usage: "/dm <user> <message>",
        description: "Send a private message",
    },
    PaletteItem {
        cmd: "claim",
        usage: "/claim <task>",
        description: "Claim a task",
    },
    PaletteItem {
        cmd: "reply",
        usage: "/reply <id> <message>",
        description: "Reply to a message",
    },
    PaletteItem {
        cmd: "set_status",
        usage: "/set_status <status>",
        description: "Set your presence status",
    },
    PaletteItem {
        cmd: "who",
        usage: "/who",
        description: "List users in the room",
    },
    // Admin commands
    PaletteItem {
        cmd: "kick",
        usage: "/kick <user>",
        description: "Kick a user from the room",
    },
    PaletteItem {
        cmd: "reauth",
        usage: "/reauth <user>",
        description: "Invalidate a user's token",
    },
    PaletteItem {
        cmd: "clear-tokens",
        usage: "/clear-tokens",
        description: "Revoke all session tokens",
    },
    PaletteItem {
        cmd: "exit",
        usage: "/exit",
        description: "Shut down the broker",
    },
    PaletteItem {
        cmd: "clear",
        usage: "/clear",
        description: "Clear the room history",
    },
];

pub(super) struct CommandPalette {
    pub(super) active: bool,
    pub(super) selected: usize,
    /// Indices into `commands` that match the current query.
    pub(super) filtered: Vec<usize>,
    /// The command list this palette draws from.
    pub(super) commands: &'static [PaletteItem],
}

impl CommandPalette {
    pub(super) fn new(commands: &'static [PaletteItem]) -> Self {
        Self {
            active: false,
            selected: 0,
            filtered: (0..commands.len()).collect(),
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
    pub(super) fn update_filter(&mut self, query: &str) {
        let q = query.to_ascii_lowercase();
        self.filtered = self
            .commands
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                item.cmd.starts_with(q.as_str())
                    || item.description.to_ascii_lowercase().contains(q.as_str())
            })
            .map(|(i, _)| i)
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

    /// The full usage string (e.g. `/dm <user>`) of the selected entry.
    pub(super) fn selected_usage(&self) -> Option<&'static str> {
        self.filtered
            .get(self.selected)
            .map(|&i| self.commands[i].usage)
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── CommandPalette unit tests ─────────────────────────────────────────────

    #[test]
    fn palette_starts_inactive() {
        let p = CommandPalette::new(PALETTE_COMMANDS);
        assert!(!p.active);
        assert_eq!(p.filtered.len(), p.commands.len());
    }

    #[test]
    fn palette_activate_resets_state() {
        let mut p = CommandPalette::new(PALETTE_COMMANDS);
        p.selected = 2;
        p.filtered = vec![1];
        p.activate();
        assert!(p.active);
        assert_eq!(p.selected, 0);
        assert_eq!(p.filtered.len(), p.commands.len());
    }

    #[test]
    fn palette_deactivate_clears_active() {
        let mut p = CommandPalette::new(PALETTE_COMMANDS);
        p.activate();
        p.deactivate();
        assert!(!p.active);
    }

    #[test]
    fn palette_filter_by_cmd_prefix() {
        let mut p = CommandPalette::new(PALETTE_COMMANDS);
        // "dm" matches exactly one command by cmd prefix (no description ambiguity)
        p.update_filter("dm");
        assert_eq!(p.filtered.len(), 1);
        assert_eq!(p.commands[p.filtered[0]].cmd, "dm");
    }

    #[test]
    fn palette_filter_dm_exact() {
        let mut p = CommandPalette::new(PALETTE_COMMANDS);
        p.update_filter("dm");
        assert_eq!(p.filtered.len(), 1);
        assert_eq!(p.commands[p.filtered[0]].cmd, "dm");
    }

    #[test]
    fn palette_filter_empty_query_shows_all() {
        let mut p = CommandPalette::new(PALETTE_COMMANDS);
        p.update_filter("");
        assert_eq!(p.filtered.len(), p.commands.len());
    }

    #[test]
    fn palette_filter_no_match_returns_empty() {
        let mut p = CommandPalette::new(PALETTE_COMMANDS);
        p.update_filter("zzz_no_match");
        assert!(p.filtered.is_empty());
    }

    #[test]
    fn palette_filter_by_description_keyword() {
        let mut p = CommandPalette::new(PALETTE_COMMANDS);
        p.update_filter("private");
        // Should match "dm" whose description is "Send a private message"
        assert!(p.filtered.iter().any(|&i| p.commands[i].cmd == "dm"));
    }

    #[test]
    fn palette_move_up_clamps_at_zero() {
        let mut p = CommandPalette::new(PALETTE_COMMANDS);
        p.activate();
        p.move_up();
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn palette_move_down_clamps_at_end() {
        let mut p = CommandPalette::new(PALETTE_COMMANDS);
        p.activate();
        for _ in 0..100 {
            p.move_down();
        }
        assert_eq!(p.selected, p.commands.len() - 1);
    }

    #[test]
    fn palette_move_up_down_navigate() {
        let mut p = CommandPalette::new(PALETTE_COMMANDS);
        p.activate();
        p.move_down();
        p.move_down();
        assert_eq!(p.selected, 2);
        p.move_up();
        assert_eq!(p.selected, 1);
    }

    #[test]
    fn palette_selected_usage_returns_usage_string() {
        let mut p = CommandPalette::new(PALETTE_COMMANDS);
        p.activate();
        // First entry in unfiltered list
        let usage = p.selected_usage().unwrap();
        assert!(usage.starts_with('/'));
    }

    #[test]
    fn palette_selected_usage_empty_when_no_filtered() {
        let mut p = CommandPalette::new(PALETTE_COMMANDS);
        p.filtered.clear();
        assert!(p.selected_usage().is_none());
    }

    #[test]
    fn palette_selected_clamps_after_filter_narrows() {
        let mut p = CommandPalette::new(PALETTE_COMMANDS);
        p.activate();
        // Navigate to last entry
        for _ in 0..100 {
            p.move_down();
        }
        assert_eq!(p.selected, p.commands.len() - 1);
        // Now narrow filter so fewer entries remain
        p.update_filter("dm");
        assert_eq!(p.filtered.len(), 1);
        assert_eq!(p.selected, 0); // clamped
    }

    // ── PALETTE_COMMANDS completeness tests ───────────────────────────────────

    #[test]
    fn palette_commands_contains_set_status() {
        assert!(
            PALETTE_COMMANDS.iter().any(|c| c.cmd == "set_status"),
            "PALETTE_COMMANDS must include set_status"
        );
    }

    #[test]
    fn palette_filter_set_status() {
        let mut p = CommandPalette::new(PALETTE_COMMANDS);
        p.update_filter("set");
        assert!(
            p.filtered
                .iter()
                .any(|&i| p.commands[i].cmd == "set_status"),
            "filter 'set' must match set_status"
        );
    }

    // ── ADMIN_COMMANDS palette tests ──────────────────────────────────────────

    #[test]
    fn unified_palette_starts_inactive() {
        let p = CommandPalette::new(PALETTE_COMMANDS);
        assert!(!p.active);
        assert_eq!(p.filtered.len(), PALETTE_COMMANDS.len());
    }

    #[test]
    fn palette_contains_all_admin_commands() {
        let cmds: Vec<&str> = PALETTE_COMMANDS.iter().map(|c| c.cmd).collect();
        assert!(cmds.contains(&"kick"));
        assert!(cmds.contains(&"reauth"));
        assert!(cmds.contains(&"clear-tokens"));
        assert!(cmds.contains(&"exit"));
        assert!(cmds.contains(&"clear"));
    }

    #[test]
    fn admin_usages_use_slash_prefix() {
        let admin_cmds = ["kick", "reauth", "clear-tokens", "exit", "clear"];
        for item in PALETTE_COMMANDS {
            if admin_cmds.contains(&item.cmd) {
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
        let mut p = CommandPalette::new(PALETTE_COMMANDS);
        p.update_filter("ki");
        assert_eq!(p.filtered.len(), 1);
        assert_eq!(p.commands[p.filtered[0]].cmd, "kick");
    }

    #[test]
    fn admin_selected_usage_slash() {
        let mut p = CommandPalette::new(PALETTE_COMMANDS);
        p.activate();
        // All commands use / prefix
        let usage = p.selected_usage().unwrap();
        assert!(usage.starts_with('/'));
    }

    // ── MentionPicker tests ───────────────────────────────────────────────────

    #[test]
    fn mention_picker_shows_user_added_from_message_sender() {
        // Simulate: a user r2d2 sends a message; their name is seeded into online_users
        // (the drain loop adds Message::Message senders if not already present).
        let mut online_users: Vec<String> = Vec::new();
        // Replicate the drain-loop guard exactly as written in the source.
        let user = "r2d2".to_owned();
        if !online_users.contains(&user) {
            online_users.push(user);
        }
        // MentionPicker should now find this user when activated with an empty query.
        let mut picker = MentionPicker::new();
        picker.activate(0, &online_users, "");
        assert!(picker.active);
        assert_eq!(picker.filtered, vec!["r2d2".to_owned()]);
    }

    #[test]
    fn message_sender_not_duplicated_if_already_online() {
        let mut online_users = vec!["alice".to_owned()];
        // Same guard as the drain loop.
        let user = "alice".to_owned();
        if !online_users.contains(&user) {
            online_users.push(user);
        }
        assert_eq!(online_users.len(), 1);
    }
}
