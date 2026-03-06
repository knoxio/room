//! Context monitoring — tracks token usage and decides when to restart.
//!
//! Port of scripts/context-monitor.sh to Rust.
//! Owner: bumblebee (bb) — tests and implementation.

use std::path::Path;

/// Default model context window size (tokens).
pub const DEFAULT_CONTEXT_LIMIT: u64 = 200_000;

/// Default restart threshold as percentage of context limit.
pub const DEFAULT_THRESHOLD_PCT: u64 = 80;

/// Extract input_tokens from claude `--output-format json` output.
///
/// Tries multiple JSON paths: `.usage.input_tokens`, `.result.usage.input_tokens`,
/// `.statistics.input_tokens`. Returns 0 if not found.
pub fn parse_usage(json: &str) -> u64 {
    parse_token_field(json, "input_tokens")
}

/// Extract output_tokens from claude JSON output.
pub fn parse_output_tokens(json: &str) -> u64 {
    parse_token_field(json, "output_tokens")
}

/// Extract total cost (USD) from claude JSON output. Returns 0.0 if not found.
pub fn parse_cost(json: &str) -> f64 {
    if json.is_empty() {
        return 0.0;
    }
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return 0.0,
    };

    for path in &[
        "usage.total_cost",
        "result.usage.total_cost",
        "cost_usd",
        "total_cost",
    ] {
        if let Some(cost) = resolve_path(&v, path).and_then(|v| v.as_f64()) {
            return cost;
        }
    }
    0.0
}

/// Return the effective context window size.
pub fn context_limit() -> u64 {
    std::env::var("CONTEXT_LIMIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_CONTEXT_LIMIT)
}

/// Return the token count at which a restart should be triggered.
pub fn threshold_tokens() -> u64 {
    let limit = context_limit();
    let pct = std::env::var("CONTEXT_THRESHOLD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_THRESHOLD_PCT);
    limit * pct / 100
}

/// Returns true if input_tokens >= threshold.
pub fn should_restart(input_tokens: u64) -> bool {
    input_tokens >= threshold_tokens()
}

/// Return the percentage of context window used (integer).
pub fn usage_pct(input_tokens: u64) -> u64 {
    let limit = context_limit();
    if limit == 0 {
        return 0;
    }
    input_tokens * 100 / limit
}

/// Format a human-readable one-line usage summary.
pub fn format_usage_summary(input_tokens: u64, output_tokens: u64) -> String {
    let pct = usage_pct(input_tokens);
    let limit = context_limit();
    let threshold = threshold_tokens();
    let restart_tag = if should_restart(input_tokens) {
        " [RESTART]"
    } else {
        ""
    };
    format!(
        "context: {input_tokens}/{limit} ({pct}%) threshold: {threshold} output: {output_tokens}{restart_tag}"
    )
}

/// Append a usage entry to the progress file's Context Usage section.
pub fn log_usage(
    progress_file: &Path,
    input_tokens: u64,
    output_tokens: u64,
    iteration: u32,
) -> std::io::Result<()> {
    use std::io::Write;

    let pct = usage_pct(input_tokens);
    let threshold = threshold_tokens();
    let limit = context_limit();
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let restart_note = if should_restart(input_tokens) {
        " **RESTART TRIGGERED**"
    } else {
        ""
    };

    let entry = format!(
        "- {ts}: iter={iteration} input={input_tokens}/{limit} ({pct}%) output={output_tokens} threshold={threshold}{restart_note}\n"
    );

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(progress_file)?;

    // Check if Context Usage section exists
    if let Ok(content) = std::fs::read_to_string(progress_file) {
        if !content.contains("## Context Usage") {
            file.write_all(b"\n## Context Usage\n")?;
        }
    } else {
        file.write_all(b"\n## Context Usage\n")?;
    }

    file.write_all(entry.as_bytes())?;
    Ok(())
}

// --- Internal helpers ---

fn parse_token_field(json: &str, field: &str) -> u64 {
    if json.is_empty() {
        return 0;
    }
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return 0,
    };

    let paths = [
        format!("usage.{field}"),
        format!("result.usage.{field}"),
        format!("statistics.{field}"),
    ];

    for path in &paths {
        if let Some(tokens) = resolve_path(&v, path).and_then(|v| v.as_u64()) {
            return tokens;
        }
    }
    0
}

/// Resolve a dot-separated path in a serde_json::Value.
fn resolve_path<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Mock JSON payloads (mirroring shell test fixtures) ──────────

    const JSON_TOP: &str =
        r#"{"result":"hello","usage":{"input_tokens":150000,"output_tokens":2000}}"#;

    const JSON_NESTED: &str =
        r#"{"result":{"content":"hello","usage":{"input_tokens":95000,"output_tokens":1500}}}"#;

    const JSON_STATS: &str =
        r#"{"result":"hello","statistics":{"input_tokens":180000,"output_tokens":3000}}"#;

    const JSON_NONE: &str = r#"{"result":"hello"}"#;

    const JSON_COST: &str = r#"{"result":"hello","usage":{"input_tokens":100000,"output_tokens":2000,"total_cost":0.42}}"#;

    const JSON_COST_USD: &str = r#"{"result":"hello","cost_usd":1.23}"#;

    const JSON_TOTAL_COST: &str = r#"{"result":"hello","total_cost":0.99}"#;

    // ── parse_usage ─────────────────────────────────────────────────

    #[test]
    fn parse_usage_top_level() {
        assert_eq!(parse_usage(JSON_TOP), 150_000);
    }

    #[test]
    fn parse_usage_nested_under_result() {
        assert_eq!(parse_usage(JSON_NESTED), 95_000);
    }

    #[test]
    fn parse_usage_statistics_path() {
        assert_eq!(parse_usage(JSON_STATS), 180_000);
    }

    #[test]
    fn parse_usage_missing_returns_zero() {
        assert_eq!(parse_usage(JSON_NONE), 0);
    }

    #[test]
    fn parse_usage_empty_string_returns_zero() {
        assert_eq!(parse_usage(""), 0);
    }

    #[test]
    fn parse_usage_invalid_json_returns_zero() {
        assert_eq!(parse_usage("not json at all"), 0);
    }

    #[test]
    fn parse_usage_null_tokens_returns_zero() {
        let json = r#"{"usage":{"input_tokens":null}}"#;
        assert_eq!(parse_usage(json), 0);
    }

    // ── parse_output_tokens ─────────────────────────────────────────

    #[test]
    fn parse_output_tokens_top_level() {
        assert_eq!(parse_output_tokens(JSON_TOP), 2_000);
    }

    #[test]
    fn parse_output_tokens_nested() {
        assert_eq!(parse_output_tokens(JSON_NESTED), 1_500);
    }

    #[test]
    fn parse_output_tokens_statistics() {
        assert_eq!(parse_output_tokens(JSON_STATS), 3_000);
    }

    #[test]
    fn parse_output_tokens_missing_returns_zero() {
        assert_eq!(parse_output_tokens(JSON_NONE), 0);
    }

    #[test]
    fn parse_output_tokens_empty_returns_zero() {
        assert_eq!(parse_output_tokens(""), 0);
    }

    // ── parse_cost ──────────────────────────────────────────────────

    #[test]
    fn parse_cost_usage_total_cost() {
        let cost = parse_cost(JSON_COST);
        assert!((cost - 0.42).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_cost_cost_usd_path() {
        let cost = parse_cost(JSON_COST_USD);
        assert!((cost - 1.23).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_cost_total_cost_path() {
        let cost = parse_cost(JSON_TOTAL_COST);
        assert!((cost - 0.99).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_cost_no_cost_returns_zero() {
        assert!((parse_cost(JSON_TOP) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_cost_empty_returns_zero() {
        assert!((parse_cost("") - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_cost_result_nested() {
        let json = r#"{"result":{"usage":{"total_cost":2.50}}}"#;
        assert!((parse_cost(json) - 2.50).abs() < f64::EPSILON);
    }

    // ── context_limit / threshold_tokens ────────────────────────────

    #[test]
    fn context_limit_default() {
        std::env::remove_var("CONTEXT_LIMIT");
        assert_eq!(context_limit(), DEFAULT_CONTEXT_LIMIT);
    }

    #[test]
    fn threshold_tokens_default() {
        std::env::remove_var("CONTEXT_LIMIT");
        std::env::remove_var("CONTEXT_THRESHOLD");
        assert_eq!(threshold_tokens(), 160_000);
    }

    // ── should_restart ──────────────────────────────────────────────

    #[test]
    fn should_restart_under_threshold() {
        std::env::remove_var("CONTEXT_LIMIT");
        std::env::remove_var("CONTEXT_THRESHOLD");
        assert!(!should_restart(100_000));
    }

    #[test]
    fn should_restart_just_below_threshold() {
        std::env::remove_var("CONTEXT_LIMIT");
        std::env::remove_var("CONTEXT_THRESHOLD");
        assert!(!should_restart(159_999));
    }

    #[test]
    fn should_restart_at_threshold() {
        std::env::remove_var("CONTEXT_LIMIT");
        std::env::remove_var("CONTEXT_THRESHOLD");
        assert!(should_restart(160_000));
    }

    #[test]
    fn should_restart_over_threshold() {
        std::env::remove_var("CONTEXT_LIMIT");
        std::env::remove_var("CONTEXT_THRESHOLD");
        assert!(should_restart(180_000));
    }

    #[test]
    fn should_restart_at_limit() {
        std::env::remove_var("CONTEXT_LIMIT");
        std::env::remove_var("CONTEXT_THRESHOLD");
        assert!(should_restart(200_000));
    }

    #[test]
    fn should_restart_zero_tokens() {
        std::env::remove_var("CONTEXT_LIMIT");
        std::env::remove_var("CONTEXT_THRESHOLD");
        assert!(!should_restart(0));
    }

    // ── usage_pct ───────────────────────────────────────────────────

    #[test]
    fn usage_pct_fifty() {
        std::env::remove_var("CONTEXT_LIMIT");
        assert_eq!(usage_pct(100_000), 50);
    }

    #[test]
    fn usage_pct_seventy_five() {
        std::env::remove_var("CONTEXT_LIMIT");
        assert_eq!(usage_pct(150_000), 75);
    }

    #[test]
    fn usage_pct_hundred() {
        std::env::remove_var("CONTEXT_LIMIT");
        assert_eq!(usage_pct(200_000), 100);
    }

    #[test]
    fn usage_pct_zero() {
        std::env::remove_var("CONTEXT_LIMIT");
        assert_eq!(usage_pct(0), 0);
    }

    // ── format_usage_summary ────────────────────────────────────────

    #[test]
    fn format_usage_summary_under_threshold() {
        std::env::remove_var("CONTEXT_LIMIT");
        std::env::remove_var("CONTEXT_THRESHOLD");
        let summary = format_usage_summary(100_000, 2_000);
        assert!(summary.contains("100000/200000"));
        assert!(summary.contains("50%"));
        assert!(summary.contains("threshold: 160000"));
        assert!(!summary.contains("[RESTART]"));
    }

    #[test]
    fn format_usage_summary_over_threshold() {
        std::env::remove_var("CONTEXT_LIMIT");
        std::env::remove_var("CONTEXT_THRESHOLD");
        let summary = format_usage_summary(180_000, 3_000);
        assert!(summary.contains("180000/200000"));
        assert!(summary.contains("90%"));
        assert!(summary.contains("[RESTART]"));
    }

    // ── log_usage ───────────────────────────────────────────────────

    #[test]
    fn log_usage_creates_section_in_new_file() {
        std::env::remove_var("CONTEXT_LIMIT");
        std::env::remove_var("CONTEXT_THRESHOLD");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("progress.md");

        log_usage(&path, 150_000, 2_000, 3).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("## Context Usage"));
        assert!(content.contains("input=150000"));
        assert!(content.contains("output=2000"));
        assert!(content.contains("iter=3"));
    }

    #[test]
    fn log_usage_appends_to_existing_section() {
        std::env::remove_var("CONTEXT_LIMIT");
        std::env::remove_var("CONTEXT_THRESHOLD");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("progress.md");

        log_usage(&path, 150_000, 2_000, 3).unwrap();
        log_usage(&path, 170_000, 2_500, 4).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.matches("## Context Usage").count(), 1);
        assert_eq!(content.matches("input=").count(), 2);
    }

    #[test]
    fn log_usage_restart_annotation() {
        std::env::remove_var("CONTEXT_LIMIT");
        std::env::remove_var("CONTEXT_THRESHOLD");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("progress.md");

        log_usage(&path, 180_000, 3_000, 5).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("RESTART TRIGGERED"));
    }

    #[test]
    fn log_usage_no_restart_under_threshold() {
        std::env::remove_var("CONTEXT_LIMIT");
        std::env::remove_var("CONTEXT_THRESHOLD");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("progress.md");

        log_usage(&path, 100_000, 1_000, 1).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.contains("RESTART TRIGGERED"));
    }

    // ── resolve_path (internal) ─────────────────────────────────────

    #[test]
    fn resolve_path_single_segment() {
        let v: serde_json::Value = serde_json::from_str(r#"{"a":42}"#).unwrap();
        assert_eq!(resolve_path(&v, "a").unwrap().as_u64(), Some(42));
    }

    #[test]
    fn resolve_path_nested() {
        let v: serde_json::Value = serde_json::from_str(r#"{"a":{"b":{"c":99}}}"#).unwrap();
        assert_eq!(resolve_path(&v, "a.b.c").unwrap().as_u64(), Some(99));
    }

    #[test]
    fn resolve_path_missing_returns_none() {
        let v: serde_json::Value = serde_json::from_str(r#"{"a":1}"#).unwrap();
        assert!(resolve_path(&v, "b").is_none());
        assert!(resolve_path(&v, "a.b").is_none());
    }
}
