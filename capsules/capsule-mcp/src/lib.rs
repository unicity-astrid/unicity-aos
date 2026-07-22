//! AOS MCP broker capsule shared by Codex, Claude, and Grok host adapters.

#![deny(unsafe_code)]
#![deny(clippy::all)]

use aos_mcp_broker::handlers;
use astrid_sdk::prelude::*;

mod host_hooks;

/// Unicity AOS MCP broker capsule over the neutral runtime wire.
#[derive(Default)]
pub struct AosMcp;

fn ensure_identity() {
    aos_mcp_broker::install_aos();
}

#[capsule]
impl AosMcp {
    /// Token-validated hook ingress from the Codex plugin.
    #[astrid::interceptor("handle_codex_hook")]
    pub fn handle_codex_hook(&self, payload: serde_json::Value) -> Result<(), SysError> {
        ensure_identity();
        host_hooks::handle("codex", payload)
    }

    /// Token-validated hook ingress from the Claude plugin.
    #[astrid::interceptor("handle_claude_hook")]
    pub fn handle_claude_hook(&self, payload: serde_json::Value) -> Result<(), SysError> {
        ensure_identity();
        host_hooks::handle("claude", payload)
    }

    /// Token-validated hook ingress from the Grok plugin.
    #[astrid::interceptor("handle_grok_hook")]
    pub fn handle_grok_hook(&self, payload: serde_json::Value) -> Result<(), SysError> {
        ensure_identity();
        host_hooks::handle("grok", payload)
    }

    /// Relay a validated bridge response to the authenticated host call.
    #[astrid::interceptor("relay_host_hook_response")]
    pub fn relay_host_hook_response(&self, payload: serde_json::Value) -> Result<(), SysError> {
        ensure_identity();
        host_hooks::relay_response(payload)
    }

    /// Handle tool discovery requests.
    #[astrid::interceptor("describe_tools")]
    pub fn describe_tools(&self, payload: serde_json::Value) -> Result<(), SysError> {
        ensure_identity();
        handlers::describe_tools(payload)
    }

    /// Merge a tool descriptor response.
    #[astrid::interceptor("collect_tool_descriptors")]
    pub fn collect_tool_descriptors(&self, payload: serde_json::Value) -> Result<(), SysError> {
        ensure_identity();
        handlers::collect_tool_descriptors(payload)
    }

    /// Invalidate discovery after the capsule set changes.
    #[astrid::interceptor("handle_capsules_changed")]
    pub fn handle_capsules_changed(&self, payload: serde_json::Value) -> Result<(), SysError> {
        ensure_identity();
        handlers::handle_capsules_changed(payload)
    }

    /// Serve a sanitized MCP tool list.
    #[astrid::interceptor("handle_mcp_list")]
    pub fn handle_mcp_list(&self, payload: serde_json::Value) -> Result<(), SysError> {
        ensure_identity();
        handlers::handle_mcp_list(payload)
    }

    /// Broker one MCP tool call.
    #[astrid::interceptor("handle_mcp_call")]
    pub fn handle_mcp_call(&self, payload: serde_json::Value) -> Result<(), SysError> {
        ensure_identity();
        handlers::handle_mcp_call(payload)
    }

    /// Apply an elicited capability approval decision.
    #[astrid::interceptor("handle_mcp_approval")]
    pub fn handle_mcp_approval(&self, payload: serde_json::Value) -> Result<(), SysError> {
        ensure_identity();
        handlers::handle_mcp_approval(payload)
    }

    /// Apply an elicited ingress-consent decision.
    #[astrid::interceptor("handle_mcp_ingress_respond")]
    pub fn handle_mcp_ingress_respond(&self, payload: serde_json::Value) -> Result<(), SysError> {
        ensure_identity();
        handlers::handle_mcp_ingress_respond(payload)
    }

    /// Apply an elicited capsule-grant decision.
    #[astrid::interceptor("handle_mcp_grant_respond")]
    pub fn handle_mcp_grant_respond(&self, payload: serde_json::Value) -> Result<(), SysError> {
        ensure_identity();
        handlers::handle_mcp_grant_respond(payload)
    }

    /// Answer native host policy hooks.
    #[astrid::interceptor("handle_before_tool_call")]
    pub fn handle_before_tool_call(&self, payload: serde_json::Value) -> Result<(), SysError> {
        ensure_identity();
        handlers::handle_before_tool_call(payload)
    }
}
