//! Fixed AOS product identity for the MCP broker.

/// Product identity used by the single AOS MCP broker capsule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpIdentity {
    /// Capsule component id.
    pub capsule_name: &'static str,
    /// Full MCP tool prefix including the trailing separator.
    pub mcp_tool_prefix: &'static str,
    /// Log tag.
    pub log_tag: &'static str,
    /// Tool-list publication topic.
    pub tools_list_topic: &'static str,
    /// Audit topic prefix.
    pub audit_topic_prefix: &'static str,
}

impl McpIdentity {
    /// The singleton product identity.
    pub const AOS: Self = Self {
        capsule_name: "aos-mcp",
        mcp_tool_prefix: "mcp__aos__",
        log_tag: "aos-mcp",
        tools_list_topic: "astrid.v1.tools.list",
        audit_topic_prefix: "astrid.v1.audit.",
    };

    /// Build a full audit topic.
    #[must_use]
    pub fn audit_topic(self, event: &str) -> String {
        let mut topic = String::with_capacity(self.audit_topic_prefix.len() + event.len());
        topic.push_str(self.audit_topic_prefix);
        topic.push_str(event);
        topic
    }
}
