//! Sample "hello" plugin for the room chat system.
//!
//! This is a minimal external plugin that validates the dynamic plugin
//! pipeline end-to-end: C ABI export, libloading, install/remove/update.
//!
//! Registers a single `/hello` command that responds with a greeting.

use chrono::Utc;

use room_protocol::plugin::{
    BoxFuture, CommandContext, CommandInfo, ParamSchema, ParamType, Plugin, PluginResult,
};

// ── C ABI entry points ────────────────────────────────────────────────────

/// Create a [`HelloPlugin`] from a JSON config string (currently unused).
fn create_hello_from_config(_config: &str) -> HelloPlugin {
    HelloPlugin
}

room_protocol::declare_plugin!("hello", create_hello_from_config);

// ── Plugin implementation ─────────────────────────────────────────────────

/// A minimal plugin that responds to `/hello` with a greeting.
///
/// This plugin exists to validate the full dynamic plugin lifecycle:
/// build as cdylib, install via `room plugin install`, load via libloading,
/// use the `/hello` command, and remove via `room plugin remove`.
pub struct HelloPlugin;

impl Plugin for HelloPlugin {
    fn name(&self) -> &str {
        "hello"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn commands(&self) -> Vec<CommandInfo> {
        vec![CommandInfo {
            name: "hello".to_owned(),
            description: "Say hello — sample plugin for testing the dynamic plugin pipeline"
                .to_owned(),
            usage: "/hello [name]".to_owned(),
            params: vec![ParamSchema {
                name: "name".to_owned(),
                param_type: ParamType::Text,
                required: false,
                description: "Name to greet (defaults to sender)".to_owned(),
            }],
        }]
    }

    fn handle(&self, ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>> {
        Box::pin(async move {
            let target = ctx
                .params
                .first()
                .filter(|s| !s.is_empty())
                .map(String::as_str)
                .unwrap_or(&ctx.sender);

            let now = Utc::now().format("%H:%M:%S");
            let greeting = format!("hello {target}! greetings from the hello plugin at {now}");

            let data = serde_json::json!({
                "plugin": "hello",
                "target": target,
                "sender": ctx.sender,
                "timestamp": Utc::now().to_rfc3339(),
            });

            Ok(PluginResult::Reply(greeting, Some(data)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_name_is_hello() {
        assert_eq!(HelloPlugin.name(), "hello");
    }

    #[test]
    fn plugin_version_matches_crate() {
        assert_eq!(HelloPlugin.version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn plugin_api_version_is_current() {
        assert_eq!(
            HelloPlugin.api_version(),
            room_protocol::plugin::PLUGIN_API_VERSION
        );
    }

    #[test]
    fn plugin_registers_hello_command() {
        let cmds = HelloPlugin.commands();
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].name, "hello");
        assert_eq!(cmds[0].params.len(), 1);
        assert_eq!(cmds[0].params[0].name, "name");
        assert!(!cmds[0].params[0].required);
    }

    // ── ABI entry point tests ────────────────────────────────────────────

    #[test]
    fn abi_declaration_matches_plugin() {
        let decl = &ROOM_PLUGIN_DECLARATION;
        assert_eq!(decl.api_version, room_protocol::plugin::PLUGIN_API_VERSION);
        unsafe {
            assert_eq!(decl.name().unwrap(), "hello");
            assert_eq!(decl.version().unwrap(), env!("CARGO_PKG_VERSION"));
            assert_eq!(decl.min_protocol().unwrap(), "0.0.0");
        }
    }

    #[test]
    fn abi_create_with_empty_config() {
        let plugin_ptr = unsafe { room_plugin_create(std::ptr::null(), 0) };
        assert!(!plugin_ptr.is_null());
        let plugin = unsafe { Box::from_raw(plugin_ptr) };
        assert_eq!(plugin.name(), "hello");
    }

    #[test]
    fn abi_destroy_null_is_safe() {
        unsafe { room_plugin_destroy(std::ptr::null_mut()) };
    }

    #[test]
    fn abi_create_and_destroy_roundtrip() {
        let ptr = unsafe { room_plugin_create(std::ptr::null(), 0) };
        assert!(!ptr.is_null());
        unsafe { room_plugin_destroy(ptr) };
    }
}
