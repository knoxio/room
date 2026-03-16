//! Plugin management commands: install, list, remove, update.
//!
//! These are client-side operations that manage the `~/.room/plugins/`
//! directory. The daemon loads plugins from this directory on startup
//! using `libloading`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ── Builtin plugin metadata ─────────────────────────────────────────────────

/// Statically compiled plugins shipped with the room binary.
///
/// These are always available regardless of what is installed in
/// `~/.room/plugins/`. Displayed as `[builtin]` in `room plugin list`.
pub const BUILTIN_PLUGINS: &[(&str, &str)] = &[
    ("agent", "Agent spawn/stop/list/logs, /spawn personalities"),
    ("queue", "FIFO message queue (push/pop/peek/list/clear)"),
    ("stats", "Room statistics (message counts, uptime)"),
    (
        "taskboard",
        "Task lifecycle management (post/claim/plan/approve/finish)",
    ),
];

// ── Plugin metadata ─────────────────────────────────────────────────────────

/// Metadata written alongside each installed plugin shared library.
///
/// Stored as `<name>.meta.json` in `~/.room/plugins/`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginMeta {
    /// Short plugin name (e.g. `"agent"`, `"taskboard"`).
    pub name: String,
    /// Crate name on crates.io (e.g. `"room-plugin-agent"`).
    pub crate_name: String,
    /// Installed version (semver).
    pub version: String,
    /// Minimum compatible `room-protocol` version (semver).
    pub min_protocol: String,
    /// Shared library filename (e.g. `"libroom_plugin_agent.so"`).
    pub lib_file: String,
}

// ── Name resolution ─────────────────────────────────────────────────────────

/// Resolve a user-supplied plugin name to the crate name on crates.io.
///
/// If the name already starts with `room-plugin-`, it is returned as-is.
/// Otherwise, `room-plugin-` is prepended.
pub fn resolve_crate_name(name: &str) -> String {
    if name.starts_with("room-plugin-") {
        name.to_owned()
    } else {
        format!("room-plugin-{name}")
    }
}

/// Derive the short plugin name from a crate name.
///
/// Strips the `room-plugin-` prefix if present.
pub fn short_name(crate_name: &str) -> String {
    crate_name
        .strip_prefix("room-plugin-")
        .unwrap_or(crate_name)
        .to_owned()
}

/// Compute the expected shared library filename for a plugin crate.
///
/// Cargo produces `lib<crate_name_underscored>.so` on Linux and
/// `lib<crate_name_underscored>.dylib` on macOS.
pub fn lib_filename(crate_name: &str) -> String {
    let stem = crate_name.replace('-', "_");
    let ext = if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    };
    format!("lib{stem}.{ext}")
}

/// Path to the `.meta.json` file for a plugin.
pub fn meta_path(plugins_dir: &Path, name: &str) -> PathBuf {
    plugins_dir.join(format!("{name}.meta.json"))
}

// ── Scanning installed plugins ──────────────────────────────────────────────

/// Scan the plugins directory and return metadata for all installed plugins.
pub fn scan_installed(plugins_dir: &Path) -> Vec<PluginMeta> {
    let entries = match std::fs::read_dir(plugins_dir) {
        Ok(e) => e,
        Err(_) => return vec![],
    };
    let mut metas = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json")
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".meta.json"))
        {
            if let Ok(data) = std::fs::read_to_string(&path) {
                if let Ok(meta) = serde_json::from_str::<PluginMeta>(&data) {
                    metas.push(meta);
                }
            }
        }
    }
    metas.sort_by(|a, b| a.name.cmp(&b.name));
    metas
}

// ── Commands ────────────────────────────────────────────────────────────────

/// Install a plugin from crates.io.
///
/// 1. Resolves the crate name from the short name.
/// 2. Creates a temporary directory and runs `cargo install` with
///    `--target-dir` to build the cdylib.
/// 3. Copies the shared library to `~/.room/plugins/`.
/// 4. Writes `.meta.json` alongside it.
pub fn cmd_install(plugins_dir: &Path, name: &str, version: Option<&str>) -> anyhow::Result<()> {
    let crate_name = resolve_crate_name(name);
    let short = short_name(&crate_name);

    // Check if already installed.
    let existing_meta = meta_path(plugins_dir, &short);
    if existing_meta.exists() {
        if let Ok(data) = std::fs::read_to_string(&existing_meta) {
            if let Ok(meta) = serde_json::from_str::<PluginMeta>(&data) {
                eprintln!(
                    "plugin '{}' v{} is already installed — use `room plugin update {}` to upgrade",
                    short, meta.version, short
                );
                return Ok(());
            }
        }
    }

    // Ensure plugins directory exists.
    std::fs::create_dir_all(plugins_dir)?;

    // Build in a temp directory.
    let build_dir = tempfile::TempDir::new()?;
    eprintln!("installing {crate_name}...");

    let mut cmd = std::process::Command::new("cargo");
    cmd.args(["install", "--root"])
        .arg(build_dir.path())
        .args(["--target-dir"])
        .arg(build_dir.path().join("target"))
        .arg(&crate_name);

    if let Some(v) = version {
        cmd.args(["--version", v]);
    }

    let output = cmd
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run cargo install: {e} — is cargo on PATH?"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("cargo install failed:\n{stderr}");
    }

    // Find the built shared library.
    let lib_name = lib_filename(&crate_name);
    let built_lib = find_built_lib(build_dir.path(), &lib_name)?;

    // Copy to plugins directory.
    let dest_lib = plugins_dir.join(&lib_name);
    std::fs::copy(&built_lib, &dest_lib).map_err(|e| {
        anyhow::anyhow!(
            "failed to copy {} to {}: {e}",
            built_lib.display(),
            dest_lib.display()
        )
    })?;

    // Extract version from cargo output or default.
    let installed_version = version.unwrap_or("latest").to_owned();

    // Write metadata.
    let meta = PluginMeta {
        name: short.to_owned(),
        crate_name: crate_name.clone(),
        version: installed_version,
        min_protocol: "0.0.0".to_owned(),
        lib_file: lib_name,
    };
    let meta_file = meta_path(plugins_dir, &short);
    std::fs::write(&meta_file, serde_json::to_string_pretty(&meta)?)?;

    eprintln!("installed plugin '{}' to {}", short, dest_lib.display());
    Ok(())
}

/// List all plugins — builtins first, then installed external plugins.
pub fn cmd_list(plugins_dir: &Path) -> anyhow::Result<()> {
    let externals = scan_installed(plugins_dir);

    println!(
        "{:<16} {:<12} {:<10} {}",
        "NAME", "VERSION", "SOURCE", "DESCRIPTION"
    );

    // Builtins — always shown
    let version = env!("CARGO_PKG_VERSION");
    for (name, description) in BUILTIN_PLUGINS {
        println!(
            "{:<16} {:<12} {:<10} {}",
            name, version, "[builtin]", description
        );
    }

    // External plugins from ~/.room/plugins/
    for m in &externals {
        println!(
            "{:<16} {:<12} {:<10} {}",
            m.name, m.version, "[external]", m.crate_name
        );
    }

    Ok(())
}

/// Remove an installed plugin.
pub fn cmd_remove(plugins_dir: &Path, name: &str) -> anyhow::Result<()> {
    let short = short_name(&resolve_crate_name(name));
    let meta_file = meta_path(plugins_dir, &short);

    if !meta_file.exists() {
        anyhow::bail!("plugin '{}' is not installed", short);
    }

    // Read meta to find the lib file.
    let data = std::fs::read_to_string(&meta_file)?;
    let meta: PluginMeta = serde_json::from_str(&data)?;

    // Remove the shared library.
    let lib_path = plugins_dir.join(&meta.lib_file);
    if lib_path.exists() {
        std::fs::remove_file(&lib_path)?;
    }

    // Remove the meta file.
    std::fs::remove_file(&meta_file)?;

    eprintln!("removed plugin '{}'", short);
    Ok(())
}

/// Update an installed plugin to the latest (or specified) version.
pub fn cmd_update(plugins_dir: &Path, name: &str, version: Option<&str>) -> anyhow::Result<()> {
    let short = short_name(&resolve_crate_name(name));
    let meta_file = meta_path(plugins_dir, &short);

    if !meta_file.exists() {
        anyhow::bail!(
            "plugin '{}' is not installed — use `room plugin install {}` first",
            short,
            short
        );
    }

    // Remove old installation, then reinstall.
    cmd_remove(plugins_dir, name)?;
    cmd_install(plugins_dir, name, version)?;
    eprintln!("updated plugin '{}'", short);
    Ok(())
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Search a build directory tree for a shared library matching the expected name.
fn find_built_lib(build_dir: &Path, lib_name: &str) -> anyhow::Result<PathBuf> {
    // cargo install --root puts the library in target/release/deps/ or similar.
    // Walk the tree looking for the file.
    for entry in walkdir(build_dir) {
        if let Some(name) = entry.file_name().and_then(|n| n.to_str()) {
            if name == lib_name {
                return Ok(entry);
            }
        }
    }
    anyhow::bail!(
        "built library '{}' not found in {}",
        lib_name,
        build_dir.display()
    )
}

/// Simple recursive directory walker (avoids adding a `walkdir` dependency).
fn walkdir(dir: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                results.extend(walkdir(&path));
            } else {
                results.push(path);
            }
        }
    }
    results
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── resolve_crate_name ──────────────────────────────────────────────

    #[test]
    fn resolve_short_name_prepends_prefix() {
        assert_eq!(resolve_crate_name("agent"), "room-plugin-agent");
    }

    #[test]
    fn resolve_full_name_unchanged() {
        assert_eq!(
            resolve_crate_name("room-plugin-taskboard"),
            "room-plugin-taskboard"
        );
    }

    #[test]
    fn resolve_hyphenated_name() {
        assert_eq!(resolve_crate_name("my-custom"), "room-plugin-my-custom");
    }

    // ── short_name ──────────────────────────────────────────────────────

    #[test]
    fn short_name_strips_prefix() {
        assert_eq!(short_name("room-plugin-agent"), "agent");
    }

    #[test]
    fn short_name_no_prefix() {
        assert_eq!(short_name("custom"), "custom");
    }

    // ── lib_filename ────────────────────────────────────────────────────

    #[test]
    fn lib_filename_replaces_hyphens() {
        let name = lib_filename("room-plugin-agent");
        assert!(name.starts_with("libroom_plugin_agent."));
        // Extension is platform-dependent.
        assert!(name.ends_with(".so") || name.ends_with(".dylib"));
    }

    // ── PluginMeta serialization ────────────────────────────────────────

    #[test]
    fn meta_roundtrip() {
        let meta = PluginMeta {
            name: "agent".to_owned(),
            crate_name: "room-plugin-agent".to_owned(),
            version: "3.4.0".to_owned(),
            min_protocol: "3.0.0".to_owned(),
            lib_file: "libroom_plugin_agent.so".to_owned(),
        };
        let json = serde_json::to_string(&meta).unwrap();
        let parsed: PluginMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, meta);
    }

    #[test]
    fn meta_pretty_print() {
        let meta = PluginMeta {
            name: "taskboard".to_owned(),
            crate_name: "room-plugin-taskboard".to_owned(),
            version: "1.0.0".to_owned(),
            min_protocol: "0.0.0".to_owned(),
            lib_file: "libroom_plugin_taskboard.so".to_owned(),
        };
        let json = serde_json::to_string_pretty(&meta).unwrap();
        assert!(json.contains("\"name\": \"taskboard\""));
        assert!(json.contains("\"version\": \"1.0.0\""));
    }

    // ── scan_installed ──────────────────────────────────────────────────

    #[test]
    fn scan_empty_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let result = scan_installed(dir.path());
        assert!(result.is_empty());
    }

    #[test]
    fn scan_nonexistent_dir() {
        let result = scan_installed(Path::new("/nonexistent/plugins"));
        assert!(result.is_empty());
    }

    #[test]
    fn scan_finds_valid_meta_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let meta = PluginMeta {
            name: "test".to_owned(),
            crate_name: "room-plugin-test".to_owned(),
            version: "0.1.0".to_owned(),
            min_protocol: "0.0.0".to_owned(),
            lib_file: "libroom_plugin_test.so".to_owned(),
        };
        let meta_file = dir.path().join("test.meta.json");
        std::fs::write(&meta_file, serde_json::to_string(&meta).unwrap()).unwrap();

        let result = scan_installed(dir.path());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "test");
    }

    #[test]
    fn scan_skips_invalid_json() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("bad.meta.json"), "not json").unwrap();
        let result = scan_installed(dir.path());
        assert!(result.is_empty());
    }

    #[test]
    fn scan_skips_non_meta_json() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("config.json"), "{}").unwrap();
        let result = scan_installed(dir.path());
        assert!(result.is_empty());
    }

    #[test]
    fn scan_sorts_by_name() {
        let dir = tempfile::TempDir::new().unwrap();
        for name in &["zebra", "alpha", "mid"] {
            let meta = PluginMeta {
                name: name.to_string(),
                crate_name: format!("room-plugin-{name}"),
                version: "0.1.0".to_owned(),
                min_protocol: "0.0.0".to_owned(),
                lib_file: format!("libroom_plugin_{name}.so"),
            };
            std::fs::write(
                dir.path().join(format!("{name}.meta.json")),
                serde_json::to_string(&meta).unwrap(),
            )
            .unwrap();
        }
        let result = scan_installed(dir.path());
        let names: Vec<&str> = result.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mid", "zebra"]);
    }

    // ── meta_path ───────────────────────────────────────────────────────

    #[test]
    fn meta_path_format() {
        let p = meta_path(Path::new("/home/user/.room/plugins"), "agent");
        assert_eq!(p, PathBuf::from("/home/user/.room/plugins/agent.meta.json"));
    }

    // ── cmd_remove ──────────────────────────────────────────────────────

    #[test]
    fn remove_nonexistent_plugin_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let result = cmd_remove(dir.path(), "nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not installed"));
    }

    #[test]
    fn remove_deletes_lib_and_meta() {
        let dir = tempfile::TempDir::new().unwrap();
        let meta = PluginMeta {
            name: "test".to_owned(),
            crate_name: "room-plugin-test".to_owned(),
            version: "0.1.0".to_owned(),
            min_protocol: "0.0.0".to_owned(),
            lib_file: "libroom_plugin_test.so".to_owned(),
        };
        std::fs::write(
            dir.path().join("test.meta.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();
        std::fs::write(dir.path().join("libroom_plugin_test.so"), b"fake").unwrap();

        cmd_remove(dir.path(), "test").unwrap();
        assert!(!dir.path().join("test.meta.json").exists());
        assert!(!dir.path().join("libroom_plugin_test.so").exists());
    }

    // ── walkdir ─────────────────────────────────────────────────────────

    #[test]
    fn walkdir_finds_nested_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let nested = dir.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("target.so"), b"lib").unwrap();
        std::fs::write(dir.path().join("top.txt"), b"top").unwrap();

        let files = walkdir(dir.path());
        assert!(files.iter().any(|p| p.ends_with("target.so")));
        assert!(files.iter().any(|p| p.ends_with("top.txt")));
    }

    #[test]
    fn walkdir_empty_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let files = walkdir(dir.path());
        assert!(files.is_empty());
    }

    // ── find_built_lib ──────────────────────────────────────────────────

    #[test]
    fn find_built_lib_success() {
        let dir = tempfile::TempDir::new().unwrap();
        let release = dir.path().join("target").join("release");
        std::fs::create_dir_all(&release).unwrap();
        std::fs::write(release.join("libroom_plugin_test.so"), b"elf").unwrap();

        let result = find_built_lib(dir.path(), "libroom_plugin_test.so");
        assert!(result.is_ok());
        assert!(result.unwrap().ends_with("libroom_plugin_test.so"));
    }

    #[test]
    fn find_built_lib_not_found() {
        let dir = tempfile::TempDir::new().unwrap();
        let result = find_built_lib(dir.path(), "nonexistent.so");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    // ── cmd_install duplicate check ─────────────────────────────────────

    #[test]
    fn install_skips_when_already_installed() {
        let dir = tempfile::TempDir::new().unwrap();
        let meta = PluginMeta {
            name: "existing".to_owned(),
            crate_name: "room-plugin-existing".to_owned(),
            version: "1.0.0".to_owned(),
            min_protocol: "0.0.0".to_owned(),
            lib_file: "libroom_plugin_existing.so".to_owned(),
        };
        std::fs::write(
            dir.path().join("existing.meta.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();

        // Should succeed without error (prints "already installed").
        let result = cmd_install(dir.path(), "existing", None);
        assert!(result.is_ok());
    }

    // ── cmd_update when not installed ───────────────────────────────────

    #[test]
    fn update_nonexistent_plugin_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let result = cmd_update(dir.path(), "nonexistent", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not installed"));
    }

    // ── builtin plugins ─────────────────────────────────────────────────

    #[test]
    fn builtin_plugins_has_four_entries() {
        assert_eq!(BUILTIN_PLUGINS.len(), 4);
    }

    #[test]
    fn builtin_plugins_includes_expected_names() {
        let names: Vec<&str> = BUILTIN_PLUGINS.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"agent"));
        assert!(names.contains(&"taskboard"));
        assert!(names.contains(&"queue"));
        assert!(names.contains(&"stats"));
    }

    #[test]
    fn builtin_plugins_sorted_alphabetically() {
        let names: Vec<&str> = BUILTIN_PLUGINS.iter().map(|(n, _)| *n).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }
}
