use std::path::{Path, PathBuf};

use crate::history;

/// Read the room host username from the meta file, if present.
///
/// Returns `None` if the meta file does not exist, cannot be parsed, or has no
/// `"host"` field. Callers should treat `None` the same as no host information.
pub(in crate::oneshot) fn read_host_from_meta(meta_path: &Path) -> Option<String> {
    if !meta_path.exists() {
        return None;
    }
    let data = std::fs::read_to_string(meta_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    v["host"].as_str().map(str::to_owned)
}

pub(in crate::oneshot) fn chat_path_from_meta(room_id: &str, meta_path: &Path) -> PathBuf {
    if meta_path.exists() {
        if let Ok(data) = std::fs::read_to_string(meta_path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                if let Some(p) = v["chat_path"].as_str() {
                    return PathBuf::from(p);
                }
            }
        }
    }
    history::default_chat_path(room_id)
}
