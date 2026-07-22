//! Broker-side elicitation/approval bridge.
//!
//! ROLE: this is the **Claude-front-door renderer** of Astrid's existing
//! runtime approval flow — NOT a new elicitation mechanism or channel. The
//! consent prompt is *originated* by the host `astrid:approval` syscall
//! (`request_approval`, ungated at runtime); the TUI renders it inline; this
//! bridge renders the same `astrid.v1.approval` envelope into Claude's UI via
//! the shim's `ctx.peer.elicit`. It invents no origination primitive, reuses
//! the host's response topic + verb set verbatim, and never round-trips a
//! secret. Runtime user-prompting already exists in Astrid (`request_approval`
//! for consent, `SelectionRequired` for choice); see the runtime-value-elicit
//! RFC for the one gap these do NOT cover (typed value / out-of-band secret).
//!
//! When a broker `tool.call` ([`crate::broker::handle_mcp_call`]) routes a
//! capability-gated tool, the tool's host-side `request_approval` syscall
//! publishes an `astrid.v1.approval` envelope and BLOCKS the tool's WASM
//! thread until a decision lands on
//! `astrid.v1.approval.response.<request-id>`. The broker can't call the
//! host `astrid:elicit` syscall (it is hard-gated to install/upgrade), so
//! it relays the bus-level envelopes instead: it surfaces an
//! `approval-required` flag in the `tool.call` reply so the shim can elicit
//! the choice from Claude, then maps the returned choice back onto the
//! `astrid.v1.approval.response.<request-id>` decision topic to unblock the
//! tool.
//!
//! ## Topic contract (meets the core elicit-shim slice)
//!
//! * **inbound (observed during dispatch)** `astrid.v1.approval` —
//!   `IpcPayload::ApprovalRequired` = `{ type:"approval_required",
//!   request_id, action, resource, reason }`. Published by the host
//!   `request_approval` path ([`core` `host/approval.rs`]).
//! * **inbound (shim's elicited choice)**
//!   `astrid.v1.request.mcp.approval.respond` -> [`handle_mcp_approval`] —
//!   `{ req_id, request_id, decision, tool_name, call_id }`. The shim
//!   collected the choice from Claude and forwards it through the same broker
//!   front door it uses for tool calls; `req_id` is the proxy correlation
//!   token (echoed into the reply), `request_id` is the approval correlation
//!   id from the `approval_required` envelope, and `tool_name` / `call_id`
//!   are echoed back from the `approval_required` reply flag so this handler
//!   can re-establish the result drain the original dispatch dropped.
//! * **outbound (decision → unblock tool)**
//!   `astrid.v1.approval.response.<request_id>` —
//!   `IpcPayload::ApprovalResponse` = `{ type:"approval_response",
//!   request_id, decision, reason? }`. The blocked host `request_approval`
//!   wakes on this and returns the typed decision to the tool.
//! * **outbound (resumed result → shim)** `astrid.v1.response.<req_id>` —
//!   `{ kind:"tool.call", req_id, content:[...], isError:bool }`. After
//!   publishing the decision this handler re-subscribes to the tool's result
//!   topic and drains the resumed (approve) or `isError` (deny) result,
//!   delivering it to the shim as a terminal `tool.call` reply — the same
//!   shape [`crate::broker::handle_mcp_call`] would have produced had the
//!   tool not parked on approval. THIS is the leg that completes the
//!   round-trip: the original dispatch could not keep its result
//!   subscription alive across the synchronous interceptor return, so
//!   ownership of result delivery moves here.
//!
//! ## Why the result drain lives here, not in the original dispatch
//!
//! A WASM interceptor is synchronous and returns exactly once. When
//! [`crate::execute::dispatch_with_approval`] observes the `approval_required`
//! envelope it must return so the broker can surface the elicit flag — at
//! which point its `tool.v1.execute.<name>.result` subscription is dropped.
//! The tool only publishes its result AFTER a decision lands, so by the time
//! the result exists no subscriber remains. This handler therefore re-opens
//! that subscription (subscribe BEFORE publishing the decision to avoid the
//! resume race) and owns delivering the single terminal result back to the
//! shim. The `tool_name` + `call_id` it needs are echoed by the shim from
//! the reply flag.
//!
//! ## Concurrency / correlation
//!
//! `astrid.v1.approval` is a single global broadcast topic and the host
//! envelope carries NO `call_id` / `tool_name` — only an opaque host-minted
//! `request_id`. The concern is whether one tool's approval could be surfaced
//! to a *different* tool's `tool.call` reply.
//!
//! This is prevented at the engine level: the kernel serialises guest calls
//! per capsule instance behind the store mutex
//! (`core` `engine/wasm/mod.rs`: *"The mutex still serialises one guest call
//! at a time per capsule"*). A broker dispatch
//! ([`crate::execute::dispatch_with_approval`]) holds that lock for the whole
//! synchronous drain and returns the instant it observes ANY approval, so a
//! second aos-mcp `handle_mcp_call` cannot run — and cannot be watching the
//! approval topic — while another is in flight. There is therefore at most
//! ONE aos-mcp approval-watcher at a time: the only `astrid.v1.approval`
//! envelope it can observe during its window is the one ITS OWN routed tool
//! raised (it published that tool's execute request and nothing else of
//! aos-mcp's is running). Intra-capsule cross-talk is structurally
//! impossible — no claim registry is needed.
//!
//! The decision is independently routed correctly regardless: it targets
//! `astrid.v1.approval.response.<request_id>`, the exact per-request topic the
//! blocked host `request_approval` subscribed to, so the shim round-trip
//! carrying a given `request_id` unblocks exactly the tool that raised it.
//!
//! Residual (documented, not fixed here): a DIFFERENT capsule's tool that
//! calls `request_approval` during this dispatch's drain window also lands on
//! the global topic, and nothing on the wire distinguishes it from this
//! tool's approval — the host stamps a nil source and omits any `call_id` /
//! `tool_name`. Attributing such a foreign approval correctly needs a kernel
//! correlation field on `IpcPayload::ApprovalRequired`; that is a kernel
//! contract change, out of scope for this capsule-only slice.
//!
//! ## Decision surface — approvals/confirms/selects ONLY
//!
//! The decision string is constrained to the host's recognised approval
//! verbs ([`MAP` in core `host/approval.rs::decision_from_str`]):
//! `approve`, `approve_session`, `approve_always`, `deny`. Anything else
//! (including unknown / empty) is normalised to `deny` — fail secure. A
//! `deny` decision results in the tool call returning `isError` upstream
//! (the host returns `ApprovalDecision::Denied`, the gated tool publishes
//! an error result, and the broker's drain reshapes it). **This bridge
//! NEVER relays a secret through elicitation** — it carries only the
//! action/resource/reason display fields (already host-sanitized) and a
//! constrained decision verb. No free-form text is round-tripped into the
//! tool.

use astrid_sdk::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};

/// Bus topic the host publishes capability-approval requests on. A single
/// fixed (segment-arity-stable) topic — the correlation id lives in the
/// body (`request_id`), not the topic, so a wildcard suffix is not needed
/// to observe it.
pub(crate) const APPROVAL_REQUEST_TOPIC: &str = "astrid.v1.approval";

/// Egress topic PREFIX for the decision. The host's blocked
/// `request_approval` subscribed to `astrid.v1.approval.response.<id>`
/// before publishing the request, so the decision lands on the exact
/// per-request topic. `<id>` is charset-gated by [`response_topic`].
const APPROVAL_RESPONSE_PREFIX: &str = "astrid.v1.approval.response.";

/// `request_id` charset cap. The approval correlation id is a host-minted
/// UUID; anything longer is rejected before it can be stamped into an
/// egress topic.
const MAX_REQUEST_ID_LEN: usize = 128;

/// Display-field cap mirrored from the host sanitizer
/// (`MAX_RESOURCE_LEN`). The host already trims/strips/truncates these
/// before publishing `astrid.v1.approval`; we re-cap defensively so a
/// future producer that skips sanitization cannot inflate the reply body
/// the shim renders to Claude.
const MAX_DISPLAY_LEN: usize = 1024;

/// The four approval verbs the host's `decision_from_str` recognises.
/// `approve` / `approve_session` / `approve_always` grant; everything
/// else (including `deny`) is treated as a deny by the host. We normalise
/// the shim's choice to exactly one of these so no free-form string is
/// ever round-tripped onto the decision topic.
const APPROVE: &str = "approve";
const APPROVE_SESSION: &str = "approve_session";
const APPROVE_ALWAYS: &str = "approve_always";
const DENY: &str = "deny";

/// Bound on the post-decision result drain in [`handle_mcp_approval`].
/// The tool resumes (or denies) the instant the decision lands; its result
/// is one bus hop away, so this only needs to cover scheduling latency, not
/// a fresh tool runtime. Kept well under the host's 60 s approval window and
/// the proxy's own request deadline so the shim gets a terminal reply
/// promptly rather than hanging.
const RESUME_TIMEOUT_MS: u64 = 50_000;

/// Poll slice for the resume drain. Mirrors the execute-path slice cadence
/// so a result published the instant the tool resumes is picked up within
/// one short slice rather than blocking the whole budget on the first
/// `recv`.
const RESUME_SLICE_MS: u64 = 250;

/// Parsed `astrid.v1.approval` envelope (`IpcPayload::ApprovalRequired`).
///
/// Only the display + correlation fields are deserialized. The `type`
/// discriminator tag is ignored by the struct-deserialize (serde drops
/// unknown fields), and `is_approval_required` gates on it explicitly so
/// a sibling `IpcPayload` variant published on the shared topic is not
/// mistaken for an approval.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ApprovalRequired {
    /// Host-minted correlation id; echoed onto the response topic suffix.
    pub(crate) request_id: String,
    /// The action being requested (e.g. `"git push"`). Display only.
    #[serde(default)]
    pub(crate) action: String,
    /// The resource target (e.g. full command string). Display only.
    #[serde(default)]
    pub(crate) resource: String,
    /// Justification shown to the user. Display only.
    #[serde(default)]
    pub(crate) reason: String,
}

impl ApprovalRequired {
    /// Shape the `approval_required` flag embedded in the broker
    /// `tool.call` reply.
    ///
    /// Carries the host-sanitized display fields plus the correlation id
    /// (never any tool argument or secret), AND the dispatch's `tool_name` +
    /// `call_id`. The latter two are NOT secrets — they are the same routing
    /// tokens the shim already used to make the `tool.call` — and the shim
    /// MUST echo them back on `astrid.v1.request.mcp.approval.respond` so
    /// [`handle_mcp_approval`] can re-subscribe to the tool's result topic
    /// and deliver the resumed/denied outcome the original dispatch could not
    /// keep a subscription alive for.
    pub(crate) fn to_reply_flag(&self, tool_name: &str, call_id: &str) -> Value {
        json!({
            "request_id": self.request_id,
            "action": clamp(&self.action),
            "resource": clamp(&self.resource),
            "reason": clamp(&self.reason),
            "tool_name": tool_name,
            "call_id": call_id,
        })
    }
}

/// Decide whether a JSON value parsed from the `astrid.v1.approval` topic
/// is actually an `ApprovalRequired` envelope.
///
/// The `IpcPayload` enum is `#[serde(tag = "type", rename_all =
/// "snake_case")]`, so a genuine approval carries `"type":
/// "approval_required"`. Gate on the tag AND a non-empty `request_id` (a
/// blank id can't be routed back). Other payloads sharing the topic are
/// skipped.
pub(crate) fn is_approval_required(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("approval_required")
        && value
            .get("request_id")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty())
}

/// Parsed `astrid.v1.approval` grant-gate-miss envelope
/// (`IpcPayload::GrantRequired`, published by the kernel #1001 producer when
/// the principal has not granted the target capsule).
///
/// `grant-on-use` mirrors the INGRESS flow (gate → consent → re-send), NOT
/// capability-approval (park → resume): the kernel DROPPED the original
/// `tool.call` at the access gate, so there is nothing parked and nothing to
/// drain. The kernel grants the capsule when an APPROVE `approval_response`
/// lands on `astrid.v1.approval.response.<request_id>` — the exact same
/// response topic + envelope the approval flow uses, reused verbatim.
///
/// Only the correlation + display fields are deserialized. The `type`
/// discriminator tag is dropped by struct-deserialize; [`is_grant_required`]
/// gates on it explicitly so a sibling `IpcPayload` variant on the shared topic
/// is not mistaken for a grant request.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GrantRequired {
    /// Host-minted correlation id; echoed onto the response topic suffix
    /// (`astrid.v1.approval.response.<request_id>`) so the kernel grant handler
    /// resumes the exact gate miss.
    pub(crate) request_id: String,
    /// The principal the grant would be recorded for. Display only — the
    /// kernel resolves the real principal from the approve it consumes; this is
    /// never trusted as an identity.
    #[serde(default)]
    pub(crate) principal: String,
    /// The capsule id the access gate refused. Carried to the shim for display
    /// and used as the dedup key `(principal, capsule_id)`.
    #[serde(default)]
    pub(crate) capsule_id: String,
}

impl GrantRequired {
    /// Shape the `grant_required` flag embedded in the broker `tool.call`
    /// reply.
    ///
    /// Carries the correlation id, the (display-only) principal, the capsule id
    /// the gate refused, plus the dispatch's `tool_name` + `call_id` — the same
    /// routing tokens the shim already used for the `tool.call`, so it can
    /// RE-SEND the original call on approve (grant-on-use re-sends; it does not
    /// resume). No tool argument or secret is carried.
    pub(crate) fn to_reply_flag(&self, tool_name: &str, call_id: &str) -> Value {
        json!({
            "request_id": self.request_id,
            "capsule_id": clamp(&self.capsule_id),
            "principal": clamp(&self.principal),
            "tool_name": tool_name,
            "call_id": call_id,
        })
    }
}

/// Decide whether a JSON value parsed from the `astrid.v1.approval` topic is a
/// `GrantRequired` envelope.
///
/// The `IpcPayload` enum is `#[serde(tag = "type", rename_all = "snake_case")]`,
/// so a genuine grant request carries `"type": "grant_required"`. Gate on the
/// tag AND a non-empty `request_id` (a blank id can't be routed back). Other
/// payloads sharing the topic — including the sibling `approval_required` — are
/// skipped.
pub(crate) fn is_grant_required(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("grant_required")
        && value
            .get("request_id")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty())
}

/// Inbound `astrid.v1.request.mcp.approval.respond` payload — the shim's
/// elicited choice for an outstanding approval.
///
/// `req_id` is the proxy correlation token (echoed into the terminal reply);
/// `request_id` is the approval correlation id from the `approval_required`
/// envelope; `decision` is the shim's choice, normalised to a host verb by
/// [`normalize_decision`]. `tool_name` + `call_id` are echoed back from the
/// reply flag and identify which `tool.v1.execute.<name>.result` to drain for
/// the resumed/denied outcome. An optional `reason` is carried as a display /
/// audit string only — it is NOT a tool argument and never substitutes into
/// the tool call.
#[derive(Debug, Deserialize)]
struct ApprovalRespond {
    req_id: String,
    request_id: String,
    decision: String,
    tool_name: String,
    call_id: String,
    #[serde(default)]
    reason: Option<String>,
}

/// Handle `astrid.v1.request.mcp.approval.respond`.
///
/// Maps the shim's elicited choice onto an
/// `astrid.v1.approval.response.<request_id>` decision so the blocked host
/// `request_approval` resumes, then DRAINS the resumed (approve) or `isError`
/// (deny) tool result and delivers it to the shim on
/// `astrid.v1.response.<req_id>` as a terminal `tool.call` reply — completing
/// the round-trip the original [`crate::execute::dispatch_with_approval`]
/// could not (its result subscription died when the interceptor returned).
///
/// Ordering is load-bearing: we subscribe to the tool's result topic BEFORE
/// publishing the decision, otherwise the tool could resume and publish its
/// result before we are listening.
///
/// State-mutating (it can grant a capability), so it is confused-deputy
/// gated identically to [`crate::broker::handle_mcp_call`]: the inbound
/// message's kernel-set `source_id` must already be a trusted ingress
/// ([`crate::execute::is_ingress_trusted`]) before any decision is published.
/// A rejected or malformed request publishes a `deny` so an outstanding tool
/// can never hang waiting on a decision that will not come — fail secure —
/// and (when routable) delivers the resulting `isError` reply to the shim.
pub(crate) fn handle_mcp_approval(payload: Value) -> Result<(), SysError> {
    let req: ApprovalRespond = match serde_json::from_value(payload) {
        Ok(v) => v,
        Err(e) => {
            // No recoverable req_id — there is no channel to reply on, and no
            // request_id to deny. The host's approval times out (60 s) on
            // its own schedule. Log and drop.
            log::warn(format!(
                "{}: broker approval.respond: malformed payload: {e}",
                crate::profile::log_tag()
            ));
            return Ok(());
        }
    };

    log::info(format!(
        "{}: broker ingress method=approval.respond req_id={} request_id={} tool={}",
        crate::profile::log_tag(),
        req.req_id,
        req.request_id,
        req.tool_name
    ));

    let reply_topic = crate::broker::reply_topic(&req.req_id);
    let Some(response_topic) = response_topic(&req.request_id) else {
        log::warn(format!(
            "{}: broker approval.respond: rejecting unroutable request_id '{}'",
            crate::profile::log_tag(),
            req.request_id
        ));
        // Reply to the shim if we at least have a clean req_id so it doesn't
        // hang; the decision was never published (bad request_id).
        if let Some(reply) = &reply_topic {
            deliver_error(reply, &req.req_id, "approval request_id was not routable");
        }
        return Ok(());
    };

    // Confused-deputy gate. Granting a capability is the most sensitive
    // action this capsule performs — require the kernel-set `source_id`
    // (NOT a body field) to be in the operator-pinned trusted-ingress
    // allow-set. On any failure (no caller context, untrusted ingress) we
    // publish a `deny` decision so the blocked tool retires cleanly, then
    // deliver the resulting denied outcome to the shim.
    let source_id = match runtime::caller() {
        Ok(ctx) => ctx.source_id,
        Err(e) => {
            log::warn(format!(
                "{}: broker approval.respond: no caller context, denying request_id '{}': {e}",
                crate::profile::log_tag(),
                req.request_id
            ));
            resolve_with_decision(&req, &response_topic, DENY, None);
            return Ok(());
        }
    };
    if !crate::execute::is_ingress_trusted(&source_id) {
        log::warn(format!(
            "{}: broker approval.respond: untrusted ingress source_id '{source_id}', \
             denying request_id '{}'",
            crate::profile::log_tag(),
            req.request_id
        ));
        resolve_with_decision(&req, &response_topic, DENY, None);
        return Ok(());
    }

    // Normalise the shim's choice to exactly one host verb. Unknown /
    // empty collapses to `deny` — fail secure. The optional `reason` is a
    // display/audit string only; it is forwarded verbatim into the
    // `ApprovalResponse.reason` field and is NEVER treated as a tool
    // argument (the host's approval path discards it after logging).
    let decision = normalize_decision(&req.decision);
    resolve_with_decision(&req, &response_topic, decision, req.reason.as_deref());
    Ok(())
}

/// Inbound `astrid.v1.request.mcp.ingress.respond` payload — the user's
/// consent decision for an untrusted ingress, collected by the shim's
/// elicit prompt.
///
/// `req_id` is the proxy correlation token (the ack lands on
/// `astrid.v1.response.<req_id>`); `accept` is the user's decision. There is
/// DELIBERATELY no `source_id` field — the ingress to trust is the
/// kernel-stamped `runtime::caller().source_id` of whoever sent THIS message
/// (the cli proxy), never a value the body could forge. Any other fields are
/// ignored.
#[derive(Debug, Deserialize)]
struct IngressRespond {
    req_id: String,
    accept: bool,
}

/// Inbound `astrid.v1.request.mcp.grant.respond` payload — the shim's elicited
/// Grant/Deny decision for an outstanding capsule-grant prompt.
///
/// `req_id` is the proxy correlation token (the ack lands on
/// `astrid.v1.response.<req_id>`); `request_id` is the kernel grant correlation
/// id from the `grant_required` envelope (echoed onto
/// `astrid.v1.approval.response.<request_id>`); `decision` is the user's choice,
/// normalised to a host verb by [`normalize_decision`]. `capsule_id` is the
/// grant target, used to clear the dedup marker on respond. There is
/// DELIBERATELY no `tool_name` / `call_id`: grant-on-use does NOT drain a result
/// (the kernel dropped the call), so there is no result topic to re-establish —
/// unlike [`ApprovalRespond`]. Any other fields are ignored.
#[derive(Debug, Deserialize)]
struct GrantRespond {
    req_id: String,
    request_id: String,
    decision: String,
    #[serde(default)]
    capsule_id: String,
}

/// Handle `astrid.v1.request.mcp.ingress.respond`.
///
/// The shim elicited the user's consent for an untrusted ingress (in response
/// to an `ingress_approval_required` reply a prior
/// [`crate::broker::handle_mcp_call`] produced) and forwards the decision
/// here as `{ req_id, accept }`. On `accept:true` we record trust for the
/// ingress under `mcp.ingress.trust.<source_id>` so a re-sent `tool.call`
/// passes the confused-deputy gate; on `accept:false` we record nothing.
/// Either way we ack on `astrid.v1.response.<req_id>` so the shim does not
/// hang.
///
/// **SECURITY-CRITICAL**: the `source_id` trust is recorded against is the
/// kernel-stamped `runtime::caller().source_id` of THIS message — the
/// identity of whoever actually forwarded the `ingress.respond` (the cli
/// proxy). It is NEVER taken from the payload body. A capsule cannot smuggle
/// a trust grant for some OTHER ingress by putting a foreign source_id in the
/// body, because the body carries none and we read only the kernel-stamped
/// caller. An empty / unattributed caller is refused
/// ([`crate::execute::ingress_trust_key`] returns `None`).
///
/// **Correlation gate**: trust is recorded only when this respond consumes an
/// outstanding consent-prompt marker the broker wrote for `source_id` when it
/// replied `ingress_approval_required`
/// ([`crate::execute::take_ingress_pending`]). An accept with no marker —
/// unsolicited, replayed, or for an ingress the broker never prompted on — is
/// refused. The shim mints a FRESH `req_id` per respond and the broker hands
/// it no prompt id to echo, so `source_id` (identical on the gated call and
/// its respond, since the same proxy forwards both) is the only correlation
/// token available without a wire change. The marker is single-use (consumed
/// on accept and decline). This does NOT by itself prove a human (vs a
/// capsule) drove the accept — that remains the publish-ACL on
/// `astrid.v1.request.mcp.ingress.respond`; it removes the unsolicited /
/// replayed-respond residual.
pub(crate) fn handle_mcp_ingress_respond(payload: Value) -> Result<(), SysError> {
    let req: IngressRespond = match serde_json::from_value(payload) {
        Ok(v) => v,
        Err(e) => {
            // No recoverable req_id — there is no channel to ack on. Log
            // and drop; the shim times out its own request.
            log::warn(format!(
                "{}: broker ingress.respond: malformed payload: {e}",
                crate::profile::log_tag()
            ));
            return Ok(());
        }
    };

    log::info(format!(
        "{}: broker ingress method=ingress.respond req_id={} accept={}",
        crate::profile::log_tag(),
        req.req_id,
        req.accept
    ));

    let reply_topic = crate::broker::reply_topic(&req.req_id);

    // The kernel-stamped caller is the ONLY source of the ingress identity.
    let source_id = match runtime::caller() {
        Ok(ctx) => ctx.source_id,
        Err(e) => {
            log::warn(format!(
                "{}: broker ingress.respond: no caller context, recording no trust: {e}",
                crate::profile::log_tag()
            ));
            if let Some(reply) = &reply_topic {
                ingress_ack(reply, &req.req_id, false);
            }
            return Ok(());
        }
    };

    // Correlation gate: only honour a respond that consumes a prompt the
    // broker actually issued for THIS ingress. Consumed on accept AND decline
    // so a declined prompt cannot leave a stale marker a later unsolicited
    // accept could ride. Fail-closed (missing marker / read error → false).
    let had_prompt = crate::execute::take_ingress_pending(&source_id);

    if req.accept {
        if !had_prompt {
            // Unsolicited or replayed accept: no consent prompt is outstanding
            // for this ingress, so the broker never asked. Refuse to record
            // trust and ack as not-granted — the shim will not re-send.
            log::warn(format!(
                "{}: broker ingress.respond: accept with no outstanding prompt for \
                 source_id '{source_id}'; recording no trust",
                crate::profile::log_tag()
            ));
            if let Some(reply) = &reply_topic {
                ingress_ack(reply, &req.req_id, false);
            }
            return Ok(());
        }
        match crate::execute::ingress_trust_key(&source_id) {
            Some(key) => {
                if let Err(e) = kv::set_bytes(&key, b"1") {
                    log::warn(format!(
                        "{}: broker ingress.respond: failed to record trust for \
                         source_id '{source_id}': {e}",
                        crate::profile::log_tag()
                    ));
                    // Could not persist — ack as not-granted so the shim does
                    // not falsely believe trust was recorded.
                    if let Some(reply) = &reply_topic {
                        ingress_ack(reply, &req.req_id, false);
                    }
                    return Ok(());
                }
                log::info(format!(
                    "{}: broker ingress.respond: recorded trust for ingress source_id \
                     '{source_id}'",
                    crate::profile::log_tag()
                ));
                // Best-effort audit on the same `astrid.v1.audit.*` family the
                // policy gate uses. source_id is kernel-stamped, not a
                // reflected argument.
                let _ = ipc::publish_json(
                    &crate::profile::audit_topic("ingress_trusted"),
                    &json!({ "source_id": source_id }),
                );
                if let Some(reply) = &reply_topic {
                    ingress_ack(reply, &req.req_id, true);
                }
            }
            None => {
                // Empty / unattributed caller — refuse to record a routable
                // trust key. Ack as not-granted.
                log::warn(format!(
                    "{}: broker ingress.respond: empty caller source_id; no trust recorded",
                    crate::profile::log_tag()
                ));
                if let Some(reply) = &reply_topic {
                    ingress_ack(reply, &req.req_id, false);
                }
            }
        }
    } else {
        log::info(format!(
            "{}: broker ingress.respond: user declined trust for ingress source_id \
             '{source_id}'",
            crate::profile::log_tag()
        ));
        if let Some(reply) = &reply_topic {
            ingress_ack(reply, &req.req_id, false);
        }
    }
    Ok(())
}

/// Ack an `ingress.respond` to the shim on `astrid.v1.response.<req_id>`.
///
/// `granted` reports whether trust was actually persisted, so the shim only
/// re-sends the parked `tool.call` when the gate will now pass. Best-effort:
/// a publish failure is logged, not retried (the shim times out otherwise).
fn ingress_ack(reply_topic: &str, req_id: &str, granted: bool) {
    let reply = json!({
        "kind": "ingress.respond",
        "req_id": req_id,
        "granted": granted,
    });
    if let Err(e) = ipc::publish_json(reply_topic, &reply) {
        log::warn(format!(
            "{}: broker ingress.respond: failed to ack {reply_topic}: {e}",
            crate::profile::log_tag()
        ));
    }
}

/// Handle `astrid.v1.request.mcp.grant.respond`.
///
/// The shim elicited the user's Grant/Deny choice for a capsule the kernel
/// access gate refused (in response to a `grant_required` flag a prior
/// [`crate::broker::handle_mcp_call`] reply carried) and forwards it here as
/// `{ req_id, request_id, decision, capsule_id }`. This maps the choice onto
/// `astrid.v1.approval.response.<request_id>` — the SAME response topic and
/// `approval_response` envelope the approval flow uses, consumed VERBATIM by
/// the kernel grant handler (#1001), which persists the capsule grant on an
/// APPROVE — then acks the shim on `astrid.v1.response.<req_id>` with
/// `{ kind:"grant.respond", req_id, granted }`.
///
/// An APPROVE is additionally recorded durably
/// ([`crate::grant_decision::record_grant_decision`]) before the decision is
/// published, so the flow converges from the record even when the kernel
/// awaiter that prompted it has expired. A DENY is deliberately NOT recorded:
/// the shim publishes `deny` on every non-accept path (decline, elicit
/// error/timeout, no elicitation capability) with no provenance, so it stays
/// ephemeral — see [`crate::grant_decision::respond_decision_to_record`].
///
/// ## Publish-then-ack, NO result drain (the key divergence)
///
/// Unlike [`handle_mcp_approval`], this handler MUST NOT subscribe to
/// `tool.v1.execute.*.result` and MUST NOT drain a result. grant-on-use mirrors
/// the INGRESS flow: the kernel DROPPED the original `tool.call` at the access
/// gate, so nothing is parked and no result will ever be published. Draining
/// would block the full window then emit a spurious error. The shim RE-SENDS
/// the original `tool.call` itself once it sees `granted:true`. Structurally
/// this matches [`handle_mcp_ingress_respond`] (publish/record then ack), NOT
/// [`handle_mcp_approval`] (publish then drain).
///
/// State-mutating (an APPROVE causes the kernel to persist a capsule grant), so
/// it is confused-deputy gated identically to [`crate::broker::handle_mcp_call`]:
/// the inbound message's kernel-set `source_id` must already be a trusted
/// ingress ([`crate::execute::is_ingress_trusted`]) before any decision is
/// published. A rejected request publishes a `deny` (when a `request_id` is
/// routable) so the kernel gate miss retires cleanly and clears the dedup
/// marker — fail secure.
///
/// The dedup marker for `(principal, capsule_id)` is consumed on BOTH approve
/// and deny ([`crate::execute::take_grant_pending`]) so a declined prompt can
/// never leave a marker that suppresses every future grant prompt for the pair.
/// A payload so malformed it carries no `request_id` (and so no routable deny)
/// — or no `capsule_id` to clear by — is logged and dropped; the kernel gate
/// miss times out on its own schedule and any pending marker self-heals at
/// [`crate::execute::GRANT_PENDING_TTL_MS`], so even that path cannot wedge the
/// pair.
pub(crate) fn handle_mcp_grant_respond(payload: Value) -> Result<(), SysError> {
    let req: GrantRespond = match serde_json::from_value(payload) {
        Ok(v) => v,
        Err(e) => {
            // No recoverable req_id — no channel to ack on, and no request_id
            // to deny. The kernel grant gate retires on its own schedule, and a
            // pending marker self-heals at GRANT_PENDING_TTL_MS, so dropping
            // here cannot wedge the pair. Log and drop.
            log::warn(format!(
                "{}: broker grant.respond: malformed payload: {e}",
                crate::profile::log_tag()
            ));
            return Ok(());
        }
    };

    log::info(format!(
        "{}: broker ingress method=grant.respond req_id={} request_id={} capsule_id={}",
        crate::profile::log_tag(),
        req.req_id,
        req.request_id,
        req.capsule_id
    ));

    let reply_topic = crate::broker::reply_topic(&req.req_id);
    let Some(response_topic) = response_topic(&req.request_id) else {
        log::warn(format!(
            "{}: broker grant.respond: rejecting unroutable request_id '{}'",
            crate::profile::log_tag(),
            req.request_id
        ));
        // Clear any dedup marker so the next ungranted call can re-prompt — the
        // decision was never published (bad request_id), so leaving the marker
        // would wedge the pair. The caller principal scopes the KV; the suffix
        // is the capsule id.
        clear_grant_marker(&req.capsule_id);
        // Ack the shim (when routable) so it does not hang; not granted.
        if let Some(reply) = &reply_topic {
            grant_ack(reply, &req.req_id, false);
        }
        return Ok(());
    };

    // Confused-deputy gate. An APPROVE here causes the kernel to persist a
    // capsule grant — the most sensitive action on this path — so require the
    // kernel-set `source_id` (NOT a body field) to be a trusted ingress. On any
    // failure (no caller context, untrusted ingress) publish a `deny` so the
    // kernel gate miss retires, clear the marker, and ack not-granted.
    let source_id = match runtime::caller() {
        Ok(ctx) => ctx.source_id,
        Err(e) => {
            log::warn(format!(
                "{}: broker grant.respond: no caller context, denying request_id '{}': {e}",
                crate::profile::log_tag(),
                req.request_id
            ));
            publish_decision(&response_topic, &req.request_id, DENY, None);
            clear_grant_marker(&req.capsule_id);
            if let Some(reply) = &reply_topic {
                grant_ack(reply, &req.req_id, false);
            }
            return Ok(());
        }
    };
    if !crate::execute::is_ingress_trusted(&source_id) {
        log::warn(format!(
            "{}: broker grant.respond: untrusted ingress source_id '{source_id}', \
             denying request_id '{}'",
            crate::profile::log_tag(),
            req.request_id
        ));
        publish_decision(&response_topic, &req.request_id, DENY, None);
        clear_grant_marker(&req.capsule_id);
        if let Some(reply) = &reply_topic {
            grant_ack(reply, &req.req_id, false);
        }
        return Ok(());
    }

    // Normalise the shim's choice to exactly one host verb. Unknown / empty
    // collapses to `deny` — fail secure: no string the shim sends grants a
    // capsule unless it is exactly an approve verb.
    let decision = normalize_decision(&req.decision);
    let granted = is_approve_verb(decision);

    // Record a DURABLE decision BEFORE forwarding it to the kernel, so it
    // survives even if the kernel awaiter that raised this grant has already
    // expired fail-closed (its 60 s window vs. a slow or re-asked human answer):
    // the next call for this capsule then converges from the record without
    // re-prompting (see [`crate::grant_decision`] and the broker's auto-respond
    // path). APPROVE-ONLY: the shim publishes `deny` on every non-accept path —
    // user decline, elicit error, elicit timeout, or a client with no
    // elicitation support — with no provenance in the respond body to tell them
    // apart, so durably recording a deny would make a transport glitch a
    // permanent auto-deny the user never chose. A deny keeps its ephemeral
    // semantics (marker consumed below, next call re-prompts: "not now", never
    // "never") until the respond carries provenance
    // (astrid-runtime/astrid#1114). The approve/skip choice lives in the pure
    // [`crate::grant_decision::respond_decision_to_record`] chokepoint. This
    // runs only AFTER the confused-deputy gate above has passed, so a
    // security-refusal deny (no caller context / untrusted ingress, handled
    // earlier) records nothing either way — it is not the user's decision.
    if let Some(record) = crate::grant_decision::respond_decision_to_record(granted) {
        crate::grant_decision::record_grant_decision(&req.capsule_id, record);
    }

    // Publish the decision on the per-request response topic. The kernel grant
    // handler consumes this `approval_response` verbatim and, on an approve,
    // persists the capsule grant. NO result drain follows — the call was
    // dropped at the gate; the shim re-sends it on `granted:true`.
    publish_decision(&response_topic, &req.request_id, decision, None);

    // Consume the dedup marker on BOTH approve and deny so it is single-use and
    // can never stick. The grant itself is driven by the published decision, not
    // this marker; clearing it here is what lets the next call re-prompt (and a
    // TTL self-heal backstops the clear if this respond never arrives).
    clear_grant_marker(&req.capsule_id);

    if let Some(reply) = &reply_topic {
        grant_ack(reply, &req.req_id, granted);
    } else {
        log::warn(format!(
            "{}: broker grant.respond: decision published but req_id '{}' unroutable; \
             no ack delivered",
            crate::profile::log_tag(),
            req.req_id
        ));
    }
    Ok(())
}

/// Whether a normalised host decision verb is one of the three APPROVE verbs
/// (i.e. the kernel will GRANT). Anything else — `deny` — is not a grant. Used
/// to report `granted` in the grant ack so the shim only re-sends the dropped
/// `tool.call` when the capsule will now be granted.
fn is_approve_verb(decision: &str) -> bool {
    matches!(decision, APPROVE | APPROVE_SESSION | APPROVE_ALWAYS)
}

/// Consume the `(principal, capsule_id)` grant-pending dedup marker. The KV
/// scope is per-principal (the caller's principal), so the capsule id is the
/// key suffix; the principal argument is passed for intent only. Best-effort —
/// a failure to clear just risks one extra suppressed prompt, never a spurious
/// grant. Called on every respond outcome so the marker is single-use.
///
/// An empty `capsule_id` (a respond that omitted the field) makes this a no-op —
/// there is no key to clear. That does not wedge the pair: the marker carries a
/// write timestamp and self-heals at [`crate::execute::GRANT_PENDING_TTL_MS`],
/// so a missing-`capsule_id` respond degrades to a slightly delayed re-prompt,
/// never permanent suppression.
fn clear_grant_marker(capsule_id: &str) {
    let principal = runtime::caller()
        .ok()
        .and_then(|ctx| ctx.principal)
        .unwrap_or_default();
    let _ = crate::execute::take_grant_pending(&principal, capsule_id);
}

/// Ack a `grant.respond` to the shim on `astrid.v1.response.<req_id>`.
///
/// `granted` reports whether the kernel will grant the capsule (an approve
/// verb), so the shim only RE-SENDS the dropped `tool.call` when the access
/// gate will now pass. Mirrors [`ingress_ack`]'s shape with a `grant.respond`
/// kind. Best-effort: a publish failure is logged, not retried.
fn grant_ack(reply_topic: &str, req_id: &str, granted: bool) {
    let reply = json!({
        "kind": "grant.respond",
        "req_id": req_id,
        "granted": granted,
    });
    if let Err(e) = ipc::publish_json(reply_topic, &reply) {
        log::warn(format!(
            "{}: broker grant.respond: failed to ack {reply_topic}: {e}",
            crate::profile::log_tag()
        ));
    }
}

/// Publish `decision` on the per-request response topic to unblock the host,
/// then drain the resumed/denied tool result and deliver it to the shim.
///
/// Subscribe-before-publish: the result subscription on
/// `tool.v1.execute.<tool_name>.result` is established BEFORE the decision is
/// published so the tool cannot resume and publish ahead of us. Then we drain
/// for `call_id` and reply on `astrid.v1.response.<req_id>` with the same
/// terminal `tool.call` shape the broker's non-parked path produces.
///
/// `tool_name` is charset-validated (it must form the result topic). A
/// non-routable `tool_name`, a subscribe failure, or a drain timeout still
/// publishes the decision (so the host-blocked tool is never left hanging)
/// and delivers a best-effort `isError` reply to the shim.
fn resolve_with_decision(
    req: &ApprovalRespond,
    response_topic: &str,
    decision: &'static str,
    reason: Option<&str>,
) {
    let reply_topic = crate::broker::reply_topic(&req.req_id);

    // Validate the tool name BEFORE building the result topic — the same
    // charset gate the execute path applies, so a hostile / buggy echo
    // cannot smuggle topic segments into our subscription.
    if !crate::execute::is_valid_tool_name(&req.tool_name) {
        log::warn(format!(
            "{}: broker approval.respond: invalid tool_name '{}' for request_id '{}'; \
             publishing decision without result drain",
            crate::profile::log_tag(),
            req.tool_name,
            req.request_id
        ));
        // Still unblock the host so the tool retires, then surface an error.
        publish_decision(response_topic, &req.request_id, decision, reason);
        if let Some(reply) = &reply_topic {
            deliver_error(reply, &req.req_id, "approval echoed an invalid tool name");
        }
        return;
    }

    let result_topic = format!("tool.v1.execute.{}.result", req.tool_name);

    // Subscribe BEFORE publishing the decision so the resumed tool's result
    // cannot race ahead of our subscription. A subscribe failure is
    // non-fatal: we still publish the decision (the host must not be left
    // blocked) and fall back to a best-effort error reply.
    let result_sub = match ipc::subscribe(&result_topic) {
        Ok(s) => Some(s),
        Err(e) => {
            log::warn(format!(
                "{}: broker approval.respond: failed to subscribe {result_topic}: {e}",
                crate::profile::log_tag()
            ));
            None
        }
    };

    publish_decision(response_topic, &req.request_id, decision, reason);

    let Some(reply) = &reply_topic else {
        // Decision is published (tool unblocked) but we have no channel to
        // deliver the terminal result on. The shim will time out its own
        // request; nothing more we can do.
        log::warn(format!(
            "{}: broker approval.respond: decision published but req_id '{}' unroutable; \
             no terminal reply delivered",
            crate::profile::log_tag(),
            req.req_id
        ));
        return;
    };

    // Drain the resumed/denied result and deliver it as the terminal reply.
    match result_sub.and_then(|sub| drain_result(&sub, &req.call_id)) {
        Some((content, is_error)) => deliver_result(reply, &req.req_id, content, is_error),
        None => deliver_error(
            reply,
            &req.req_id,
            "tool did not return a result after the approval decision",
        ),
    }
}

/// Drain `tool.v1.execute.<name>.result` for `call_id` after a decision,
/// returning `(content, is_error)` for the first matching result or `None` on
/// timeout. Bounded by [`RESUME_TIMEOUT_MS`]; polled in [`RESUME_SLICE_MS`]
/// slices so a result published the instant the tool resumes is picked up
/// promptly. Reuses the execute path's result-envelope parser so both legs
/// agree on the wire shape.
fn drain_result(sub: &ipc::Subscription, call_id: &str) -> Option<(Value, bool)> {
    let mut remaining = RESUME_TIMEOUT_MS;
    while remaining > 0 {
        let step = remaining.min(RESUME_SLICE_MS);
        match sub.recv(step) {
            Ok(poll) => {
                for msg in poll.messages {
                    if let Some(found) = crate::execute::match_result(&msg.payload, call_id) {
                        return Some(found);
                    }
                }
            }
            Err(_) => {
                // Slice timeout — keep draining until the budget closes.
            }
        }
        remaining = remaining.saturating_sub(step);
    }
    None
}

/// Deliver a terminal `tool.call` reply to the shim carrying the resumed/
/// denied tool result, mirroring [`crate::broker::handle_mcp_call`]'s
/// non-parked reply shape so the shim needs no special-casing.
fn deliver_result(reply_topic: &str, req_id: &str, content: Value, is_error: bool) {
    let reply = json!({
        "kind": "tool.call",
        "req_id": req_id,
        "content": crate::broker::mcp_content(content),
        "isError": is_error,
    });
    if let Err(e) = ipc::publish_json(reply_topic, &reply) {
        log::warn(format!(
            "{}: broker approval.respond: failed to deliver result {reply_topic}: {e}",
            crate::profile::log_tag()
        ));
    }
}

/// Deliver a terminal `isError` `tool.call` reply to the shim with `text` as
/// the body. Used for every failure path so the shim always gets exactly one
/// terminal reply and never hangs.
fn deliver_error(reply_topic: &str, req_id: &str, text: &str) {
    deliver_result(
        reply_topic,
        req_id,
        Value::String(format!("{}: {text}", crate::profile::log_tag())),
        true,
    );
}

/// Normalise an arbitrary decision string to one of the host's recognised
/// approval verbs. Trims and lowercases first so `"Approve"` / `" deny "`
/// behave intuitively, then maps; anything unrecognised → `deny`.
fn normalize_decision(decision: &str) -> &'static str {
    match decision.trim().to_ascii_lowercase().as_str() {
        APPROVE => APPROVE,
        APPROVE_SESSION => APPROVE_SESSION,
        APPROVE_ALWAYS => APPROVE_ALWAYS,
        // `deny` and EVERYTHING else (unknown verbs, empty, free-form) →
        // deny. Fail secure: no string the shim can send grants a
        // capability unless it is exactly one of the three approve verbs.
        _ => DENY,
    }
}

/// Publish the `IpcPayload::ApprovalResponse` decision on the per-request
/// response topic so the blocked host `request_approval` wakes. The `type`
/// tag matches the host's `#[serde(tag = "type", rename_all =
/// "snake_case")]` discriminator for `ApprovalResponse`.
fn publish_decision(topic: &str, request_id: &str, decision: &str, reason: Option<&str>) {
    let mut envelope = json!({
        "type": "approval_response",
        "request_id": request_id,
        "decision": decision,
    });
    if let Some(r) = reason
        && let Some(obj) = envelope.as_object_mut()
    {
        // `reason` is a display/audit field on the host side — clamp it
        // like the other display strings so a hostile shim can't inflate
        // it. Empty after clamping is omitted.
        let clamped = clamp(r);
        if !clamped.is_empty() {
            obj.insert("reason".to_string(), Value::String(clamped));
        }
    }
    if let Err(e) = ipc::publish_json(topic, &envelope) {
        log::warn(format!(
            "{}: failed to publish approval decision {topic}: {e}",
            crate::profile::log_tag()
        ));
    }
}

/// Publish an auto-decision for an observed grant-gate miss WITHOUT eliciting —
/// the broker's realization of a DURABLE recorded decision
/// ([`crate::grant_decision::recorded_grant_decision`]). Maps the choice onto the
/// SAME `astrid.v1.approval.response.<request_id>` envelope a human
/// `grant.respond` would produce, so the kernel grant handler consumes it
/// verbatim: an approve persists the grant against the (fresh, live) awaiter that
/// raised this signal, a deny retires that awaiter cleanly instead of letting it
/// fail-closed at 60 s. NO result drain — grant-on-use dropped the original call;
/// the caller re-sends. Returns whether the `request_id` was routable (an
/// unroutable id publishes nothing and the kernel awaiter times out on its own).
///
/// SECURITY: this only ever answers a KERNEL-CORRELATED `GrantRequired` whose
/// `request_id` the kernel access gate minted. The broker never grants anything
/// itself — it replays a decision the user already recorded onto the kernel's own
/// response topic; the kernel remains the sole grantor, and the
/// response-never-carries-a-target invariant is untouched.
pub(crate) fn publish_grant_auto_decision(request_id: &str, approve: bool) -> bool {
    let Some(topic) = response_topic(request_id) else {
        log::warn(format!(
            "{}: broker grant auto-decision: unroutable request_id '{request_id}'; \
             not publishing (kernel awaiter will time out on its own)",
            crate::profile::log_tag()
        ));
        return false;
    };
    let decision = if approve { APPROVE } else { DENY };
    publish_decision(&topic, request_id, decision, None);
    true
}

/// Build the single-segment decision topic for `request_id`, or `None` if
/// the id cannot form a clean topic segment.
///
/// Same charset family as [`crate::broker::reply_topic`]: a `.` would turn
/// the egress topic into one the host's exact
/// `astrid.v1.approval.response.<id>` subscription can't match, and
/// whitespace / control / wildcard bytes could forge or shadow topics.
fn response_topic(request_id: &str) -> Option<String> {
    if request_id.is_empty() || request_id.len() > MAX_REQUEST_ID_LEN {
        return None;
    }
    let clean = request_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
    if !clean {
        return None;
    }
    Some(format!("{APPROVAL_RESPONSE_PREFIX}{request_id}"))
}

/// Trim + length-clamp a host display string defensively. The host
/// already sanitizes these; this is belt-and-suspenders against a future
/// producer that skips it.
fn clamp(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= MAX_DISPLAY_LEN {
        return trimmed.to_string();
    }
    trimmed.chars().take(MAX_DISPLAY_LEN).collect()
}

#[cfg(test)]
mod tests {
    fn install_test_profile() {
        crate::profile::install_aos();
    }

    use super::*;

    #[test]
    fn approval_required_parses_canonical_envelope() {
        install_test_profile();
        let v = json!({
            "type": "approval_required",
            "request_id": "0191f3a2b4c74d8e9f01234567890abc",
            "action": "git push",
            "resource": "git push origin main",
            "reason": "Capsule 'shell' requests approval",
        });
        assert!(is_approval_required(&v));
        let req: ApprovalRequired = serde_json::from_value(v).unwrap();
        assert_eq!(req.request_id, "0191f3a2b4c74d8e9f01234567890abc");
        assert_eq!(req.action, "git push");
    }

    #[test]
    fn is_approval_required_rejects_other_payloads() {
        install_test_profile();
        // Wrong tag.
        assert!(!is_approval_required(&json!({
            "type": "approval_response",
            "request_id": "x",
            "decision": "approve"
        })));
        // Right tag, empty request_id.
        assert!(!is_approval_required(&json!({
            "type": "approval_required",
            "request_id": ""
        })));
        // No tag at all.
        assert!(!is_approval_required(&json!({ "request_id": "x" })));
    }

    #[test]
    fn normalize_decision_maps_approve_verbs() {
        install_test_profile();
        assert_eq!(normalize_decision("approve"), APPROVE);
        assert_eq!(normalize_decision("approve_session"), APPROVE_SESSION);
        assert_eq!(normalize_decision("approve_always"), APPROVE_ALWAYS);
    }

    #[test]
    fn normalize_decision_is_case_and_whitespace_insensitive() {
        install_test_profile();
        assert_eq!(normalize_decision("  Approve "), APPROVE);
        assert_eq!(normalize_decision("APPROVE_ALWAYS"), APPROVE_ALWAYS);
    }

    #[test]
    fn normalize_decision_fails_secure_on_unknown() {
        install_test_profile();
        // Anything not exactly an approve verb denies — no string the shim
        // sends can smuggle a grant.
        assert_eq!(normalize_decision("deny"), DENY);
        assert_eq!(normalize_decision(""), DENY);
        assert_eq!(normalize_decision("yes"), DENY);
        assert_eq!(normalize_decision("approve; rm -rf /"), DENY);
        assert_eq!(normalize_decision("allow"), DENY);
    }

    #[test]
    fn response_topic_accepts_uuid() {
        install_test_profile();
        assert_eq!(
            response_topic("0191f3a2b4c74d8e9f01234567890abc").as_deref(),
            Some("astrid.v1.approval.response.0191f3a2b4c74d8e9f01234567890abc")
        );
        assert!(response_topic("0191f3a2-b4c7-4d8e-9f01-234567890abc").is_some());
    }

    #[test]
    fn response_topic_rejects_topic_smuggling() {
        install_test_profile();
        assert!(response_topic("").is_none());
        assert!(response_topic("a.b").is_none());
        assert!(response_topic("a b").is_none());
        assert!(response_topic("a*b").is_none());
        assert!(response_topic("a/b").is_none());
        assert!(response_topic("a\nb").is_none());
        let too_long = "a".repeat(MAX_REQUEST_ID_LEN + 1);
        assert!(response_topic(&too_long).is_none());
    }

    #[test]
    fn reply_flag_carries_display_and_routing_fields_only() {
        install_test_profile();
        let req = ApprovalRequired {
            request_id: "rid".to_string(),
            action: "  git push  ".to_string(),
            resource: "git push origin main".to_string(),
            reason: "needs consent".to_string(),
        };
        let flag = req.to_reply_flag("shell.exec", "call-7");
        assert_eq!(flag["request_id"], "rid");
        // Trimmed.
        assert_eq!(flag["action"], "git push");
        assert_eq!(flag["resource"], "git push origin main");
        assert_eq!(flag["reason"], "needs consent");
        // Routing tokens the shim must echo back so the resume path can
        // re-establish the result drain. These are not secrets — they are the
        // same tokens the shim used to make the original tool.call.
        assert_eq!(flag["tool_name"], "shell.exec");
        assert_eq!(flag["call_id"], "call-7");
        // No arguments / secret fields leak in.
        assert!(flag.get("arguments").is_none());
    }

    #[test]
    fn clamp_truncates_oversize_display() {
        install_test_profile();
        let long = "a".repeat(MAX_DISPLAY_LEN + 50);
        assert_eq!(clamp(&long).chars().count(), MAX_DISPLAY_LEN);
        assert_eq!(clamp("  hi  "), "hi");
    }

    #[test]
    fn approval_respond_parses_minimum_shape() {
        install_test_profile();
        let v = json!({
            "req_id": "r1",
            "request_id": "rid",
            "decision": "approve",
            "tool_name": "shell.exec",
            "call_id": "r1",
        });
        let parsed: ApprovalRespond = serde_json::from_value(v).unwrap();
        assert_eq!(parsed.req_id, "r1");
        assert_eq!(parsed.request_id, "rid");
        assert_eq!(parsed.decision, "approve");
        assert_eq!(parsed.tool_name, "shell.exec");
        assert_eq!(parsed.call_id, "r1");
        assert!(parsed.reason.is_none());
    }

    #[test]
    fn approval_respond_requires_routing_fields() {
        install_test_profile();
        // `tool_name` / `call_id` are mandatory — without them the resume
        // path cannot re-establish the result drain, so the wire shape must
        // reject a payload missing them rather than silently default.
        assert!(
            serde_json::from_value::<ApprovalRespond>(json!({
                "req_id": "r1",
                "request_id": "rid",
                "decision": "approve",
            }))
            .is_err()
        );
    }

    #[test]
    fn ingress_respond_parses_minimum_shape() {
        install_test_profile();
        let v = json!({ "req_id": "r1", "accept": true });
        let parsed: IngressRespond = serde_json::from_value(v).unwrap();
        assert_eq!(parsed.req_id, "r1");
        assert!(parsed.accept);
    }

    #[test]
    fn ingress_respond_requires_accept_and_req_id() {
        install_test_profile();
        // `req_id` is the only ack channel; `accept` is the decision. Both
        // are mandatory — a payload missing either is rejected rather than
        // silently defaulting (a defaulted `accept` could mean a silent
        // grant or a silent deny, both wrong).
        assert!(serde_json::from_value::<IngressRespond>(json!({ "accept": true })).is_err());
        assert!(serde_json::from_value::<IngressRespond>(json!({ "req_id": "r1" })).is_err());
    }

    #[test]
    fn ingress_respond_ignores_body_source_id() {
        install_test_profile();
        // SECURITY: a `source_id` in the body must be inert — the trust write
        // keys on the kernel-stamped caller, never this field. The struct does
        // not deserialize it, so a hostile body cannot smuggle a foreign
        // ingress identity through the wire shape.
        let v = json!({
            "req_id": "r1",
            "accept": true,
            "source_id": "attacker-controlled-uuid",
        });
        let parsed: IngressRespond = serde_json::from_value(v).unwrap();
        assert_eq!(parsed.req_id, "r1");
        assert!(parsed.accept);
        // No field on the struct carries the body source_id — it is dropped.
    }

    #[test]
    fn approval_respond_carries_optional_reason() {
        install_test_profile();
        let v = json!({
            "req_id": "r1",
            "request_id": "rid",
            "decision": "deny",
            "tool_name": "shell.exec",
            "call_id": "r1",
            "reason": "user declined",
        });
        let parsed: ApprovalRespond = serde_json::from_value(v).unwrap();
        assert_eq!(parsed.reason.as_deref(), Some("user declined"));
    }

    // ------------------------------------------------------------------
    // grant-on-use (grant_required signal + grant.respond bridge).
    // ------------------------------------------------------------------

    #[test]
    fn grant_required_parses_canonical_envelope() {
        install_test_profile();
        let v = json!({
            "type": "grant_required",
            "request_id": "0191f3a2b4c74d8e9f01234567890abc",
            "principal": "alice",
            "capsule_id": "fs",
        });
        assert!(is_grant_required(&v));
        let req: GrantRequired = serde_json::from_value(v).unwrap();
        assert_eq!(req.request_id, "0191f3a2b4c74d8e9f01234567890abc");
        assert_eq!(req.principal, "alice");
        assert_eq!(req.capsule_id, "fs");
    }

    #[test]
    fn is_grant_required_rejects_other_payloads() {
        install_test_profile();
        // Wrong tag — the sibling approval_required must NOT be read as a grant.
        assert!(!is_grant_required(&json!({
            "type": "approval_required",
            "request_id": "x",
        })));
        // Right tag, empty request_id (can't be routed back).
        assert!(!is_grant_required(&json!({
            "type": "grant_required",
            "request_id": ""
        })));
        // No tag at all.
        assert!(!is_grant_required(&json!({ "request_id": "x" })));
    }

    #[test]
    fn is_approval_required_rejects_grant_required() {
        install_test_profile();
        // Symmetry: the grant envelope must NOT be read as an approval, so the
        // combined poll classifies each signal to exactly one outcome.
        assert!(!is_approval_required(&json!({
            "type": "grant_required",
            "request_id": "x",
            "capsule_id": "fs",
        })));
    }

    #[test]
    fn grant_reply_flag_carries_display_and_routing_fields_only() {
        install_test_profile();
        let req = GrantRequired {
            request_id: "rid".to_string(),
            principal: "  alice  ".to_string(),
            capsule_id: "fs".to_string(),
        };
        let flag = req.to_reply_flag("fs.read", "call-7");
        assert_eq!(flag["request_id"], "rid");
        // Trimmed display fields.
        assert_eq!(flag["principal"], "alice");
        assert_eq!(flag["capsule_id"], "fs");
        // Routing tokens the shim echoes / re-sends with.
        assert_eq!(flag["tool_name"], "fs.read");
        assert_eq!(flag["call_id"], "call-7");
        // No arguments / secret fields leak in.
        assert!(flag.get("arguments").is_none());
    }

    #[test]
    fn grant_respond_parses_minimum_shape() {
        install_test_profile();
        let v = json!({
            "req_id": "r1",
            "request_id": "rid",
            "decision": "approve",
            "capsule_id": "fs",
        });
        let parsed: GrantRespond = serde_json::from_value(v).unwrap();
        assert_eq!(parsed.req_id, "r1");
        assert_eq!(parsed.request_id, "rid");
        assert_eq!(parsed.decision, "approve");
        assert_eq!(parsed.capsule_id, "fs");
    }

    #[test]
    fn grant_respond_requires_req_id_request_id_decision() {
        install_test_profile();
        // `req_id` (ack channel), `request_id` (decision topic), and `decision`
        // are mandatory; only `capsule_id` defaults (used solely to clear the
        // dedup marker — its absence degrades to a no-op clear, never a wrong
        // decision). A payload missing any of the three is rejected.
        assert!(
            serde_json::from_value::<GrantRespond>(json!({
                "request_id": "rid",
                "decision": "approve",
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<GrantRespond>(json!({
                "req_id": "r1",
                "decision": "approve",
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<GrantRespond>(json!({
                "req_id": "r1",
                "request_id": "rid",
            }))
            .is_err()
        );
    }

    #[test]
    fn grant_respond_does_not_require_tool_name_or_call_id() {
        install_test_profile();
        // Divergence from ApprovalRespond: grant-on-use does NOT drain a result
        // (the kernel dropped the call), so the wire shape carries no
        // `tool_name` / `call_id`. A payload without them must still parse.
        let v = json!({
            "req_id": "r1",
            "request_id": "rid",
            "decision": "deny",
        });
        let parsed: GrantRespond = serde_json::from_value(v).unwrap();
        assert_eq!(parsed.capsule_id, "");
    }

    #[test]
    fn is_approve_verb_only_true_for_approve_verbs() {
        install_test_profile();
        // The grant ack's `granted` flag — true only when the kernel will grant
        // (an approve verb), so the shim re-sends only on a real grant.
        assert!(is_approve_verb(APPROVE));
        assert!(is_approve_verb(APPROVE_SESSION));
        assert!(is_approve_verb(APPROVE_ALWAYS));
        assert!(!is_approve_verb(DENY));
    }

    #[test]
    fn grant_respond_decision_normalizes_to_grant_or_deny() {
        install_test_profile();
        // End-to-end of the verb spine the handler uses: an approve verb → the
        // kernel grants (granted:true); anything else → deny (granted:false),
        // fail secure. No string the shim sends grants unless it is exactly an
        // approve verb.
        assert!(is_approve_verb(normalize_decision("approve")));
        assert!(is_approve_verb(normalize_decision("  Approve_Always ")));
        assert!(!is_approve_verb(normalize_decision("deny")));
        assert!(!is_approve_verb(normalize_decision("")));
        assert!(!is_approve_verb(normalize_decision("grant")));
        assert!(!is_approve_verb(normalize_decision("approve; rm -rf /")));
    }
}
