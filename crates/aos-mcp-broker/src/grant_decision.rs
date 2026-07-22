//! Durable per-principal grant-on-use decisions — the convergence anchor.
//!
//! Grant-on-use consent (the kernel `GrantRequired` -> shim elicit ->
//! `grant.respond` -> kernel grant flow, see [`crate::approval`]) has a race
//! that can wedge a first-run session: the kernel awaiter that raised a
//! `GrantRequired` expires FAIL-CLOSED after 60 s, so a human answer that
//! arrives late — a slow user, or a prompt the client lost and re-asked — can
//! miss its awaiter and never persist the grant. The elicit pipeline can also
//! drop a single accept, and the 120 s pending-dedup then answers every retry
//! with "already pending", so the flow never converges without an operator
//! escape hatch (`aos --principal default agent modify <principal>
//! --add-capsule <id>`).
//!
//! This module makes the user's ACCEPT durable. The moment a `grant.respond`
//! arrives with an approve verb the broker records it here, keyed on the
//! capsule, BEFORE forwarding the decision to the kernel. Then, wherever the
//! broker would surface a fresh grant prompt, it consults the record first: a
//! recorded APPROVE auto-answers the NEXT `GrantRequired` for that capsule
//! (against a fresh, live awaiter) WITHOUT re-prompting, so the flow converges
//! even though the awaiter that originally prompted expired.
//!
//! ## Why a DENY is deliberately NOT recorded
//!
//! The core shim's `resolve_grant` contract is that it ALWAYS responds: it
//! publishes `deny` not only when the user clicks Deny but on EVERY non-accept
//! path — an elicit error, an elicit timeout, and a client that lacks the
//! elicitation capability entirely (that unconditional respond exists to clear
//! the broker's pending marker). The respond body carries no provenance, so the
//! broker cannot distinguish "the user said no" from "the consent machinery
//! failed". Durably recording such a deny would make a transport glitch — or
//! simply using a plain MCP client without elicitation support — a PERMANENT
//! auto-deny the user never chose, until an operator intervenes. So a deny
//! keeps its ephemeral semantics: the pending marker is consumed and the next
//! call re-prompts — deny means "not now", never "never". See
//! [`respond_decision_to_record`], the recording chokepoint that encodes this.
//!
//! The [`GrantDecision::Deny`] variant, its parse path, and the broker's
//! [`GrantAction::AutoDeny`] arm are kept intact as defence-in-depth: if a deny
//! record ever exists (written manually via KV, or by a future version once the
//! respond carries provenance — astrid-runtime/astrid#1114 — so genuine user
//! denies CAN be recorded durably), it is honoured rather than silently
//! ignored.
//!
//! Security posture: this stores only the user's own recorded answer and the
//! broker only ever replays it onto a kernel-correlated `GrantRequired`'s
//! response topic — the kernel remains the sole grantor; nothing here grants a
//! capsule itself. Every read fails toward PROMPTING (a missing / unreadable
//! record is "no decision"), never toward auto-approve.
//!
//! KV is per-principal-scoped by the kernel, so — exactly like the grant-pending
//! marker in [`crate::execute`] — the capsule id alone is the key suffix and the
//! principal is implicit in the storage scope, never stamped into the key.

use astrid_sdk::prelude::*;
use serde_json::{Value, json};

/// KV key prefix recording the user's durable grant decision for a capsule.
const GRANT_DECISION_KEY_PREFIX: &str = "mcp.grant.decision.";

/// The verb persisted for an approve decision (and the wire verb the shim sends).
const GRANT_DECISION_APPROVE: &str = "approve";
/// The verb persisted for a deny decision.
const GRANT_DECISION_DENY: &str = "deny";

/// The user's durable grant decision for a capsule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrantDecision {
    /// The user granted the capsule.
    Approve,
    /// The user denied the capsule.
    Deny,
}

impl GrantDecision {
    /// The KV value byte-string this decision persists as. Round-trips through
    /// [`parse_grant_decision`].
    fn as_kv_value(self) -> &'static str {
        match self {
            GrantDecision::Approve => GRANT_DECISION_APPROVE,
            GrantDecision::Deny => GRANT_DECISION_DENY,
        }
    }
}

/// The broker's action for an observed `GrantRequired`, decided purely from the
/// recorded decision so the choice is unit-testable without a host. See
/// [`grant_action`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrantAction {
    /// A durable approve is on record -> auto-publish an approve for the live
    /// kernel request_id WITHOUT eliciting.
    AutoApprove,
    /// A durable deny is on record -> suppress the prompt and return the
    /// operator-actionable deny message.
    AutoDeny,
    /// No durable decision -> fall through to the pending-dedup + elicit path.
    Prompt,
}

/// KV key recording the user's durable grant decision for `capsule_id`.
///
/// Mirrors `execute::grant_pending_key`: an empty `capsule_id` returns `None` so
/// an unattributed decision can never collapse to the bare prefix and answer for
/// every capsule of the principal.
fn grant_decision_key(capsule_id: &str) -> Option<String> {
    if capsule_id.is_empty() {
        return None;
    }
    Some(format!("{GRANT_DECISION_KEY_PREFIX}{capsule_id}"))
}

/// Parse a recorded grant-decision KV value into a [`GrantDecision`].
///
/// Pure (no host) so it is unit-testable. Anything that is not exactly one of
/// the two recognised verbs — garbage, empty, a legacy presence marker, a
/// truncated write, invalid UTF-8 — yields `None`, which the read path
/// ([`recorded_grant_decision`]) surfaces as "no decision" so the broker PROMPTS
/// rather than acting on an unreadable record. Fail toward prompting, never
/// toward auto-approve.
fn parse_grant_decision(value: &[u8]) -> Option<GrantDecision> {
    match std::str::from_utf8(value).map(str::trim).unwrap_or("") {
        GRANT_DECISION_APPROVE => Some(GrantDecision::Approve),
        GRANT_DECISION_DENY => Some(GrantDecision::Deny),
        _ => None,
    }
}

/// What (if anything) a `grant.respond` should durably record, given whether it
/// carried an approve verb.
///
/// This is THE recording chokepoint, pure so it is unit-testable without a
/// host: an approve records [`GrantDecision::Approve`]; a deny records NOTHING.
/// The shim publishes `deny` on every non-accept path — user decline, elicit
/// error, elicit timeout, or a client with no elicitation support — with no
/// provenance to tell them apart, so recording a deny durably would turn a
/// machinery failure into a permanent auto-deny the user never chose (see the
/// module doc). A deny therefore stays ephemeral ("not now"): the pending
/// marker is consumed by the respond handler and the next call re-prompts.
pub(crate) fn respond_decision_to_record(granted: bool) -> Option<GrantDecision> {
    if granted {
        Some(GrantDecision::Approve)
    } else {
        None
    }
}

/// Record the user's durable grant decision for `capsule_id`.
///
/// Called by [`crate::approval::handle_mcp_grant_respond`] — via the
/// [`respond_decision_to_record`] chokepoint, so in practice only ever with
/// [`GrantDecision::Approve`] — BEFORE the kernel decision is published, so the
/// record survives even when the kernel awaiter that prompted the grant has
/// already expired fail-closed: the next call converges from the record.
/// Accepts [`GrantDecision::Deny`] for the defence-in-depth read path (module
/// doc), but no current caller passes it. Best-effort: a write failure is
/// logged, not fatal, and only costs a re-prompt on the next call (fail toward
/// prompting). An empty `capsule_id` records nothing.
pub(crate) fn record_grant_decision(capsule_id: &str, decision: GrantDecision) {
    let Some(key) = grant_decision_key(capsule_id) else {
        log::warn(format!(
            "{}: empty capsule_id; not recording a grant decision",
            crate::profile::log_tag()
        ));
        return;
    };
    if let Err(e) = kv::set_bytes(&key, decision.as_kv_value().as_bytes()) {
        log::warn(format!(
            "{}: failed to record grant decision for capsule_id '{capsule_id}': {e}",
            crate::profile::log_tag()
        ));
    }
}

/// Read the durable recorded grant decision for `capsule_id`, or `None` if the
/// user has not decided (or the record cannot be read / parsed).
///
/// Fail toward prompting: a missing key, a KV read error, or an unparseable
/// value all return `None` so the broker surfaces a fresh consent prompt. It
/// NEVER returns `Approve` on a read it is unsure about — an auto-approve must
/// only ever follow a record the user actually created.
pub(crate) fn recorded_grant_decision(capsule_id: &str) -> Option<GrantDecision> {
    let key = grant_decision_key(capsule_id)?;
    match kv::get_bytes_opt(&key) {
        Ok(Some(bytes)) => parse_grant_decision(&bytes),
        Ok(None) => None,
        Err(e) => {
            log::warn(format!(
                "{}: grant decision read error for capsule_id '{capsule_id}', \
                 will prompt: {e}",
                crate::profile::log_tag()
            ));
            None
        }
    }
}

/// Map a recorded-decision read to the broker's [`GrantAction`].
///
/// Pure — the KV read happens in [`recorded_grant_decision`]; this is the
/// testable decision spine (a recorded approve auto-responds, a recorded deny
/// suppresses, no record prompts).
pub(crate) fn grant_action(decision: Option<GrantDecision>) -> GrantAction {
    match decision {
        Some(GrantDecision::Approve) => GrantAction::AutoApprove,
        Some(GrantDecision::Deny) => GrantAction::AutoDeny,
        None => GrantAction::Prompt,
    }
}

// ----------------------------------------------------------------------------
// Terminal `tool.call` reply shaping for the grant-on-use outcomes.
//
// Pure (the only host touch is `crate::broker::mcp_content`, itself pure) so the
// user-facing text and the `isError` contract are unit-testable without a live
// bus. The broker arm calls these; keeping them here keeps the grant-on-use
// presentation with its decision logic and keeps `broker.rs` under the size cap.
// ----------------------------------------------------------------------------

/// Terminal reply for the grant-pending DEDUP: a consent prompt is already open
/// for this capsule, so instead of stacking a duplicate elicit we return an
/// actionable message. `isError:false` — a prompt is in flight, not a failure.
///
/// The improved text replaces the old opaque "a capsule-access grant prompt is
/// already pending for this capsule": it now tells the user exactly what to do,
/// and that a lost prompt is re-asked after the pending TTL. The wait hint is
/// DERIVED from [`crate::execute::GRANT_PENDING_TTL_MS`] (never hard-coded) so
/// the guidance cannot drift if the TTL is retuned.
pub(crate) fn grant_dedup_reply(req_id: &str) -> Value {
    grant_reply(
        req_id,
        format!(
            "{}: a consent prompt for this capsule is open in your client. Answer \
             it and retry; if it never appeared, retry after {} and it will be re-asked.",
            crate::profile::log_tag(),
            ttl_hint(crate::execute::GRANT_PENDING_TTL_MS)
        ),
        false,
    )
}

/// Render a TTL in milliseconds as the human wait hint the dedup reply carries:
/// whole minutes when the TTL divides evenly into them, else whole seconds
/// (rounded UP, so the hint never advises retrying before the marker can have
/// expired). Pure, so the formatting is unit-testable and the reply text stays
/// pinned to the real constant.
fn ttl_hint(ttl_ms: u64) -> String {
    const MS_PER_MINUTE: u64 = 60_000;
    if ttl_ms > 0 && ttl_ms.is_multiple_of(MS_PER_MINUTE) {
        let minutes = ttl_ms / MS_PER_MINUTE;
        if minutes == 1 {
            "~1 minute".to_string()
        } else {
            format!("~{minutes} minutes")
        }
    } else {
        format!("~{} seconds", ttl_ms.div_ceil(1_000))
    }
}

/// Terminal reply after a DURABLE recorded APPROVE auto-answered the kernel grant
/// gate: the grant is applied against a fresh, live awaiter, so the re-sent call
/// converges. `isError:false` — access was granted, not a failure.
pub(crate) fn grant_auto_approve_reply(req_id: &str, capsule_id: &str) -> Value {
    grant_reply(
        req_id,
        format!(
            "{}: session access to capsule '{capsule_id}' is already approved for \
             this identity; the grant has been applied. Retry the tool call.",
            crate::profile::log_tag()
        ),
        false,
    )
}

/// Terminal reply after a DURABLE recorded DENY suppressed the grant prompt: the
/// user denied this capsule, so the call cannot proceed. `isError:true`, with the
/// operator escape hatch.
pub(crate) fn grant_auto_deny_reply(req_id: &str, capsule_id: &str, principal: &str) -> Value {
    let who = if principal.is_empty() {
        "<principal>"
    } else {
        principal
    };
    grant_reply(
        req_id,
        format!(
            "{}: session access to capsule '{capsule_id}' was denied for this \
             identity. An operator can allow it with: aos --principal default agent modify {who} \
             --add-capsule {capsule_id} (or later revoke a grant with --remove-capsule).",
            crate::profile::log_tag()
        ),
        true,
    )
}

/// Terminal reply when a recorded approve could NOT be applied because the kernel
/// request_id was unroutable. Defensive: a kernel-minted UUID is always routable,
/// but if that ever changed we must NOT return the retry-hint reply (the caller
/// would spin retrying a grant that can never publish) — `isError:true` breaks
/// the loop and points at the operator escape hatch.
pub(crate) fn grant_unroutable_reply(req_id: &str, capsule_id: &str) -> Value {
    grant_reply(
        req_id,
        format!(
            "{}: could not apply the recorded grant for capsule '{capsule_id}' \
             (unroutable kernel request id). Re-grant it, or an operator can run: \
             aos --principal default agent modify <principal> --add-capsule {capsule_id}.",
            crate::profile::log_tag()
        ),
        true,
    )
}

/// Shape a terminal `tool.call` reply with the broker's standard content
/// encoding, so every grant-outcome reply agrees on the envelope.
fn grant_reply(req_id: &str, message: String, is_error: bool) -> Value {
    json!({
        "kind": "tool.call",
        "req_id": req_id,
        "content": crate::broker::mcp_content(Value::String(message)),
        "isError": is_error,
    })
}

#[cfg(test)]
mod tests {
    fn install_test_profile() {
        crate::profile::install_aos();
    }

    use super::*;

    #[test]
    fn grant_decision_key_applies_prefix_on_capsule_only() {
        install_test_profile();
        // KV is per-principal-scoped, so the suffix is the capsule id alone; the
        // principal is implicit in the storage scope, mirroring grant_pending_key.
        assert_eq!(
            grant_decision_key("fs").as_deref(),
            Some("mcp.grant.decision.fs")
        );
    }

    #[test]
    fn grant_decision_key_rejects_empty_capsule() {
        install_test_profile();
        // An empty capsule id must never resolve to a routable key, or one record
        // would answer for every capsule of the principal.
        assert_eq!(grant_decision_key(""), None);
    }

    #[test]
    fn parse_grant_decision_reads_recognised_verbs() {
        install_test_profile();
        assert_eq!(
            parse_grant_decision(b"approve"),
            Some(GrantDecision::Approve)
        );
        assert_eq!(parse_grant_decision(b"deny"), Some(GrantDecision::Deny));
        // Whitespace-tolerant, mirroring the marker parser.
        assert_eq!(
            parse_grant_decision(b"  approve\n"),
            Some(GrantDecision::Approve)
        );
    }

    #[test]
    fn parse_grant_decision_unknown_is_none_fail_toward_prompt() {
        install_test_profile();
        // Garbage, empty, a legacy presence marker, a partial write, or invalid
        // UTF-8 all read as "no decision" so the broker prompts rather than
        // acting on an unreadable record — never silently auto-approving.
        assert_eq!(parse_grant_decision(b""), None);
        assert_eq!(parse_grant_decision(b"1"), None);
        assert_eq!(parse_grant_decision(b"approved"), None);
        assert_eq!(parse_grant_decision(b"yes"), None);
        assert_eq!(parse_grant_decision(&[0xff, 0xfe]), None);
    }

    #[test]
    fn grant_decision_kv_value_roundtrips() {
        install_test_profile();
        // The value written is exactly what parse reads back.
        assert_eq!(
            parse_grant_decision(GrantDecision::Approve.as_kv_value().as_bytes()),
            Some(GrantDecision::Approve)
        );
        assert_eq!(
            parse_grant_decision(GrantDecision::Deny.as_kv_value().as_bytes()),
            Some(GrantDecision::Deny)
        );
    }

    #[test]
    fn grant_action_maps_recorded_decision() {
        install_test_profile();
        // The crux decision spine: a recorded approve auto-responds (respond, not
        // elicit); a recorded deny suppresses the prompt (defence-in-depth — no
        // current respond path writes one, see respond_records_approve_only);
        // no record prompts.
        assert_eq!(
            grant_action(Some(GrantDecision::Approve)),
            GrantAction::AutoApprove
        );
        assert_eq!(
            grant_action(Some(GrantDecision::Deny)),
            GrantAction::AutoDeny
        );
        assert_eq!(grant_action(None), GrantAction::Prompt);
    }

    #[test]
    fn respond_records_approve_only_never_deny() {
        install_test_profile();
        // REGRESSION: the shim's resolve_grant publishes `deny` on EVERY
        // non-accept path — user decline, elicit error, elicit timeout, or a
        // client with no elicitation capability — and the respond body carries
        // no provenance to tell them apart. A deny must therefore record
        // NOTHING durable (ephemeral "not now"; the pending marker clears and
        // the next call re-prompts), or a timed-out dialog / plain MCP client
        // would permanently auto-deny a capsule the user never answered on.
        // Only an approve is durably recorded.
        assert_eq!(
            respond_decision_to_record(true),
            Some(GrantDecision::Approve)
        );
        assert_eq!(respond_decision_to_record(false), None);
    }

    #[test]
    fn grant_action_composes_with_parse() {
        install_test_profile();
        // End to end over the pure spine (parse -> action) as the broker runs it,
        // minus the host KV read: a stored "approve" chooses AutoApprove
        // (respond), a stored "deny" chooses AutoDeny (suppress), an unreadable
        // value prompts.
        assert_eq!(
            grant_action(parse_grant_decision(b"approve")),
            GrantAction::AutoApprove
        );
        assert_eq!(
            grant_action(parse_grant_decision(b"deny")),
            GrantAction::AutoDeny
        );
        assert_eq!(
            grant_action(parse_grant_decision(b"garbage")),
            GrantAction::Prompt
        );
    }

    /// Pull the single text content block out of a grant-outcome reply.
    fn reply_text(reply: &Value) -> String {
        reply
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .expect("grant reply must carry one text content block")
            .to_string()
    }

    #[test]
    fn grant_dedup_reply_is_actionable_not_an_error() {
        install_test_profile();
        let reply = grant_dedup_reply("req-d");
        // A prompt in flight is not a failure — isError must stay false so the
        // client treats it as a normal (retriable) result.
        assert_eq!(
            reply.pointer("/isError").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            reply.pointer("/req_id").and_then(Value::as_str),
            Some("req-d")
        );
        // The improved D text: tell the user what to do, and that a lost prompt
        // is re-asked after the TTL — not the old opaque "already pending".
        let text = reply_text(&reply);
        assert!(text.contains("open in your client"), "text was {text:?}");
        assert!(text.contains("retry"), "text was {text:?}");
        assert!(text.contains("re-asked"), "text was {text:?}");
        assert!(
            !text.contains("already pending"),
            "must not keep the old opaque text: {text:?}"
        );
    }

    #[test]
    fn grant_dedup_reply_wait_hint_derives_from_pending_ttl() {
        install_test_profile();
        // The wait hint must be DERIVED from the real TTL constant, never a
        // re-hard-coded literal: this asserts the derived rendering of
        // GRANT_PENDING_TTL_MS appears, so it fails if someone changes the
        // constant while the reply text stays behind (or inlines a literal).
        let expected = ttl_hint(crate::execute::GRANT_PENDING_TTL_MS);
        let text = reply_text(&grant_dedup_reply("req-d"));
        assert!(
            text.contains(&format!("retry after {expected} ")),
            "hint {expected:?} missing from {text:?}"
        );
    }

    #[test]
    fn ttl_hint_renders_minutes_or_ceil_seconds() {
        install_test_profile();
        // Whole minutes when the TTL divides evenly (the current 120s TTL reads
        // "~2 minutes"), singular at exactly one minute, else whole seconds
        // rounded UP so the hint never advises retrying before the marker can
        // have expired. Zero degrades to seconds, not "0 minutes".
        assert_eq!(ttl_hint(120_000), "~2 minutes");
        assert_eq!(ttl_hint(60_000), "~1 minute");
        assert_eq!(ttl_hint(90_000), "~90 seconds");
        assert_eq!(ttl_hint(90_500), "~91 seconds");
        assert_eq!(ttl_hint(0), "~0 seconds");
    }

    #[test]
    fn grant_auto_approve_reply_names_capsule_and_asks_retry() {
        install_test_profile();
        let reply = grant_auto_approve_reply("req-a", "fs");
        // Access granted -> not an error; a retry converges.
        assert_eq!(
            reply.pointer("/isError").and_then(Value::as_bool),
            Some(false)
        );
        let text = reply_text(&reply);
        assert!(text.contains("'fs'"), "text was {text:?}");
        assert!(text.contains("already approved"), "text was {text:?}");
        assert!(text.contains("Retry"), "text was {text:?}");
    }

    #[test]
    fn grant_auto_deny_reply_is_error_with_operator_path() {
        install_test_profile();
        let reply = grant_auto_deny_reply("req-x", "system", "claude-code");
        // A denied capsule is a hard failure for the call.
        assert_eq!(
            reply.pointer("/isError").and_then(Value::as_bool),
            Some(true)
        );
        let text = reply_text(&reply);
        assert!(text.contains("'system'"), "text was {text:?}");
        assert!(text.contains("denied"), "text was {text:?}");
        // The actionable operator escape hatch, with the resolved principal.
        assert!(
            text.contains("aos --principal default agent modify claude-code --add-capsule system"),
            "text was {text:?}"
        );
    }

    #[test]
    fn grant_auto_deny_reply_placeholder_principal_when_unknown() {
        install_test_profile();
        // An empty principal degrades to a readable placeholder, never a blank
        // command the operator cannot run.
        let reply = grant_auto_deny_reply("req-x", "system", "");
        let text = reply_text(&reply);
        assert!(
            text.contains("aos --principal default agent modify <principal> --add-capsule system"),
            "text was {text:?}"
        );
    }

    #[test]
    fn grant_unroutable_reply_is_error_breaks_retry_loop() {
        install_test_profile();
        // The defensive path must be an error, not the auto-approve retry-hint,
        // or the caller would spin retrying a grant that can never publish.
        let reply = grant_unroutable_reply("req-u", "fs");
        assert_eq!(
            reply.pointer("/isError").and_then(Value::as_bool),
            Some(true)
        );
        assert!(reply_text(&reply).contains("'fs'"));
    }
}
