//! CLI handlers for `room plugin list|remove|update`.
//!
//! Scans `~/.room/plugins/` for installed plugins (identified by `.meta.json`
//! sidecar files) and provides lifecycle management commands.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Metadata sidecar for an installed plugin.
///
/// Written by `room plugin install` alongside the compiled `.so`/`.dylib`.
/// Format: `~/.room/plugins/<name>.meta.json`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginMeta {
    /// Plugin name (e.g. "agent", "taskboard").
    pub name: String,
    /// Installed version (semver).
    pub version: String,
    /// Source crate name on crates.io (e.g. "room-plugin-agent").
    pub crate_name: String,
    /// Compatible room-protocol version range (semver requirement string).
    pub protocol_compat: String,
    /// Filename of the compiled shared library (e.g. "libroom_plugin_agent.so").
    pub lib_file: String,
}

/// Return the plugin directory path (`~/.room/plugins/`).
pub fn plugins_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
    PathBuf::from(home).join(".room").join("plugins")
}

/// Scan `~/.room/plugins/` for installed plugins by reading `.meta.json` files.
pub fn scan_plugins(dir: &Path) -> Vec<PluginMeta> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut plugins = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json")
            && path
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

/// List installed plugins in a formatted table.
pub fn cmd_list() {
    let dir = plugins_dir();
    let plugins = scan_plugins(&dir);

    if plugins.is_empty() {
        println!("no plugins installed");
        println!("  plugin directory: {}", dir.display());
        return;
    }

    let header = format_plugin_row("NAME", "VERSION", "CRATE", "PROTOCOL COMPAT");
    println!("{header}");
    for p in &plugins {
        let row = format_plugin_row(&p.name, &p.version, &p.crate_name, &p.protocol_compat);
        println!("{row}");
    }
    println!("\n{} plugin(s) installed", plugins.len());
}

fn format_plugin_row(name: &str, version: &str, crate_name: &str, compat: &str) -> String {
    format!("{name:<20} {version:<12} {crate_name:<30} {compat}")
}

/// Remove a plugin by name — deletes both the shared library and the `.meta.json`.
pub fn cmd_remove(name: &str) -> anyhow::Result<()> {
    let dir = plugins_dir();
    let meta_path = dir.join(format!("{name}.meta.json"));

    if !meta_path.exists() {
        anyhow::bail!("plugin '{name}' is not installed (no {name}.meta.json found)");
    }

    let meta_data = std::fs::read_to_string(&meta_path)?;
    let meta: PluginMeta = serde_json::from_str(&meta_data)?;

    // Remove the shared library.
    let lib_path = dir.join(&meta.lib_file);
    if lib_path.exists() {
        std::fs::remove_file(&lib_path)?;
        println!("removed {}", lib_path.display());
    }

    // Remove the metadata file.
    std::fs::remove_file(&meta_path)?;
    println!("removed {}", meta_path.display());

    println!("plugin '{name}' v{} uninstalled", meta.version);
    Ok(())
}

/// Update a plugin by re-downloading and recompiling from crates.io.
///
/// This is a placeholder — the actual download+compile logic depends on
/// the install infrastructure from #745. For now, it checks if the plugin
/// exists and reports what would happen.
pub fn cmd_update(name: &str) -> anyhow::Result<()> {
    let dir = plugins_dir();
    let meta_path = dir.join(format!("{name}.meta.json"));

    if !meta_path.exists() {
        anyhow::bail!("plugin '{name}' is not installed — use `room plugin install {name}` first");
    }

    let meta_data = std::fs::read_to_string(&meta_path)?;
    let meta: PluginMeta = serde_json::from_str(&meta_data)?;

    // TODO(#745): query crates.io for latest version of meta.crate_name,
    // compare with meta.version, download + compile if newer.
    println!(
        "plugin '{name}' is at v{} (crate: {})",
        meta.version, meta.crate_name
    );
    println!("update check requires crates.io integration (#745)");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_meta(dir: &Path, meta: &PluginMeta) {
        let path = dir.join(format!("{}.meta.json", meta.name));
        let data = serde_json::to_string_pretty(meta).unwrap();
        std::fs::write(path, data).unwrap();
    }

    fn sample_meta(name: &str) -> PluginMeta {
        PluginMeta {
            name: name.to_owned(),
            version: "1.0.0".to_owned(),
            crate_name: format!("room-plugin-{name}"),
            protocol_compat: ">=3.0.0, <4.0.0".to_owned(),
            lib_file: format!("libroom_plugin_{name}.so"),
        }
    }

    #[test]
    fn scan_empty_dir_returns_empty() {
        let dir = TempDir::new().unwrap();
        let plugins = scan_plugins(dir.path());
        assert!(plugins.is_empty());
    }

    #[test]
    fn scan_nonexistent_dir_returns_empty() {
        let plugins = scan_plugins(Path::new("/nonexistent/path/plugins"));
        assert!(plugins.is_empty());
    }

    #[test]
    fn scan_finds_valid_meta_files() {
        let dir = TempDir::new().unwrap();
        write_meta(dir.path(), &sample_meta("agent"));
        write_meta(dir.path(), &sample_meta("taskboard"));

        let plugins = scan_plugins(dir.path());
        assert_eq!(plugins.len(), 2);
        assert_eq!(plugins[0].name, "agent");
        assert_eq!(plugins[1].name, "taskboard");
    }

    #[test]
    fn scan_skips_invalid_json() {
        let dir = TempDir::new().unwrap();
        write_meta(dir.path(), &sample_meta("valid"));
        std::fs::write(dir.path().join("broken.meta.json"), "not json").unwrap();

        let plugins = scan_plugins(dir.path());
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "valid");
    }

    #[test]
    fn scan_skips_non_meta_json_files() {
        let dir = TempDir::new().unwrap();
        write_meta(dir.path(), &sample_meta("real"));
        std::fs::write(dir.path().join("other.json"), r#"{"key":"val"}"#).unwrap();
        std::fs::write(dir.path().join("readme.txt"), "hello").unwrap();

        let plugins = scan_plugins(dir.path());
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "real");
    }

    #[test]
    fn remove_nonexistent_returns_error() {
        let dir = TempDir::new().unwrap();
        // Override HOME to use temp dir
        let orig_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", dir.path());
        std::fs::create_dir_all(dir.path().join(".room/plugins")).unwrap();

        let result = cmd_remove("nonexistent");
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("not installed"),
            "error should mention not installed"
        );

        // Restore HOME
        match orig_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn remove_deletes_meta_and_lib() {
        let dir = TempDir::new().unwrap();
        let plugins_path = dir.path().join(".room").join("plugins");
        std::fs::create_dir_all(&plugins_path).unwrap();

        let meta = sample_meta("test-plugin");
        write_meta(&plugins_path, &meta);
        // Create a fake lib file
        std::fs::write(plugins_path.join(&meta.lib_file), b"fake lib").unwrap();

        let orig_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", dir.path());

        let result = cmd_remove("test-plugin");
        assert!(result.is_ok(), "remove should succeed: {:?}", result.err());

        assert!(
            !plugins_path.join("test-plugin.meta.json").exists(),
            "meta file should be deleted"
        );
        assert!(
            !plugins_path.join(&meta.lib_file).exists(),
            "lib file should be deleted"
        );

        match orig_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn update_nonexistent_returns_error() {
        let dir = TempDir::new().unwrap();
        let orig_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", dir.path());
        std::fs::create_dir_all(dir.path().join(".room/plugins")).unwrap();

        let result = cmd_update("nonexistent");
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("not installed"),
            "error should mention not installed"
        );

        match orig_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn meta_serialization_roundtrip() {
        let meta = sample_meta("roundtrip");
        let json = serde_json::to_string(&meta).unwrap();
        let parsed: PluginMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "roundtrip");
        assert_eq!(parsed.version, "1.0.0");
        assert_eq!(parsed.crate_name, "room-plugin-roundtrip");
        assert_eq!(parsed.protocol_compat, ">=3.0.0, <4.0.0");
        assert_eq!(parsed.lib_file, "libroom_plugin_roundtrip.so");
    }

    #[test]
    fn plugins_dir_uses_home() {
        let dir = plugins_dir();
        assert!(
            dir.to_string_lossy().contains("plugins"),
            "plugins_dir should contain 'plugins'"
        );
    }
}
