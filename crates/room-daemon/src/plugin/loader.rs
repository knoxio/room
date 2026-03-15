//! Dynamic plugin loader using libloading.
//!
//! Scans `~/.room/plugins/` for shared libraries (`.so` on Linux, `.dylib` on
//! macOS) and loads them via the C ABI entry points defined in
//! [`room_protocol::plugin::abi`].
//!
//! Each loaded plugin goes through three stages:
//! 1. Load the shared library
//! 2. Read the `ROOM_PLUGIN_DECLARATION` static to verify API/protocol compat
//! 3. Call `room_plugin_create` to obtain a `Box<dyn Plugin>`
//!
//! On drop, the loader calls `room_plugin_destroy` before unloading the library.

use std::path::{Path, PathBuf};

use room_protocol::plugin::abi::{
    CreateFn, DestroyFn, PluginDeclaration, CREATE_SYMBOL, DECLARATION_SYMBOL, DESTROY_SYMBOL,
};
use room_protocol::plugin::{Plugin, PLUGIN_API_VERSION, PROTOCOL_VERSION};

/// A dynamically loaded plugin and its backing library handle.
///
/// The library must outlive the plugin — `Drop` calls the destroy function
/// before the library is unloaded.
pub struct LoadedPlugin {
    plugin: *mut Box<dyn Plugin>,
    destroy_fn: DestroyFn,
    _library: libloading::Library,
    /// Path to the shared library (for diagnostics).
    pub path: PathBuf,
}

// SAFETY: The plugin trait object is Send + Sync (required by the Plugin trait),
// and we only call destroy_fn once in Drop.
unsafe impl Send for LoadedPlugin {}
unsafe impl Sync for LoadedPlugin {}

impl std::fmt::Debug for LoadedPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedPlugin")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl LoadedPlugin {
    /// Get a reference to the loaded plugin.
    pub fn plugin(&self) -> &dyn Plugin {
        // SAFETY: plugin was returned by CreateFn and has not been destroyed.
        unsafe { &**self.plugin }
    }

    /// Consume this wrapper and return the boxed plugin trait object.
    ///
    /// The caller takes ownership of the plugin and is responsible for
    /// ensuring the library outlives it. The destroy function will NOT
    /// be called — the caller must arrange cleanup.
    ///
    /// # Safety
    ///
    /// The returned `Box<dyn Plugin>` references vtable entries inside the
    /// shared library. The library (`_library` field) is dropped when this
    /// struct is consumed, so the caller must ensure the plugin is dropped
    /// before the library would be unloaded. In practice, this is safe when
    /// the plugin is registered into the PluginRegistry and the registry
    /// is dropped before process exit (which is the normal lifecycle).
    pub unsafe fn into_boxed_plugin(self) -> Box<dyn Plugin> {
        let plugin = *Box::from_raw(self.plugin);
        // Prevent Drop from calling destroy_fn — we transferred ownership.
        std::mem::forget(self);
        plugin
    }
}

impl Drop for LoadedPlugin {
    fn drop(&mut self) {
        // SAFETY: plugin was returned by CreateFn from the same library,
        // and we only call destroy once (Drop runs exactly once).
        unsafe {
            (self.destroy_fn)(self.plugin);
        }
    }
}

/// Errors that can occur when loading a plugin.
#[derive(Debug)]
pub enum LoadError {
    /// Failed to open the shared library.
    LibraryOpen(String),
    /// A required symbol was not found.
    SymbolNotFound(String),
    /// The plugin's API version does not match the broker's.
    ApiVersionMismatch { expected: u32, found: u32 },
    /// The plugin's minimum protocol version is newer than the running broker.
    ProtocolMismatch { required: String, running: String },
    /// UTF-8 decoding failed on a declaration string field.
    InvalidUtf8(String),
    /// The create function returned a null pointer.
    CreateReturnedNull,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LibraryOpen(e) => write!(f, "failed to open library: {e}"),
            Self::SymbolNotFound(s) => write!(f, "symbol not found: {s}"),
            Self::ApiVersionMismatch { expected, found } => {
                write!(
                    f,
                    "API version mismatch: expected {expected}, found {found}"
                )
            }
            Self::ProtocolMismatch { required, running } => {
                write!(
                    f,
                    "protocol mismatch: plugin requires {required}, running {running}"
                )
            }
            Self::InvalidUtf8(field) => write!(f, "invalid UTF-8 in declaration field: {field}"),
            Self::CreateReturnedNull => write!(f, "plugin create function returned null"),
        }
    }
}

impl std::error::Error for LoadError {}

/// Load a plugin from a shared library at the given path.
///
/// Validates the `PluginDeclaration` against the running broker's API and
/// protocol versions before calling the create function.
///
/// # Safety
///
/// Loading a shared library executes its initialization routines, which may
/// have arbitrary side effects. Only load trusted libraries.
pub fn load_plugin(path: &Path, config_json: Option<&str>) -> Result<LoadedPlugin, LoadError> {
    // SAFETY: loading a shared library is inherently unsafe — it runs init
    // routines. We trust the caller to only load vetted plugin libraries.
    let library = unsafe { libloading::Library::new(path) }
        .map_err(|e| LoadError::LibraryOpen(format!("{}: {e}", path.display())))?;

    // Read the declaration static.
    let declaration: &PluginDeclaration = unsafe {
        let sym = library
            .get::<*const PluginDeclaration>(DECLARATION_SYMBOL)
            .map_err(|e| LoadError::SymbolNotFound(format!("ROOM_PLUGIN_DECLARATION: {e}")))?;
        &**sym
    };

    // Validate API version.
    if declaration.api_version != PLUGIN_API_VERSION {
        return Err(LoadError::ApiVersionMismatch {
            expected: PLUGIN_API_VERSION,
            found: declaration.api_version,
        });
    }

    // Validate protocol version (plugin's minimum must not exceed ours).
    let min_protocol = unsafe {
        declaration
            .min_protocol()
            .map_err(|_| LoadError::InvalidUtf8("min_protocol".to_owned()))?
    };
    if !protocol_satisfies(min_protocol, PROTOCOL_VERSION) {
        return Err(LoadError::ProtocolMismatch {
            required: min_protocol.to_owned(),
            running: PROTOCOL_VERSION.to_owned(),
        });
    }

    // Look up create and destroy functions.
    let create_fn: CreateFn = unsafe {
        *library
            .get::<CreateFn>(CREATE_SYMBOL)
            .map_err(|e| LoadError::SymbolNotFound(format!("room_plugin_create: {e}")))?
    };
    let destroy_fn: DestroyFn = unsafe {
        *library
            .get::<DestroyFn>(DESTROY_SYMBOL)
            .map_err(|e| LoadError::SymbolNotFound(format!("room_plugin_destroy: {e}")))?
    };

    // Call the create function.
    let (config_ptr, config_len) = match config_json {
        Some(s) => (s.as_ptr(), s.len()),
        None => (std::ptr::null(), 0),
    };
    let plugin = unsafe { create_fn(config_ptr, config_len) };
    if plugin.is_null() {
        return Err(LoadError::CreateReturnedNull);
    }

    Ok(LoadedPlugin {
        plugin,
        destroy_fn,
        _library: library,
        path: path.to_owned(),
    })
}

/// Scan a directory for plugin shared libraries and load each one.
///
/// Returns successfully loaded plugins and logs warnings for any that fail.
/// An empty or nonexistent directory returns an empty vec (not an error).
pub fn scan_plugin_dir(dir: &Path) -> Vec<LoadedPlugin> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut plugins = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_shared_lib(&path) {
            continue;
        }
        match load_plugin(&path, None) {
            Ok(loaded) => {
                let name = loaded.plugin().name().to_owned();
                eprintln!(
                    "[plugin] loaded external plugin '{}' from {}",
                    name,
                    path.display()
                );
                plugins.push(loaded);
            }
            Err(e) => {
                eprintln!("[plugin] failed to load plugin {}: {e}", path.display());
            }
        }
    }
    plugins
}

/// Check if a path looks like a shared library (`.so` or `.dylib`).
fn is_shared_lib(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext == "so" || ext == "dylib")
}

/// Check if `running` satisfies `required` (i.e. running >= required).
///
/// Compares major.minor.patch numerically. Returns true if the running
/// version is greater than or equal to the required version.
fn protocol_satisfies(required: &str, running: &str) -> bool {
    let parse = |s: &str| -> Option<(u64, u64, u64)> {
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() < 3 {
            return None;
        }
        Some((
            parts[0].parse().ok()?,
            parts[1].parse().ok()?,
            parts[2].parse().ok()?,
        ))
    };

    match (parse(required), parse(running)) {
        (Some(req), Some(run)) => run >= req,
        // If either fails to parse, be permissive — let the plugin load.
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_shared_lib_recognizes_so() {
        assert!(is_shared_lib(Path::new("/tmp/plugins/myplugin.so")));
    }

    #[test]
    fn is_shared_lib_recognizes_dylib() {
        assert!(is_shared_lib(Path::new("/tmp/plugins/myplugin.dylib")));
    }

    #[test]
    fn is_shared_lib_rejects_other_extensions() {
        assert!(!is_shared_lib(Path::new("/tmp/plugins/myplugin.toml")));
        assert!(!is_shared_lib(Path::new("/tmp/plugins/myplugin.json")));
        assert!(!is_shared_lib(Path::new("/tmp/plugins/myplugin.rs")));
        assert!(!is_shared_lib(Path::new("/tmp/plugins/README")));
    }

    #[test]
    fn is_shared_lib_rejects_no_extension() {
        assert!(!is_shared_lib(Path::new("/tmp/plugins/myplugin")));
    }

    #[test]
    fn protocol_satisfies_exact_match() {
        assert!(protocol_satisfies("3.4.0", "3.4.0"));
    }

    #[test]
    fn protocol_satisfies_running_newer() {
        assert!(protocol_satisfies("3.0.0", "3.4.0"));
        assert!(protocol_satisfies("2.0.0", "3.4.0"));
    }

    #[test]
    fn protocol_satisfies_running_older_fails() {
        assert!(!protocol_satisfies("4.0.0", "3.4.0"));
        assert!(!protocol_satisfies("3.5.0", "3.4.0"));
    }

    #[test]
    fn protocol_satisfies_zero_always_passes() {
        assert!(protocol_satisfies("0.0.0", "3.4.0"));
    }

    #[test]
    fn protocol_satisfies_unparseable_is_permissive() {
        assert!(protocol_satisfies("bad", "3.4.0"));
        assert!(protocol_satisfies("3.4.0", "bad"));
    }

    #[test]
    fn load_plugin_nonexistent_path_returns_error() {
        let result = load_plugin(Path::new("/nonexistent/plugin.so"), None);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, LoadError::LibraryOpen(_)));
    }

    #[test]
    fn scan_plugin_dir_empty_dir_returns_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let plugins = scan_plugin_dir(dir.path());
        assert!(plugins.is_empty());
    }

    #[test]
    fn scan_plugin_dir_nonexistent_returns_empty() {
        let plugins = scan_plugin_dir(Path::new("/nonexistent/plugins"));
        assert!(plugins.is_empty());
    }

    #[test]
    fn scan_plugin_dir_skips_non_library_files() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("readme.txt"), "not a plugin").unwrap();
        std::fs::write(dir.path().join("config.toml"), "[plugin]").unwrap();
        let plugins = scan_plugin_dir(dir.path());
        assert!(plugins.is_empty());
    }

    #[test]
    fn load_error_display_messages() {
        let e = LoadError::LibraryOpen("no such file".into());
        assert!(e.to_string().contains("no such file"));

        let e = LoadError::ApiVersionMismatch {
            expected: 1,
            found: 2,
        };
        assert!(e.to_string().contains("expected 1"));
        assert!(e.to_string().contains("found 2"));

        let e = LoadError::ProtocolMismatch {
            required: "4.0.0".into(),
            running: "3.4.0".into(),
        };
        assert!(e.to_string().contains("4.0.0"));

        let e = LoadError::CreateReturnedNull;
        assert!(e.to_string().contains("null"));
    }
}
