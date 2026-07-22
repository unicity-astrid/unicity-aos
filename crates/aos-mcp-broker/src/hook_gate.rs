//! Native-tool `before_tool_call` verdict responder — aos-mcp as a
//! `ToolCallBefore` merge participant.
//!
//! This is the SECOND plane of the same per-principal PDP. The broker
//! ([`crate::broker`]) gates the `mcp__aos__*` tool plane in-process and
//! un-bypassably. This module gates Claude's NATIVE tools (`Bash`, `Write`,
//! …), which execute inside the agent process and reach no in-process
//! chokepoint — the only lever there is the agent's PreToolUse hook. Both
//! planes evaluate the SAME [`crate::policy`] rule set; one operator policy,
//! two transports.
//!
//! ## Wire contract (the hook-bridge `ToolCallBefore` fan-out)
//!
//! * **inbound** `hook.v1.event.before_tool_call` — a fan-out request
//!   carrying `{ hook, payload, correlation_id? }`. `payload` is the
//!   tool-call info (`{ tool_name, tool_input, … }`). The publisher is
//!   either the kernel hook-bridge (a bus-mediated lifecycle event) or an
//!   external `astrid-gate` PreToolUse hook (the native-tool path) — the
//!   responder does not care which.
//! * **outbound** `hook.v1.response.before_tool_call.<correlation_id>` —
//!   `{ skip: bool, reason? }`. Per [`astrid_capsule_hook_bridge`]'s
//!   `ToolCallBefore` merge, **any** responder returning `skip:true` blocks
//!   the call (deny-wins), so a deny here is binding regardless of other
//!   participants.
//!
//! A request with **no** `correlation_id` is a fire-and-forget / observe
//! fan-out (no reply expected); the responder evaluates nothing and stays
//! silent. Only a correlation-keyed request is a decision request.
//!
//! ## Relationship to the broker's mcp_tool gate
//!
//! The broker's [`crate::broker::pretooluse_gate_reply`] is the MVP native
//! gate (Claude's `mcp_tool` PreToolUse hook → broker → policy). This
//! responder is the production transport the `astrid-gate` binary drives. The
//! two never double-fire: the mcp_tool gate answers `astrid.v1.request.mcp.
//! tool.call`, this answers `hook.v1.event.before_tool_call` — different
//! topics. When `astrid-gate` lands and host install migrates the hook
//! authoring `mcp_tool → astrid-gate`, the mcp_tool gate retires and this
//! becomes the live native-tool path.

use astrid_sdk::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};

/// Reply topic prefix; the egress topic is `<prefix><correlation_id>`.
const REPLY_PREFIX: &str = "hook.v1.response.before_tool_call.";

/// Correlation-id length cap — a UUID-ish token; anything longer is rejected
/// before it can be stamped into an egress topic. Same family as the
/// broker's `req_id` gate.
const MAX_CORRELATION_LEN: usize = 128;

/// Inbound `hook.v1.event.before_tool_call` request.
///
/// Mirrors the hook-bridge `HookEventRequest` shape (`hook`, `payload`,
/// `correlation_id`); we only read `payload` + `correlation_id`. Any other
/// fields are ignored (forward-compat).
#[derive(Debug, Deserialize)]
struct BeforeToolCallEvent {
    #[serde(default)]
    payload: Value,
    #[serde(default)]
    correlation_id: Option<String>,
}

/// Handle a `hook.v1.event.before_tool_call` fan-out.
///
/// Evaluates the native tool named in the payload against the invoking
/// principal's [`crate::policy`] rule set and, when a correlation id is
/// present, replies `{ skip }` on the correlation topic. A `Deny` →
/// `skip:true` (blocks the tool); an `Allow` (incl. the no-rule /
/// load-failure default) → `skip:false` (defers to the agent's own
/// permission flow). Every failure path stays silent rather than erroring —
/// a broken responder must not wedge the fan-out, and a missing reply simply
/// means "no opinion" to the merge.
pub(crate) fn handle_before_tool_call(payload: Value) -> Result<(), SysError> {
    let event: BeforeToolCallEvent = match serde_json::from_value(payload) {
        Ok(v) => v,
        Err(e) => {
            log::warn(format!(
                "{}: before_tool_call: malformed event payload: {e}",
                crate::profile::log_tag()
            ));
            return Ok(());
        }
    };

    // No correlation id → observe/fire-and-forget fan-out (e.g. the kernel
    // bridge with no responder expected). Nothing to answer.
    let Some(correlation_id) = event.correlation_id.as_deref() else {
        return Ok(());
    };
    let Some(reply_topic) = reply_topic(correlation_id) else {
        log::warn(format!(
            "{}: before_tool_call: rejecting unroutable correlation_id '{correlation_id}'",
            crate::profile::log_tag()
        ));
        return Ok(());
    };

    let tool_name = event
        .payload
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tool_input = crate::broker::gate_tool_input(&event.payload);

    let reply = verdict(tool_name, &tool_input);

    if let Err(e) = ipc::publish_json(&reply_topic, &reply) {
        log::warn(format!(
            "{}: before_tool_call: failed to reply on {reply_topic}: {e}",
            crate::profile::log_tag()
        ));
    }
    Ok(())
}

/// Evaluate one native tool call and shape the `ToolCallBefore` reply. The
/// host calls (policy load, deny audit, log) live here; the pure
/// decision→reply mapping is [`verdict_body`] so it stays unit-testable.
fn verdict(tool_name: &str, tool_input: &Value) -> Value {
    let decision = crate::policy::evaluate(&crate::policy::load_rules(), tool_name, tool_input);
    if let crate::policy::Decision::Deny { reason } = &decision {
        log::info(format!(
            "{}: before_tool_call denied native tool '{tool_name}': {reason}",
            crate::profile::log_tag()
        ));
        // Audit on the same `astrid.v1.audit.*` family the other gate uses.
        // Operator rule id only — never reflected arguments (injection).
        let _ = ipc::publish_json(
            &crate::profile::audit_topic("pretooluse_deny"),
            &json!({ "tool": tool_name, "rule": reason }),
        );
    }
    verdict_body(&decision)
}

/// Pure decision → `ToolCallBefore` reply. No host calls — unit-testable.
///
/// `Deny` → `{ skip: true, reason }` (blocks; `reason` is the operator rule
/// id, surfaced for the merge's logging, not a reflected argument). `Allow`
/// → `{ skip: false }` (no veto; the agent's own permission flow decides).
/// It only ever NARROWS — `skip:false` is not an assertion that the tool is
/// safe, only that THIS policy has no objection.
fn verdict_body(decision: &crate::policy::Decision) -> Value {
    match decision {
        crate::policy::Decision::Deny { reason } => json!({ "skip": true, "reason": reason }),
        crate::policy::Decision::Allow => json!({ "skip": false }),
    }
}

/// Build the single-segment egress topic for `correlation_id`, or `None` if
/// it cannot form a clean single segment. Rejects empty, oversized, and any
/// id carrying a `.` (which would over-extend the egress topic) or
/// whitespace / control / wildcard bytes (which would forge or shadow
/// topics). Same charset family the broker's `reply_topic` uses.
fn reply_topic(correlation_id: &str) -> Option<String> {
    if correlation_id.is_empty() || correlation_id.len() > MAX_CORRELATION_LEN {
        return None;
    }
    let clean = correlation_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
    if !clean {
        return None;
    }
    Some(format!("{REPLY_PREFIX}{correlation_id}"))
}

#[cfg(test)]
mod tests {
    fn install_test_profile() {
        crate::profile::install_aos();
    }

    use super::*;

    #[test]
    fn deny_skips_with_rule_reason() {
        install_test_profile();
        let body = verdict_body(&crate::policy::Decision::Deny {
            reason: "no-ssh-write".into(),
        });
        assert_eq!(body.pointer("/skip").and_then(Value::as_bool), Some(true));
        assert_eq!(
            body.pointer("/reason").and_then(Value::as_str),
            Some("no-ssh-write")
        );
    }

    #[test]
    fn allow_does_not_skip_and_never_broadens() {
        install_test_profile();
        let body = verdict_body(&crate::policy::Decision::Allow);
        // skip:false = "no objection", never an assertion of safety.
        assert_eq!(body.pointer("/skip").and_then(Value::as_bool), Some(false));
        assert!(
            body.pointer("/reason").is_none(),
            "allow must not carry a reason"
        );
    }

    #[test]
    fn reply_topic_accepts_uuid_and_rejects_smuggling() {
        install_test_profile();
        assert_eq!(
            reply_topic("0191f3a2b4c74d8e9f01234567890abc").as_deref(),
            Some("hook.v1.response.before_tool_call.0191f3a2b4c74d8e9f01234567890abc")
        );
        assert!(reply_topic("0191f3a2-b4c7-4d8e-9f01-234567890abc").is_some());
        // A `.` would over-extend the egress topic; wildcards / whitespace
        // forge or shadow topics; empty / oversize are rejected.
        assert!(reply_topic("a.b").is_none());
        assert!(reply_topic("a*b").is_none());
        assert!(reply_topic("a b").is_none());
        assert!(reply_topic("").is_none());
        assert!(reply_topic(&"a".repeat(MAX_CORRELATION_LEN + 1)).is_none());
    }

    #[test]
    fn event_without_correlation_id_deserializes_as_observe() {
        install_test_profile();
        // No correlation_id present → the handler treats it as observe-only
        // and never replies. Confirm the shape parses with it absent.
        let event: BeforeToolCallEvent = serde_json::from_value(json!({
            "hook": "before_tool_call",
            "payload": { "tool_name": "Bash", "tool_input": { "command": "ls" } }
        }))
        .unwrap();
        assert!(event.correlation_id.is_none());
        assert_eq!(
            event.payload.pointer("/tool_name").and_then(Value::as_str),
            Some("Bash")
        );
    }

    #[test]
    fn end_to_end_pure_spine_denies_matching_native_tool() {
        install_test_profile();
        // gate_tool_input -> evaluate -> verdict_body, minus host calls.
        let rules = vec![crate::policy::Rule {
            id: "no-rm-rf".into(),
            effect: crate::policy::Effect::Deny,
            tool: "Bash".into(),
            when: vec![crate::policy::ArgMatcher {
                pointer: "/command".into(),
                op: crate::policy::MatchOp::Contains,
                value: "rm -rf".into(),
            }],
        }];
        let payload = json!({ "tool_name": "Bash", "tool_input": { "command": "rm -rf /tmp" } });
        let tool_input = crate::broker::gate_tool_input(&payload);
        let decision = crate::policy::evaluate(
            &rules,
            payload.get("tool_name").and_then(Value::as_str).unwrap(),
            &tool_input,
        );
        assert_eq!(
            verdict_body(&decision)
                .pointer("/skip")
                .and_then(Value::as_bool),
            Some(true)
        );
    }
}
