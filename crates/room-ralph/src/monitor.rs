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

    #[test]
    fn parse_usage_from_usage_path() {
        let json = r#"{"usage":{"input_tokens":150000,"output_tokens":5000}}"#;
        assert_eq!(parse_usage(json), 150000);
        assert_eq!(parse_output_tokens(json), 5000);
    }

    #[test]
    fn parse_usage_from_result_path() {
        let json = r#"{"result":{"usage":{"input_tokens":80000}}}"#;
        assert_eq!(parse_usage(json), 80000);
    }

    #[test]
    fn parse_usage_from_statistics_path() {
        let json = r#"{"statistics":{"input_tokens":42000}}"#;
        assert_eq!(parse_usage(json), 42000);
    }

    #[test]
    fn parse_usage_empty_and_missing() {
        assert_eq!(parse_usage(""), 0);
        assert_eq!(parse_usage("not json"), 0);
        assert_eq!(parse_usage(r#"{"other":"field"}"#), 0);
    }

    #[test]
    fn parse_cost_paths() {
        assert!((parse_cost(r#"{"usage":{"total_cost":0.05}}"#) - 0.05).abs() < f64::EPSILON);
        assert!((parse_cost(r#"{"cost_usd":1.23}"#) - 1.23).abs() < f64::EPSILON);
        assert!((parse_cost(r#"{"total_cost":0.99}"#) - 0.99).abs() < f64::EPSILON);
        assert!((parse_cost("") - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn should_restart_thresholds() {
        // Default: 200000 * 80% = 160000
        assert!(!should_restart(100000));
        assert!(!should_restart(159999));
        assert!(should_restart(160000));
        assert!(should_restart(200000));
    }

    #[test]
    fn usage_pct_calculation() {
        assert_eq!(usage_pct(100000), 50);
        assert_eq!(usage_pct(200000), 100);
        assert_eq!(usage_pct(0), 0);
    }

    #[test]
    fn format_usage_summary_below_threshold() {
        let s = format_usage_summary(100000, 5000);
        assert!(s.contains("100000/200000"));
        assert!(s.contains("50%"));
        assert!(!s.contains("[RESTART]"));
    }

    #[test]
    fn format_usage_summary_above_threshold() {
        let s = format_usage_summary(180000, 5000);
        assert!(s.contains("[RESTART]"));
    }

    #[test]
    fn context_limit_default() {
        // Unset env var to test default
        std::env::remove_var("CONTEXT_LIMIT");
        assert_eq!(context_limit(), 200000);
    }

    #[test]
    fn threshold_tokens_default() {
        std::env::remove_var("CONTEXT_LIMIT");
        std::env::remove_var("CONTEXT_THRESHOLD");
        assert_eq!(threshold_tokens(), 160000);
    }
}
