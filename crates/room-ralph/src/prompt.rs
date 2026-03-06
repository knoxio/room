use std::path::Path;

use room_protocol::Message;

/// Configuration for building a prompt.
pub struct PromptConfig<'a> {
    pub room_id: &'a str,
    pub username: &'a str,
    pub token: &'a str,
    pub custom_prompt_file: Option<&'a Path>,
    pub progress_file: &'a Path,
    pub issue: Option<&'a str>,
}

/// Build the prompt for `claude -p` from system context, progress file,
/// and recent room messages.
pub fn build_prompt(config: &PromptConfig<'_>, messages: &[Message]) -> String {
    let mut prompt = String::new();

    // System context
    if let Some(custom) = config.custom_prompt_file {
        if let Ok(content) = std::fs::read_to_string(custom) {
            prompt.push_str(&content);
        }
    } else {
        prompt.push_str(&format!(
            "You are {}, an autonomous agent in room {}.",
            config.username, config.room_id
        ));
        prompt.push_str(&format!(
            " You communicate via the room CLI. Your token is {}.\n\n",
            config.token
        ));
        prompt.push_str("Commands available:\n");
        prompt.push_str(&format!(
            "  room send {} -t {} '<message>'  -- send a message\n",
            config.room_id, config.token
        ));
        prompt.push_str(&format!(
            "  room poll {} -t {}              -- check for new messages\n",
            config.room_id, config.token
        ));
        prompt.push_str(&format!(
            "  room watch {} -t {} --interval 2 -- block until a message arrives\n\n",
            config.room_id, config.token
        ));
        prompt.push_str("Rules:\n");
        prompt.push_str("- Announce your plan before writing code\n");
        prompt.push_str("- One concern per PR\n");
        prompt.push_str("- Run scripts/pre-push.sh before pushing\n");
        prompt.push_str("- Check room assignments before committing fixes\n");
        prompt.push_str(&format!(
            "- Write progress to {} at each milestone\n\n",
            config.progress_file.display()
        ));
    }

    // Progress file from previous iteration
    if config.progress_file.exists() {
        if let Ok(content) = std::fs::read_to_string(config.progress_file) {
            prompt.push_str("--- PROGRESS FROM PREVIOUS CONTEXT ---\n");
            prompt.push_str(&content);
            prompt.push_str("\n--- END PROGRESS ---\n\n");
        }
    }

    // Recent room messages
    if !messages.is_empty() {
        prompt.push_str("--- RECENT ROOM MESSAGES ---\n");
        for msg in messages {
            if let Ok(json) = serde_json::to_string(msg) {
                prompt.push_str(&json);
                prompt.push('\n');
            }
        }
        prompt.push_str("--- END MESSAGES ---\n\n");
    }

    // Task context
    if let Some(issue) = config.issue {
        prompt.push_str(&format!(
            "Your current assignment is GitHub issue #{issue}. \
             Work on this issue, coordinate in the room, and update progress."
        ));
    } else {
        prompt.push_str("Poll the room for assignments. Work on whatever is assigned to you.");
    }

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn build_prompt_default_system_context() {
        let progress = PathBuf::from("/tmp/test-nonexistent-progress.md");
        let config = PromptConfig {
            room_id: "myroom",
            username: "agent1",
            token: "tok-123",
            custom_prompt_file: None,
            progress_file: &progress,
            issue: Some("42"),
        };
        let prompt = build_prompt(&config, &[]);
        assert!(prompt.contains("You are agent1"));
        assert!(prompt.contains("room myroom"));
        assert!(prompt.contains("tok-123"));
        assert!(prompt.contains("issue #42"));
        assert!(prompt.contains("room send myroom"));
    }

    #[test]
    fn build_prompt_with_messages() {
        let progress = PathBuf::from("/tmp/test-nonexistent-progress.md");
        let config = PromptConfig {
            room_id: "r",
            username: "u",
            token: "t",
            custom_prompt_file: None,
            progress_file: &progress,
            issue: None,
        };
        let msg = room_protocol::make_message("r", "bob", "hello");
        let prompt = build_prompt(&config, &[msg]);
        assert!(prompt.contains("RECENT ROOM MESSAGES"));
        assert!(prompt.contains("hello"));
        assert!(prompt.contains("Poll the room for assignments"));
    }

    #[test]
    fn build_prompt_no_issue_gives_generic_instruction() {
        let progress = PathBuf::from("/tmp/test-nonexistent-progress.md");
        let config = PromptConfig {
            room_id: "r",
            username: "u",
            token: "t",
            custom_prompt_file: None,
            progress_file: &progress,
            issue: None,
        };
        let prompt = build_prompt(&config, &[]);
        assert!(prompt.contains("Poll the room for assignments"));
        assert!(!prompt.contains("issue #"));
    }

    #[test]
    fn build_prompt_with_progress_file() {
        let progress = PathBuf::from("/tmp/test-ralph-progress-build.md");
        std::fs::write(&progress, "## Status\nIn progress").ok();
        let config = PromptConfig {
            room_id: "r",
            username: "u",
            token: "t",
            custom_prompt_file: None,
            progress_file: &progress,
            issue: None,
        };
        let prompt = build_prompt(&config, &[]);
        assert!(prompt.contains("PROGRESS FROM PREVIOUS CONTEXT"));
        assert!(prompt.contains("In progress"));
        std::fs::remove_file(&progress).ok();
    }
}
