use std::collections::HashMap;
use std::path::Path;

/// Write a token map to disk as JSON.
pub(crate) fn save_token_map(map: &HashMap<String, String>, path: &Path) -> Result<(), String> {
    let json = serde_json::to_string_pretty(&map).map_err(|e| format!("serialize tokens: {e}"))?;
    std::fs::write(path, json).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Load a token map from disk. Returns an empty map if the file does not exist.
///
/// `token_map_path` is the `.tokens` file (see [`crate::paths::broker_tokens_path`]).
pub(crate) fn load_token_map(token_map_path: &Path) -> HashMap<String, String> {
    let contents = match std::fs::read_to_string(token_map_path) {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };
    serde_json::from_str(&contents).unwrap_or_else(|e| {
        eprintln!(
            "[auth] corrupt token file {}: {e}",
            token_map_path.display()
        );
        HashMap::new()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_token_map_missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let token_map_path = dir.path().join("nonexistent.tokens");
        let map = load_token_map(&token_map_path);
        assert!(map.is_empty());
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let token_map_path = dir.path().join("test.tokens");

        let mut original = HashMap::new();
        original.insert("tok-1".to_owned(), "alice".to_owned());
        original.insert("tok-2".to_owned(), "bob".to_owned());

        save_token_map(&original, &token_map_path).unwrap();
        let loaded = load_token_map(&token_map_path);
        assert_eq!(loaded, original);
    }

    #[test]
    fn load_token_map_corrupt_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let token_map_path = dir.path().join("corrupt.tokens");
        std::fs::write(&token_map_path, "not json{{{").unwrap();

        let map = load_token_map(&token_map_path);
        assert!(map.is_empty());
    }
}
