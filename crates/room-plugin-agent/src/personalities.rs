use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// A named agent personality preset.
///
/// Personalities define everything needed to spawn an agent: model, tool
/// restrictions, prompt, and naming conventions. They are resolved from
/// user-defined TOML files (`~/.room/personalities/<name>.toml`) first,
/// then built-in defaults compiled into the binary.
#[derive(Debug, Clone, Deserialize)]
pub struct Personality {
    pub personality: PersonalityCore,
    #[serde(default)]
    pub tools: ToolConfig,
    #[serde(default)]
    pub prompt: PromptConfig,
    #[serde(default)]
    pub naming: NamingConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PersonalityCore {
    pub name: String,
    pub description: String,
    #[serde(default = "default_model")]
    pub model: String,
}

fn default_model() -> String {
    "sonnet".to_owned()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ToolConfig {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub disallow: Vec<String>,
    #[serde(default)]
    pub allow_all: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PromptConfig {
    #[serde(default)]
    pub template: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NamingConfig {
    #[serde(default)]
    pub name_pool: Vec<String>,
}

impl Personality {
    /// Generate a username for this personality.
    ///
    /// If a name pool is configured and `used_names` doesn't exhaust it,
    /// picks an unused name from the pool. Otherwise falls back to
    /// `<personality>-<short-uuid>`.
    pub fn generate_username(&self, used_names: &[String]) -> String {
        let prefix = &self.personality.name;

        // Try name pool first
        for name in &self.naming.name_pool {
            let candidate = format!("{prefix}-{name}");
            if !used_names.iter().any(|u| u == &candidate) {
                return candidate;
            }
        }

        // Fallback: short UUID
        let short = &uuid::Uuid::new_v4().to_string()[..8];
        format!("{prefix}-{short}")
    }
}

// ── Built-in personalities ──────────────────────────────────────────────────

fn builtin_coder() -> Personality {
    Personality {
        personality: PersonalityCore {
            name: "coder".to_owned(),
            description: "Development agent — reads, writes, tests, commits".to_owned(),
            model: "opus".to_owned(),
        },
        tools: ToolConfig::default(),
        prompt: PromptConfig {
            template: "You are a development agent. Your workflow:\n\
                1. Poll the taskboard for available tasks\n\
                2. Claim a task and announce your plan\n\
                3. Implement, test, and open a PR\n\
                4. Announce completion and return to idle"
                .to_owned(),
        },
        naming: NamingConfig {
            name_pool: vec![
                "anna".to_owned(),
                "kai".to_owned(),
                "nova".to_owned(),
                "zara".to_owned(),
                "leo".to_owned(),
                "mika".to_owned(),
            ],
        },
    }
}

fn builtin_reviewer() -> Personality {
    Personality {
        personality: PersonalityCore {
            name: "reviewer".to_owned(),
            description: "PR reviewer — read-only code access, gh commands".to_owned(),
            model: "sonnet".to_owned(),
        },
        tools: ToolConfig {
            disallow: vec!["Write".to_owned(), "Edit".to_owned()],
            ..Default::default()
        },
        prompt: PromptConfig {
            template: "You are a code reviewer. Focus on correctness, test coverage, \
                and adherence to the project's coding standards. Use `gh pr` commands \
                to leave reviews."
                .to_owned(),
        },
        naming: NamingConfig {
            name_pool: vec!["alice".to_owned(), "bob".to_owned(), "charlie".to_owned()],
        },
    }
}

fn builtin_scout() -> Personality {
    Personality {
        personality: PersonalityCore {
            name: "scout".to_owned(),
            description: "Codebase explorer — search and summarize only".to_owned(),
            model: "haiku".to_owned(),
        },
        tools: ToolConfig {
            disallow: vec!["Write".to_owned(), "Edit".to_owned(), "Bash".to_owned()],
            ..Default::default()
        },
        prompt: PromptConfig {
            template: "You are a codebase explorer. Search and summarize code, \
                answer questions about architecture and patterns. Do not modify files."
                .to_owned(),
        },
        naming: NamingConfig {
            name_pool: vec!["hawk".to_owned(), "owl".to_owned(), "fox".to_owned()],
        },
    }
}

fn builtin_qa() -> Personality {
    Personality {
        personality: PersonalityCore {
            name: "qa".to_owned(),
            description: "Test writer — finds coverage gaps, writes tests".to_owned(),
            model: "sonnet".to_owned(),
        },
        tools: ToolConfig::default(),
        prompt: PromptConfig {
            template: "You are a QA agent. Your workflow:\n\
                1. Identify test coverage gaps\n\
                2. Write unit and integration tests\n\
                3. Ensure all tests pass\n\
                4. Open a PR with the new tests"
                .to_owned(),
        },
        naming: NamingConfig {
            name_pool: vec!["tara".to_owned(), "reo".to_owned(), "juno".to_owned()],
        },
    }
}

fn builtin_coordinator() -> Personality {
    Personality {
        personality: PersonalityCore {
            name: "coordinator".to_owned(),
            description: "BA/triage — reads code, manages issues, coordinates".to_owned(),
            model: "sonnet".to_owned(),
        },
        tools: ToolConfig {
            disallow: vec!["Write".to_owned(), "Edit".to_owned()],
            ..Default::default()
        },
        prompt: PromptConfig {
            template: "You are a coordinator agent. Triage issues, manage the taskboard, \
                review plans, and coordinate work across agents. Do not modify code directly."
                .to_owned(),
        },
        naming: NamingConfig {
            name_pool: vec!["sage".to_owned(), "atlas".to_owned()],
        },
    }
}

/// Returns the built-in personality defaults compiled into the binary.
pub fn builtin_personalities() -> HashMap<String, Personality> {
    let mut map = HashMap::new();
    for p in [
        builtin_coder(),
        builtin_reviewer(),
        builtin_scout(),
        builtin_qa(),
        builtin_coordinator(),
    ] {
        map.insert(p.personality.name.clone(), p);
    }
    map
}

/// Returns the list of all known personality names (built-in + user-defined).
pub fn all_personality_names() -> Vec<String> {
    let mut names: Vec<String> = builtin_personalities().keys().cloned().collect();

    // Add user-defined personalities from ~/.room/personalities/
    if let Some(dir) = personalities_dir() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "toml") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        if !names.contains(&stem.to_owned()) {
                            names.push(stem.to_owned());
                        }
                    }
                }
            }
        }
    }

    names.sort();
    names
}

/// Resolve a personality by name.
///
/// Resolution order:
/// 1. User-defined TOML at `~/.room/personalities/<name>.toml`
/// 2. Built-in defaults compiled into the binary
///
/// User-defined TOML files fully replace built-ins with the same name.
pub fn resolve_personality(name: &str) -> Option<Personality> {
    // 1. Try user-defined TOML
    if let Some(dir) = personalities_dir() {
        let toml_path = dir.join(format!("{name}.toml"));
        if let Some(p) = load_personality_toml(&toml_path) {
            return Some(p);
        }
    }

    // 2. Try built-in
    builtin_personalities().remove(name)
}

/// Load a personality from a TOML file, returning None on any error.
fn load_personality_toml(path: &Path) -> Option<Personality> {
    let content = std::fs::read_to_string(path).ok()?;
    toml::from_str(&content).ok()
}

/// Returns the personality directory path (`~/.room/personalities/`).
fn personalities_dir() -> Option<PathBuf> {
    dirs_path().map(|d| d.join("personalities"))
}

/// Returns `~/.room` base path.
fn dirs_path() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".room"))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_personalities_has_all_five() {
        let builtins = builtin_personalities();
        assert!(builtins.contains_key("coder"));
        assert!(builtins.contains_key("reviewer"));
        assert!(builtins.contains_key("scout"));
        assert!(builtins.contains_key("qa"));
        assert!(builtins.contains_key("coordinator"));
        assert_eq!(builtins.len(), 5);
    }

    #[test]
    fn builtin_coder_has_opus_model() {
        let builtins = builtin_personalities();
        let coder = &builtins["coder"];
        assert_eq!(coder.personality.model, "opus");
    }

    #[test]
    fn builtin_reviewer_disallows_write_edit() {
        let builtins = builtin_personalities();
        let reviewer = &builtins["reviewer"];
        assert!(reviewer.tools.disallow.contains(&"Write".to_owned()));
        assert!(reviewer.tools.disallow.contains(&"Edit".to_owned()));
    }

    #[test]
    fn builtin_scout_disallows_write_edit_bash() {
        let builtins = builtin_personalities();
        let scout = &builtins["scout"];
        assert!(scout.tools.disallow.contains(&"Write".to_owned()));
        assert!(scout.tools.disallow.contains(&"Edit".to_owned()));
        assert!(scout.tools.disallow.contains(&"Bash".to_owned()));
    }

    #[test]
    fn generate_username_from_name_pool() {
        let p = builtin_coder();
        let username = p.generate_username(&[]);
        assert!(username.starts_with("coder-"));
        // Should be a name from the pool, not a UUID
        assert!(
            p.naming
                .name_pool
                .iter()
                .any(|n| username == format!("coder-{n}")),
            "expected name from pool, got: {username}"
        );
    }

    #[test]
    fn generate_username_skips_used_names() {
        let p = builtin_coder();
        // Mark first name as used
        let first_name = format!("coder-{}", p.naming.name_pool[0]);
        let username = p.generate_username(&[first_name.clone()]);
        assert_ne!(username, first_name);
        assert!(username.starts_with("coder-"));
    }

    #[test]
    fn generate_username_fallback_to_uuid_when_pool_exhausted() {
        let p = builtin_reviewer();
        // Exhaust the pool
        let used: Vec<String> = p
            .naming
            .name_pool
            .iter()
            .map(|n| format!("reviewer-{n}"))
            .collect();
        let username = p.generate_username(&used);
        assert!(username.starts_with("reviewer-"));
        // Should be 8-char hex UUID suffix
        let suffix = username.strip_prefix("reviewer-").unwrap();
        assert_eq!(suffix.len(), 8);
    }

    #[test]
    fn generate_username_empty_pool_uses_uuid() {
        let mut p = builtin_coder();
        p.naming.name_pool.clear();
        let username = p.generate_username(&[]);
        assert!(username.starts_with("coder-"));
        let suffix = username.strip_prefix("coder-").unwrap();
        assert_eq!(suffix.len(), 8);
    }

    #[test]
    fn toml_deserialization_roundtrip() {
        let toml_str = r#"
[personality]
name = "custom"
description = "A custom personality"
model = "opus"

[tools]
disallow = ["Bash"]

[prompt]
template = "You are a custom agent."

[naming]
name_pool = ["alpha", "beta"]
"#;
        let p: Personality = toml::from_str(toml_str).unwrap();
        assert_eq!(p.personality.name, "custom");
        assert_eq!(p.personality.description, "A custom personality");
        assert_eq!(p.personality.model, "opus");
        assert_eq!(p.tools.disallow, vec!["Bash"]);
        assert!(p.tools.allow.is_empty());
        assert!(!p.tools.allow_all);
        assert_eq!(p.prompt.template, "You are a custom agent.");
        assert_eq!(p.naming.name_pool, vec!["alpha", "beta"]);
    }

    #[test]
    fn toml_deserialization_minimal() {
        let toml_str = r#"
[personality]
name = "minimal"
description = "Minimal personality"
"#;
        let p: Personality = toml::from_str(toml_str).unwrap();
        assert_eq!(p.personality.name, "minimal");
        assert_eq!(p.personality.model, "sonnet"); // default
        assert!(p.tools.disallow.is_empty());
        assert!(p.tools.allow.is_empty());
        assert!(p.prompt.template.is_empty());
        assert!(p.naming.name_pool.is_empty());
    }

    #[test]
    fn toml_deserialization_allow_all() {
        let toml_str = r#"
[personality]
name = "unrestricted"
description = "No tool restrictions"

[tools]
allow_all = true
"#;
        let p: Personality = toml::from_str(toml_str).unwrap();
        assert!(p.tools.allow_all);
    }

    #[test]
    fn resolve_personality_returns_builtin() {
        let p = resolve_personality("coder").unwrap();
        assert_eq!(p.personality.name, "coder");
        assert_eq!(p.personality.model, "opus");
    }

    #[test]
    fn resolve_personality_returns_none_for_unknown() {
        assert!(resolve_personality("nonexistent-personality-xyz").is_none());
    }

    #[test]
    fn resolve_personality_user_toml_overrides_builtin() {
        let dir = tempfile::tempdir().unwrap();
        let personalities_dir = dir.path().join("personalities");
        std::fs::create_dir_all(&personalities_dir).unwrap();

        let toml_content = r#"
[personality]
name = "coder"
description = "Custom coder override"
model = "haiku"
"#;
        std::fs::write(personalities_dir.join("coder.toml"), toml_content).unwrap();

        // Test loading directly from the TOML file
        let p = load_personality_toml(&personalities_dir.join("coder.toml")).unwrap();
        assert_eq!(p.personality.name, "coder");
        assert_eq!(p.personality.model, "haiku");
        assert_eq!(p.personality.description, "Custom coder override");
    }

    #[test]
    fn all_personality_names_includes_builtins() {
        let names = all_personality_names();
        assert!(names.contains(&"coder".to_owned()));
        assert!(names.contains(&"reviewer".to_owned()));
        assert!(names.contains(&"scout".to_owned()));
        assert!(names.contains(&"qa".to_owned()));
        assert!(names.contains(&"coordinator".to_owned()));
    }

    #[test]
    fn all_personality_names_sorted() {
        let names = all_personality_names();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }
}
