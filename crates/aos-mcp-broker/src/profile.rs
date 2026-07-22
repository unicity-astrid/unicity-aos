//! Active product identity for the AOS-owned broker.

use std::sync::OnceLock;

use crate::identity::McpIdentity;

static IDENTITY: OnceLock<&'static McpIdentity> = OnceLock::new();

/// Install the singleton AOS identity if it is not already set.
pub fn install_aos() {
    match IDENTITY.set(&McpIdentity::AOS) {
        Ok(()) => {}
        Err(existing) => assert!(
            core::ptr::eq(existing, &McpIdentity::AOS),
            "aos-mcp-broker: identity already installed"
        ),
    }
}

#[inline]
fn identity() -> &'static McpIdentity {
    IDENTITY
        .get()
        .copied()
        .expect("aos-mcp-broker: call install_aos before handling traffic")
}

#[inline]
pub(crate) fn log_tag() -> &'static str {
    identity().log_tag
}

#[inline]
pub(crate) fn mcp_tool_prefix() -> &'static str {
    identity().mcp_tool_prefix
}

#[inline]
pub(crate) fn tools_list_topic() -> &'static str {
    identity().tools_list_topic
}

#[inline]
pub(crate) fn audit_topic(event: &str) -> String {
    identity().audit_topic(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_is_idempotent_and_product_scoped() {
        install_aos();
        install_aos();
        assert_eq!(identity().capsule_name, "aos-mcp");
        assert_eq!(mcp_tool_prefix(), "mcp__aos__");
    }
}
