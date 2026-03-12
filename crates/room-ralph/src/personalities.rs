use std::str::FromStr;

use crate::claude::Profile;

/// A compiled-in personality template that bundles a system prompt fragment,
/// tool profile, and default model for a common agent role.
///
/// Personalities are invoked by name (`--personality coder`) and set sensible
/// defaults that can be overridden by explicit CLI flags. The prompt text is
/// prepended to the system prompt, identical to how `--personality <file>`
/// worked before builtin personalities existed.
#[derive(Debug, Clone)]
pub struct Personality {
    /// Short lowercase identifier (used in CLI and `/spawn`).
    pub name: &'static str,
    /// One-line human-readable description.
    pub description: &'static str,
    /// Text prepended to the system prompt.
    pub prompt: &'static str,
    /// Tool profile that controls auto-approval and hard-blocks.
    pub profile: Profile,
    /// Default model (can be overridden by `--model`).
    pub default_model: &'static str,
}

/// All compiled-in personalities.
const BUILTINS: &[Personality] = &[
    Personality {
        name: "coder",
        description: "Writes code, runs tests, opens PRs",
        prompt: "You are a software engineer agent. Your primary job is to write clean, \
                 well-tested code. Follow the project's conventions, run the full test suite \
                 before committing, and open a PR when your work is ready. Prefer small, \
                 focused changes over large refactors. Always run `bash scripts/pre-push.sh` \
                 before pushing.",
        profile: Profile::Coder,
        default_model: "opus",
    },
    Personality {
        name: "reviewer",
        description: "Reviews PRs, checks code quality, runs clippy",
        prompt: "You are a code reviewer agent. Your job is to review pull requests for \
                 correctness, style, and potential bugs. Check that tests are included, \
                 run `cargo clippy -- -D warnings` on the branch, and leave clear, \
                 actionable feedback. You do not write code — you read and critique it. \
                 Flag security issues, performance concerns, and missing edge cases.",
        profile: Profile::Reviewer,
        default_model: "opus",
    },
    Personality {
        name: "researcher",
        description: "Reads code, searches, summarizes findings",
        prompt: "You are a research agent. Your job is to read code, search for patterns, \
                 and summarize your findings. You do not modify files — you analyze and report. \
                 When asked to investigate an issue, trace the code path end-to-end, identify \
                 root causes, and present your findings with file paths and line numbers.",
        profile: Profile::Reader,
        default_model: "sonnet",
    },
    Personality {
        name: "coordinator",
        description: "Manages tasks, coordinates agents, tracks progress",
        prompt: "You are a coordination agent (BA). Your job is to manage the sprint: \
                 assign issues to agents, track progress, resolve conflicts, and make \
                 architectural decisions. You read code and PRs but do not write code. \
                 Keep the room informed of status changes. Maintain the sprint scorecard \
                 and enforce process rules.",
        profile: Profile::Coordinator,
        default_model: "opus",
    },
    Personality {
        name: "documenter",
        description: "Writes docs, updates external systems, maintains changelogs",
        prompt: "You are a documentation agent. Your job is to write and maintain \
                 documentation: design docs, changelogs, Notion pages, and README files. \
                 You read code to understand what it does, then write clear prose that \
                 explains it. You may also update external systems like Notion. You do \
                 not modify source code.",
        profile: Profile::Notion,
        default_model: "sonnet",
    },
];

/// Look up a builtin personality by name (case-insensitive).
pub fn lookup(name: &str) -> Option<&'static Personality> {
    BUILTINS.iter().find(|p| p.name.eq_ignore_ascii_case(name))
}

/// Return all builtin personality names, for error messages and `--list-personalities`.
pub fn all_names() -> Vec<&'static str> {
    BUILTINS.iter().map(|p| p.name).collect()
}

/// Return all builtin personalities.
pub fn all() -> &'static [Personality] {
    BUILTINS
}

/// Formats the personality list for display (used by `--list-personalities`).
pub fn format_list() -> String {
    let mut out = String::from("Available personalities:\n");
    for p in BUILTINS {
        out.push_str(&format!(
            "  {:<14} {} (profile: {}, model: {})\n",
            p.name, p.description, p.profile, p.default_model
        ));
    }
    out
}

/// A resolved personality — either a compiled-in builtin or a file path
/// to a custom personality file.
#[derive(Debug, Clone)]
pub enum ResolvedPersonality {
    /// A compiled-in personality with full defaults.
    Builtin(&'static Personality),
    /// A file path to a custom personality prompt (legacy behavior).
    File(std::path::PathBuf),
}

/// Resolve a `--personality` argument: try as a builtin name first,
/// then fall back to treating it as a file path.
pub fn resolve(value: &str) -> ResolvedPersonality {
    if let Some(builtin) = lookup(value) {
        ResolvedPersonality::Builtin(builtin)
    } else {
        ResolvedPersonality::File(std::path::PathBuf::from(value))
    }
}

/// Clap value parser for `--personality` that accepts both builtin names
/// and file paths but validates that it is one or the other.
impl FromStr for ResolvedPersonality {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(resolve(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_all_builtins() {
        for name in &[
            "coder",
            "reviewer",
            "researcher",
            "coordinator",
            "documenter",
        ] {
            assert!(lookup(name).is_some(), "builtin '{}' should be found", name);
        }
    }

    #[test]
    fn lookup_case_insensitive() {
        assert!(lookup("CODER").is_some());
        assert!(lookup("Reviewer").is_some());
        assert!(lookup("RESEARCHER").is_some());
    }

    #[test]
    fn lookup_unknown_returns_none() {
        assert!(lookup("hacker").is_none());
        assert!(lookup("").is_none());
    }

    #[test]
    fn all_names_returns_five() {
        let names = all_names();
        assert_eq!(names.len(), 5);
        assert!(names.contains(&"coder"));
        assert!(names.contains(&"reviewer"));
        assert!(names.contains(&"researcher"));
        assert!(names.contains(&"coordinator"));
        assert!(names.contains(&"documenter"));
    }

    #[test]
    fn all_returns_five_personalities() {
        assert_eq!(all().len(), 5);
    }

    #[test]
    fn format_list_contains_all_names() {
        let list = format_list();
        for name in all_names() {
            assert!(list.contains(name), "list should contain '{}'", name);
        }
        assert!(list.contains("Available personalities:"));
    }

    #[test]
    fn each_personality_has_non_empty_prompt() {
        for p in all() {
            assert!(
                !p.prompt.is_empty(),
                "personality '{}' should have a prompt",
                p.name
            );
        }
    }

    #[test]
    fn each_personality_has_non_empty_description() {
        for p in all() {
            assert!(
                !p.description.is_empty(),
                "personality '{}' should have a description",
                p.name
            );
        }
    }

    #[test]
    fn resolve_builtin_name() {
        let resolved = resolve("coder");
        assert!(
            matches!(resolved, ResolvedPersonality::Builtin(p) if p.name == "coder"),
            "should resolve 'coder' as a builtin"
        );
    }

    #[test]
    fn resolve_file_path() {
        let resolved = resolve("/tmp/my-personality.txt");
        assert!(
            matches!(resolved, ResolvedPersonality::File(ref p) if p.to_str() == Some("/tmp/my-personality.txt")),
            "should resolve path as a file"
        );
    }

    #[test]
    fn resolve_file_path_for_unknown_name() {
        let resolved = resolve("totally-custom");
        assert!(
            matches!(resolved, ResolvedPersonality::File(_)),
            "unknown name should resolve as file path"
        );
    }

    #[test]
    fn coder_profile_is_coder() {
        let p = lookup("coder").unwrap();
        assert_eq!(p.profile, Profile::Coder);
    }

    #[test]
    fn reviewer_profile_is_reviewer() {
        let p = lookup("reviewer").unwrap();
        assert_eq!(p.profile, Profile::Reviewer);
    }

    #[test]
    fn researcher_profile_is_reader() {
        let p = lookup("researcher").unwrap();
        assert_eq!(p.profile, Profile::Reader);
    }

    #[test]
    fn coordinator_profile_is_coordinator() {
        let p = lookup("coordinator").unwrap();
        assert_eq!(p.profile, Profile::Coordinator);
    }

    #[test]
    fn documenter_profile_is_notion() {
        let p = lookup("documenter").unwrap();
        assert_eq!(p.profile, Profile::Notion);
    }

    #[test]
    fn researcher_defaults_to_sonnet() {
        let p = lookup("researcher").unwrap();
        assert_eq!(p.default_model, "sonnet");
    }

    #[test]
    fn coder_defaults_to_opus() {
        let p = lookup("coder").unwrap();
        assert_eq!(p.default_model, "opus");
    }

    #[test]
    fn resolve_from_str_builtin() {
        let resolved: ResolvedPersonality = "reviewer".parse().unwrap();
        assert!(matches!(resolved, ResolvedPersonality::Builtin(p) if p.name == "reviewer"));
    }

    #[test]
    fn resolve_from_str_file() {
        let resolved: ResolvedPersonality = "/some/path.txt".parse().unwrap();
        assert!(matches!(resolved, ResolvedPersonality::File(_)));
    }
}
