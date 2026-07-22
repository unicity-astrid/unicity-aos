//! AOS-owned MCP broker over Astrid Runtime's neutral event bus.

#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]
#![warn(missing_docs)]

mod approval;
mod broker;
mod cache;
mod discovery;
mod execute;
mod grant_decision;
mod hook_gate;
mod identity;
mod policy;
mod profile;

pub use identity::McpIdentity;
pub use profile::install_aos;

/// Capsule entry points used by the thin `aos-mcp` component.
pub mod handlers {
    use astrid_sdk::prelude::*;

    /// Assemble and publish the tool list.
    pub fn describe_tools(_payload: serde_json::Value) -> Result<(), SysError> {
        crate::discovery::describe_tools();
        Ok(())
    }

    /// Merge an event-driven tool descriptor response.
    pub fn collect_tool_descriptors(payload: serde_json::Value) -> Result<(), SysError> {
        crate::discovery::collect_tool_descriptors(payload);
        Ok(())
    }

    /// Invalidate tool discovery after the loaded capsule set changes.
    pub fn handle_capsules_changed(payload: serde_json::Value) -> Result<(), SysError> {
        crate::discovery::on_capsules_loaded(payload);
        Ok(())
    }

    /// Handle `astrid.v1.request.mcp.tools.list`.
    pub fn handle_mcp_list(payload: serde_json::Value) -> Result<(), SysError> {
        crate::broker::handle_mcp_list(payload)
    }

    /// Handle `astrid.v1.request.mcp.tool.call`.
    pub fn handle_mcp_call(payload: serde_json::Value) -> Result<(), SysError> {
        crate::broker::handle_mcp_call(payload)
    }

    /// Handle constrained capability approval decisions.
    pub fn handle_mcp_approval(payload: serde_json::Value) -> Result<(), SysError> {
        crate::approval::handle_mcp_approval(payload)
    }

    /// Handle ingress-consent decisions.
    pub fn handle_mcp_ingress_respond(payload: serde_json::Value) -> Result<(), SysError> {
        crate::approval::handle_mcp_ingress_respond(payload)
    }

    /// Handle capsule-grant decisions.
    pub fn handle_mcp_grant_respond(payload: serde_json::Value) -> Result<(), SysError> {
        crate::approval::handle_mcp_grant_respond(payload)
    }

    /// Handle native host tool-call policy hooks.
    pub fn handle_before_tool_call(payload: serde_json::Value) -> Result<(), SysError> {
        crate::hook_gate::handle_before_tool_call(payload)
    }
}
