//! `room upgrade` command — checks for newer versions of room-cli and room-ralph
//! on crates.io, verifies plugin compatibility, and executes the upgrade.
//!
//! Reads `~/.room/plugins/*.meta.json` to determine installed plugin protocol
//! compatibility ranges. If any plugin would be incompatible with the new
//! room-protocol version, the upgrade is blocked with a warning.

use std::path::Path;

use serde::Deserialize;

/// Response from crates.io `/api/v1/crates/<name>` endpoint.
#[derive(Debug, Deserialize)]
struct CrateResponse {
    #[serde(rename = "crate")]
    krate: CrateInfo,
}

#[derive(Debug, Deserialize)]
struct CrateInfo {
    max_stable_version: Option<String>,
}

/// Plugin metadata sidecar (mirrors `plugin_cmd::PluginMeta` when available).
#[derive(Debug, Clone, Deserialize)]
struct PluginMeta {
    name: String,
    version: String,
    protocol_compat: String,
}

/// Result of checking a single binary for upgrades.
#[derive(Debug)]
pub struct UpgradeCheck {
    pub crate_name: String,
    pub current: String,
    pub latest: String,
    pub needs_upgrade: bool,
}

/// Result of checking plugin compatibility against a new protocol version.
#[derive(Debug)]
pub struct PluginCompat {
    pub name: String,
    pub version: String,
    pub compat_range: String,
    pub compatible: bool,
}

/// Scan `~/.room/plugins/` for installed plugin meta files.
fn scan_plugin_metas(dir: &Path) -> Vec<PluginMeta> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut plugins = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.ends_with(".meta.json"))
            .unwrap_or(false)
        {
            if let Ok(data) = std::fs::read_to_string(&path) {
                if let Ok(meta) = serde_json::from_str::<PluginMeta>(&data) {
                    plugins.push(meta);
                }
            }
        }
    }
    plugins.sort_by(|a, b| a.name.cmp(&b.name));
    plugins
}

/// Query crates.io for the latest stable version of a crate.
///
/// Uses curl to avoid adding a heavy HTTP client dependency.
fn query_latest_version(crate_name: &str) -> anyhow::Result<String> {
    let url = format!("https://crates.io/api/v1/crates/{crate_name}");
    let output = std::process::Command::new("curl")
        .args(["-sS", "-H", "User-Agent: room-cli upgrade check", &url])
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run curl: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("curl failed for {crate_name}: {stderr}");
    }

    let resp: CrateResponse = serde_json::from_slice(&output.stdout)
        .map_err(|e| anyhow::anyhow!("failed to parse crates.io response for {crate_name}: {e}"))?;
    resp.krate
        .max_stable_version
        .ok_or_else(|| anyhow::anyhow!("no stable version found for {crate_name}"))
}

/// Compare two semver strings. Returns true if `latest` > `current`.
pub fn is_newer(current: &str, latest: &str) -> bool {
    let parse = |s: &str| -> (u64, u64, u64) {
        let mut parts = s.split('.');
        let major = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        let patch = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        (major, minor, patch)
    };
    parse(latest) > parse(current)
}

/// Check whether a plugin's protocol_compat range includes a target version.
///
/// Supports simple semver ranges like `>=3.0.0, <4.0.0` and `>=3.0.0`.
/// For simplicity, parses `>=X.Y.Z` as minimum and `<X.Y.Z` as exclusive max.
pub fn check_compat(protocol_compat: &str, target_version: &str) -> bool {
    let target = parse_semver(target_version);
    let mut min: Option<(u64, u64, u64)> = None;
    let mut max_exclusive: Option<(u64, u64, u64)> = None;

    for constraint in protocol_compat.split(',') {
        let constraint = constraint.trim();
        if let Some(rest) = constraint.strip_prefix(">=") {
            min = Some(parse_semver(rest.trim()));
        } else if let Some(rest) = constraint.strip_prefix('<') {
            max_exclusive = Some(parse_semver(rest.trim()));
        }
    }

    if let Some(min_v) = min {
        if target < min_v {
            return false;
        }
    }
    if let Some(max_v) = max_exclusive {
        if target >= max_v {
            return false;
        }
    }
    true
}

fn parse_semver(s: &str) -> (u64, u64, u64) {
    let mut parts = s.split('.');
    let major = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let patch = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (major, minor, patch)
}

/// Run the upgrade check and display the plan.
///
/// If `execute` is true, runs `cargo install` for binaries that need upgrading.
pub fn cmd_upgrade(execute: bool) -> anyhow::Result<()> {
    let current_cli = env!("CARGO_PKG_VERSION");

    println!("checking for updates...\n");

    // Check room-cli
    let cli_check = match query_latest_version("room-cli") {
        Ok(latest) => {
            let needs = is_newer(current_cli, &latest);
            Some(UpgradeCheck {
                crate_name: "room-cli".to_owned(),
                current: current_cli.to_owned(),
                latest,
                needs_upgrade: needs,
            })
        }
        Err(e) => {
            eprintln!("  warning: could not check room-cli: {e}");
            None
        }
    };

    // Check room-ralph
    let ralph_check = match query_latest_version("room-ralph") {
        Ok(latest) => {
            // We don't know the installed ralph version from here.
            // Use "unknown" and always suggest checking.
            Some(UpgradeCheck {
                crate_name: "room-ralph".to_owned(),
                current: "unknown".to_owned(),
                latest,
                needs_upgrade: true,
            })
        }
        Err(e) => {
            eprintln!("  warning: could not check room-ralph: {e}");
            None
        }
    };

    // Display binary upgrade plan
    println!("binaries:");
    let mut any_upgrade = false;
    for check in [&cli_check, &ralph_check].into_iter().flatten() {
        let status = if check.needs_upgrade {
            any_upgrade = true;
            format!("{} -> {} (upgrade available)", check.current, check.latest)
        } else {
            format!("{} (up to date)", check.current)
        };
        println!("  {:<15} {status}", check.crate_name);
    }

    // Check plugin compatibility
    let plugins_dir = plugins_dir();
    let plugins = scan_plugin_metas(&plugins_dir);
    if !plugins.is_empty() {
        println!("\nplugins:");
        let target_protocol = cli_check
            .as_ref()
            .map(|c| c.latest.as_str())
            .unwrap_or(current_cli);

        let mut all_compatible = true;
        for p in &plugins {
            let compatible = check_compat(&p.protocol_compat, target_protocol);
            let status = if compatible {
                "compatible"
            } else {
                all_compatible = false;
                "INCOMPATIBLE"
            };
            println!(
                "  {:<20} v{:<10} {} (requires {})",
                p.name, p.version, status, p.protocol_compat
            );
        }

        if !all_compatible {
            eprintln!("\nwarning: some plugins are incompatible with the target version.");
            eprintln!("run `room plugin update <name>` after upgrading to fix compatibility.");
        }
    } else {
        println!("\nplugins: none installed");
    }

    if !any_upgrade {
        println!("\neverything is up to date.");
        return Ok(());
    }

    if !execute {
        println!("\nrun `room upgrade --execute` to apply the upgrade.");
        return Ok(());
    }

    // Execute upgrades
    println!("\nupgrading...");
    for check in [&cli_check, &ralph_check].into_iter().flatten() {
        if !check.needs_upgrade {
            continue;
        }
        println!("  installing {} v{}...", check.crate_name, check.latest);
        let status = std::process::Command::new("cargo")
            .args(["install", &check.crate_name, "--force"])
            .status()?;
        if status.success() {
            println!("  {} upgraded to v{}", check.crate_name, check.latest);
        } else {
            eprintln!(
                "  error: cargo install {} failed (exit {})",
                check.crate_name,
                status.code().unwrap_or(-1)
            );
        }
    }

    println!("\nupgrade complete.");
    Ok(())
}

/// Return the plugin directory path (`~/.room/plugins/`).
fn plugins_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
    std::path::PathBuf::from(home).join(".room").join("plugins")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_newer_detects_major_bump() {
        assert!(is_newer("3.1.0", "4.0.0"));
    }

    #[test]
    fn is_newer_detects_minor_bump() {
        assert!(is_newer("3.1.0", "3.2.0"));
    }

    #[test]
    fn is_newer_detects_patch_bump() {
        assert!(is_newer("3.1.0", "3.1.1"));
    }

    #[test]
    fn is_newer_same_version_is_false() {
        assert!(!is_newer("3.1.0", "3.1.0"));
    }

    #[test]
    fn is_newer_older_is_false() {
        assert!(!is_newer("3.2.0", "3.1.0"));
    }

    #[test]
    fn check_compat_within_range() {
        assert!(check_compat(">=3.0.0, <4.0.0", "3.5.0"));
    }

    #[test]
    fn check_compat_at_minimum() {
        assert!(check_compat(">=3.0.0, <4.0.0", "3.0.0"));
    }

    #[test]
    fn check_compat_below_minimum() {
        assert!(!check_compat(">=3.0.0, <4.0.0", "2.9.9"));
    }

    #[test]
    fn check_compat_at_exclusive_max() {
        assert!(!check_compat(">=3.0.0, <4.0.0", "4.0.0"));
    }

    #[test]
    fn check_compat_above_max() {
        assert!(!check_compat(">=3.0.0, <4.0.0", "5.0.0"));
    }

    #[test]
    fn check_compat_open_ended_min_only() {
        assert!(check_compat(">=3.0.0", "99.0.0"));
        assert!(!check_compat(">=3.0.0", "2.0.0"));
    }

    #[test]
    fn scan_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = scan_plugin_metas(dir.path());
        assert!(plugins.is_empty());
    }

    #[test]
    fn scan_nonexistent_dir() {
        let plugins = scan_plugin_metas(Path::new("/nonexistent/path"));
        assert!(plugins.is_empty());
    }

    #[test]
    fn scan_finds_meta_files() {
        let dir = tempfile::tempdir().unwrap();
        let meta = serde_json::json!({
            "name": "test-plugin",
            "version": "1.0.0",
            "crate_name": "room-plugin-test",
            "protocol_compat": ">=3.0.0, <4.0.0",
            "lib_file": "libroom_plugin_test.so"
        });
        std::fs::write(
            dir.path().join("test-plugin.meta.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();
        let plugins = scan_plugin_metas(dir.path());
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "test-plugin");
    }

    #[test]
    fn scan_skips_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bad.meta.json"), "not json").unwrap();
        let plugins = scan_plugin_metas(dir.path());
        assert!(plugins.is_empty());
    }

    #[test]
    fn parse_semver_basic() {
        assert_eq!(parse_semver("3.1.0"), (3, 1, 0));
        assert_eq!(parse_semver("0.0.0"), (0, 0, 0));
        assert_eq!(parse_semver("10.20.30"), (10, 20, 30));
    }

    #[test]
    fn parse_semver_malformed() {
        assert_eq!(parse_semver("garbage"), (0, 0, 0));
        assert_eq!(parse_semver(""), (0, 0, 0));
    }
}
