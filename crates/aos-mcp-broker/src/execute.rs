//! Tool execute core — `tool.v1.execute.<name>` fan-out and
//! `tool.v1.execute.<name>.result` drain, behind the `astrid.v1.*` broker.
//!
//! Wire contract:
//!
//! * **Outbound request** (`tool.v1.execute.<tool_name>`): the SDK-canonical
//!   `ToolExecuteRequest` shape `{ type:"tool_execute_request", call_id,
//!   tool_name, arguments }`, mirroring what the router capsule emits.
//!   The handler-side macro deserializes via `__AstridToolExecPayload`
//!   which only requires `{ call_id, tool_name, arguments }`, so the
//!   tagged form is accepted unchanged.
//! * **Inbound result** (`tool.v1.execute.<tool_name>.result`): parsed by
//!   [`match_result`], filtered on `call_id`, and returned to the broker
//!   caller as `(content, is_error)` for reshaping into the MCP
//!   `tool.call` reply.
//!
//! The bare tool name is supplied by the broker, which strips the
//! `mcp__aos__` MCP prefix and charset-validates before constructing the
//! routed topic — see [`crate::broker`]. The single execution door is the
//! broker; there is no `astrid-removed tool.call.*` agent-runner leg (it was
//! retired — the registered `aos mcp serve` MCP server is where the
//! supervised `claude -p` executes tools, so an inline runner dispatch would
//! double-execute).

use astrid_sdk::prelude::*;
use serde_json::{Value, json};

use crate::approval::{self, ApprovalRequired, GrantRequired};

/// Per-call drain window for the `tool.v1.execute.<name>.result` reply.
/// Bounded well under the runner's 60 s `TOOL_CALL_DEADLINE` so the bridge
/// times out first and synthesises a clean `isError:true` result rather
/// than letting the supervisor's deadline-sweeper write back a generic
/// "deadline exceeded" string. 50 s leaves comfortable headroom for the
/// stdin write + bus hop on top of a worst-case tool runtime.
const EXECUTE_TIMEOUT_MS: u64 = 50_000;

/// Slice length for the result-drain loop. A single `recv(timeout)`
/// would only pick up the first batch on the subscription; the loop
/// keeps polling in shorter slices until either the matching result
/// arrives or the timeout budget closes.
const EXECUTE_SLICE_MS: u64 = 250;

/// Tool-name charset cap. Same shape as the discovery validator
/// ([`crate::discovery`]) — names must be non-empty, ASCII
/// alphanumeric plus `_`, `.`, `-`. Rejects path separators, unicode
/// bidi overrides, control chars, and the like before they can reach
/// the routed topic. The hostile input here is the inbound
/// `astrid.v1.request.mcp.tool.call` payload — a crafted `tool_name`
/// that, if appended verbatim, would forge or shadow a
/// `tool.v1.execute.*` topic.
const MAX_TOOL_NAME_LEN: usize = 128;

/// KV key prefix recording an ingress `source_id` the user has consented
/// to trust for state-mutating broker calls. See [`is_ingress_trusted`].
const INGRESS_TRUST_KEY_PREFIX: &str = "mcp.ingress.trust.";

/// KV key prefix marking that the broker has an OUTSTANDING consent prompt
/// for an ingress `source_id` — written when [`crate::broker::handle_mcp_call`]
/// replies `ingress_approval_required` and consumed by
/// [`crate::approval::handle_mcp_ingress_respond`]. See
/// [`mark_ingress_pending`] / [`take_ingress_pending`].
const INGRESS_PENDING_KEY_PREFIX: &str = "mcp.ingress.pending.";

/// KV key prefix marking that the broker has an OUTSTANDING capsule-grant
/// consent prompt for a `(principal, capsule_id)` pair — written when
/// [`crate::broker::handle_mcp_call`] surfaces a `grant_required` flag and
/// consumed by [`crate::approval::handle_mcp_grant_respond`]. See
/// [`mark_grant_pending`] / [`take_grant_pending`]. KV is per-principal-scoped
/// by the kernel, so the capsule id alone suffices within the keyspace.
const GRANT_PENDING_KEY_PREFIX: &str = "mcp.grant.pending.";

/// Self-heal TTL for a grant-pending marker, in milliseconds.
///
/// Unlike the ingress marker — which clears on a kernel-stamped `source_id`
/// always present on the respond — a grant marker clears on the respond's
/// `capsule_id` body field. If the shim crashes after the broker marked
/// pending, or ever sends a respond without that field, the marker would
/// otherwise stick and suppress EVERY future grant prompt for the pair. So the
/// marker carries the wall-clock ms it was written, and a marker older than
/// this is treated as stale (ignored, best-effort deleted) — the dedup
/// self-heals regardless of which leg dropped the clear. Wall-clock (not
/// monotonic) so a daemon restart, after which the KV marker survives but a
/// monotonic clock would reset, still measures elapsed time correctly. Set
/// well above the kernel's 60 s grant-awaiter window so a marker never expires
/// while its grant could still land: the worst case is one duplicate prompt,
/// never a double grant (the kernel grant is idempotent).
pub(crate) const GRANT_PENDING_TTL_MS: u64 = 120_000;

/// Outcome of a broker tool dispatch that watches for a mid-call approval.
///
/// The capability-gated tool's host-side `request_approval` syscall
/// publishes an `astrid.v1.approval` envelope and BLOCKS its WASM thread
/// until a decision lands. When [`dispatch_with_approval`] observes that
/// envelope it short-circuits with [`DispatchOutcome::ApprovalRequired`]
/// so the broker can surface the elicit flag in its reply rather than
/// burning the whole drain window on a tool that cannot make progress
/// until a human (via the shim → Claude) decides.
pub(crate) enum DispatchOutcome {
    /// The tool produced a result within the window: `(content, is_error)`.
    Result(Value, bool),
    /// The tool requested capability approval mid-call; the broker must
    /// relay this to the shim and, on the returned choice, publish the
    /// matching `astrid.v1.approval.response.<request_id>` decision.
    ApprovalRequired(ApprovalRequired),
    /// The kernel access gate refused the call because the principal has not
    /// granted the target capsule. The kernel DROPPED the original
    /// `tool.call` (nothing is parked) and published a `grant_required`
    /// signal; the broker must surface a `grant_required` flag so the shim
    /// can elicit consent and, on approve, publish the
    /// `astrid.v1.approval.response.<request_id>` decision the kernel grant
    /// handler consumes — then RE-SEND the original call. This mirrors the
    /// ingress gate → consent → re-send flow, NOT the approval park →
    /// resume flow: there is no result to drain.
    GrantRequired(GrantRequired),
    /// Dispatch failed before producing either (subscribe / publish error,
    /// drain timeout with no approval observed).
    Failed(String),
}

/// Broker dispatch: subscribe-before-publish on `tool.v1.execute.<name>`,
/// drain `tool.v1.execute.<name>.result` for the matching `call_id`, and
/// additionally watch `astrid.v1.approval` for the duration of the drain.
///
/// The sole execute path behind the `astrid.v1.request.mcp.tool.call`
/// broker — the wire-shape, the charset/topic-segment hardening, the 50 s
/// bounded drain, and the `call_id` filtering live here. It subscribes
/// (before publishing the execute request) to the fixed `astrid.v1.approval`
/// topic too. If the routed tool blocks on a capability approval, that
/// envelope arrives on this subscription and the dispatch returns
/// [`DispatchOutcome::ApprovalRequired`] so the broker can drive the
/// elicitation/approval bridge ([`crate::approval`]). Otherwise the first
/// matching result wins; a closed window → `Failed`.
///
/// `tool_name` MUST already be charset-validated by the caller (the broker
/// validates via [`is_valid_tool_name`] before calling) — constructing the
/// routed topic from an unchecked name would let a hostile publisher forge
/// `tool.v1.execute.*` segments.
pub(crate) fn dispatch_with_approval(
    tool_name: &str,
    call_id: &str,
    arguments: &Value,
) -> DispatchOutcome {
    if !is_valid_tool_name(tool_name) {
        return DispatchOutcome::Failed(format!(
            "{}: invalid tool name '{tool_name}'",
            crate::profile::log_tag()
        ));
    }

    let route_topic = format!("tool.v1.execute.{tool_name}");
    let result_topic = format!("tool.v1.execute.{tool_name}.result");

    // Subscribe to BOTH the per-tool result topic and the fixed approval
    // topic BEFORE publishing the execute request, so neither a fast
    // result nor a fast approval can race ahead of our subscription. RAII
    // Drop on both handles releases the kernel-side resources on every
    // return path.
    let result_sub = match ipc::subscribe(&result_topic) {
        Ok(s) => s,
        Err(e) => {
            return DispatchOutcome::Failed(format!(
                "{}: failed to subscribe to {result_topic}: {e}",
                crate::profile::log_tag()
            ));
        }
    };
    let approval_sub = match ipc::subscribe(approval::APPROVAL_REQUEST_TOPIC) {
        Ok(s) => s,
        Err(e) => {
            return DispatchOutcome::Failed(format!(
                "{}: failed to subscribe to {}: {e}",
                crate::profile::log_tag(),
                approval::APPROVAL_REQUEST_TOPIC
            ));
        }
    };

    let forward = json!({
        "type": "tool_execute_request",
        "call_id": call_id,
        "tool_name": tool_name,
        "arguments": arguments,
    });
    if let Err(e) = ipc::publish_json(&route_topic, &forward) {
        return DispatchOutcome::Failed(format!(
            "{}: failed to publish {route_topic}: {e}",
            crate::profile::log_tag()
        ));
    }

    // Drain both subscriptions in lockstep slices until a matching result
    // arrives, an approval surfaces, or the window closes. Each `recv` is
    // bounded by the slice; we poll the approval sub non-blocking between
    // result slices so an approval published while we're parked on the
    // result `recv` is still seen within one slice.
    let mut remaining = EXECUTE_TIMEOUT_MS;
    while remaining > 0 {
        let step = remaining.min(EXECUTE_SLICE_MS);

        // Check the approval topic first (non-blocking). Both a capability
        // approval AND a kernel grant-gate miss ride the same already-subscribed
        // `astrid.v1.approval` topic, so ONE drain of the subscription must
        // serve both: a `poll()` consumes its batch, so two separate polls
        // would let the first discard a signal the second was looking for.
        // [`poll_signal`] inspects each message once and prefers a
        // `grant_required` (the kernel DROPPED the call — no result will ever
        // come) over an `approval_required` (the tool is parked). Either way the
        // result topic can make no further progress, so we must not keep
        // blocking on it.
        match poll_signal(&approval_sub) {
            Some(DispatchOutcome::GrantRequired(grant)) => {
                return DispatchOutcome::GrantRequired(grant);
            }
            Some(DispatchOutcome::ApprovalRequired(req)) => {
                return DispatchOutcome::ApprovalRequired(req);
            }
            _ => {}
        }

        match result_sub.recv(step) {
            Ok(poll) => {
                for msg in poll.messages {
                    if let Some((content, is_error)) = match_result(&msg.payload, call_id) {
                        return DispatchOutcome::Result(content, is_error);
                    }
                }
            }
            Err(_) => {
                // Result `recv` timed out for this slice; loop will re-check
                // the approval sub and continue until the budget closes.
            }
        }

        // One more combined check after the result slice — covers a signal
        // that landed during the blocking `recv` above. Same grant-first
        // single-drain rationale as the pre-`recv` check.
        match poll_signal(&approval_sub) {
            Some(DispatchOutcome::GrantRequired(grant)) => {
                return DispatchOutcome::GrantRequired(grant);
            }
            Some(DispatchOutcome::ApprovalRequired(req)) => {
                return DispatchOutcome::ApprovalRequired(req);
            }
            _ => {}
        }

        remaining = remaining.saturating_sub(step);
    }

    DispatchOutcome::Failed(format!(
        "{}: tool '{tool_name}' did not respond within {}s",
        crate::profile::log_tag(),
        EXECUTE_TIMEOUT_MS / 1_000
    ))
}

/// Non-blocking single-drain poll of the shared `astrid.v1.approval`
/// subscription for EITHER signal the broker must surface, returning the
/// matching [`DispatchOutcome`] variant or `None`.
///
/// Both an `approval_required` (a capability-gated tool parked on
/// `request_approval`) and a `grant_required` (the kernel access gate refused
/// and DROPPED the call) are published on this one topic. `sub.poll()`
/// consumes its batch, so the two must be checked in a SINGLE pass — two
/// separate polls could let the first discard a signal the second wanted.
/// A `grant_required` is preferred when both appear in the batch: a dropped
/// call has no result and nothing parked, so surfacing it promptly avoids
/// burning the drain window. The variant is mapped here so the caller's match
/// stays uniform; this never returns [`DispatchOutcome::Result`] /
/// [`DispatchOutcome::Failed`].
///
/// `astrid.v1.approval` is a single global broadcast topic carrying no
/// `call_id` / `tool_name`. Correctness here rests on the engine serialising
/// guest calls per capsule instance behind the store mutex: this dispatch
/// holds that lock for its whole drain, so no other aos-mcp `handle_mcp_call`
/// can be watching the topic concurrently. The only signal we can observe
/// during our window is the one OUR OWN routed tool raised — see the
/// "Concurrency / correlation" note in [`crate::approval`]. The decision is
/// independently routed by `request_id` to the host's per-request topic, so
/// the surfaced signal is always tied to exactly the tool that raised it.
///
/// Skips any payload on the shared topic that is neither envelope (other
/// `IpcPayload` variants could in principle share it) and any that fails to
/// deserialize.
fn poll_signal(sub: &ipc::Subscription) -> Option<DispatchOutcome> {
    let poll = sub.poll().ok()?;
    let mut approval: Option<ApprovalRequired> = None;
    for msg in poll.messages {
        let Ok(value) = serde_json::from_str::<Value>(&msg.payload) else {
            continue;
        };
        // Grant wins: return the instant we see one, even if an approval was
        // already buffered this batch.
        if approval::is_grant_required(&value)
            && let Ok(grant) = serde_json::from_value::<GrantRequired>(value.clone())
        {
            return Some(DispatchOutcome::GrantRequired(grant));
        }
        // Remember the first well-formed approval but keep scanning for a
        // grant later in the same batch.
        if approval.is_none()
            && approval::is_approval_required(&value)
            && let Ok(req) = serde_json::from_value::<ApprovalRequired>(value)
        {
            approval = Some(req);
        }
    }
    approval.map(DispatchOutcome::ApprovalRequired)
}

/// Match a `tool.v1.execute.<name>.result` payload against `call_id`,
/// returning `(content, is_error)` when it is the result for this call.
///
/// Used by [`dispatch_with_approval`]'s drain loop. `pub(crate)` so the
/// approval bridge ([`crate::approval`]) reuses the exact same parser when
/// it drains the resumed/denied result after a decision — one definition,
/// no wire-shape drift between the two result legs.
pub(crate) fn match_result(payload: &str, call_id: &str) -> Option<(Value, bool)> {
    let value = serde_json::from_str::<Value>(payload).ok()?;
    if value.get("call_id").and_then(Value::as_str) != Some(call_id) {
        return None;
    }
    let result_obj = value.get("result");
    let content = result_obj
        .and_then(|r| r.get("content"))
        .cloned()
        .unwrap_or(Value::String(String::new()));
    let is_error = result_obj
        .and_then(|r| r.get("is_error"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Some((content, is_error))
}

/// KV key recording consent to trust an ingress `source_id`. Split out so
/// the prefix is applied in exactly one place and both the read
/// ([`is_ingress_trusted`]) and the write
/// ([`crate::approval::handle_mcp_ingress_respond`]) agree on the key shape.
///
/// Returns `None` for an empty `source_id` — an unattributed caller must
/// never resolve to a routable trust key (which, with an empty suffix,
/// would collapse to the bare prefix and risk a spurious match / write).
pub(crate) fn ingress_trust_key(source_id: &str) -> Option<String> {
    if source_id.is_empty() {
        return None;
    }
    Some(format!("{INGRESS_TRUST_KEY_PREFIX}{source_id}"))
}

/// KV key marking an outstanding consent prompt for `source_id`. Same
/// empty-source guard as [`ingress_trust_key`] — an unattributed caller must
/// never resolve to a routable pending key.
fn ingress_pending_key(source_id: &str) -> Option<String> {
    if source_id.is_empty() {
        return None;
    }
    Some(format!("{INGRESS_PENDING_KEY_PREFIX}{source_id}"))
}

/// Record that the broker has surfaced an `ingress_approval_required` prompt
/// for `source_id`, so a later `ingress.respond` can be correlated to a prompt
/// the broker actually issued ([`take_ingress_pending`]).
///
/// Best-effort: a write failure is logged, not fatal. If the marker is lost
/// the subsequent accept simply fails the correlation check and the user is
/// re-prompted on the next call — fail-secure, never a spurious grant.
///
/// The keyspace is bounded by the set of installed capsules (a `source_id` is
/// a kernel-stamped capsule UUID), and every marker is consumed by the paired
/// respond, so no TTL/sweep is needed.
pub(crate) fn mark_ingress_pending(source_id: &str) {
    let Some(key) = ingress_pending_key(source_id) else {
        log::warn(format!(
            "{}: empty source_id; not marking an ingress consent prompt as pending",
            crate::profile::log_tag()
        ));
        return;
    };
    if let Err(e) = kv::set_bytes(&key, b"1") {
        log::warn(format!(
            "{}: failed to mark ingress consent prompt pending for source_id \
             '{source_id}': {e}",
            crate::profile::log_tag()
        ));
    }
}

/// Consume the outstanding consent-prompt marker for `source_id`, returning
/// whether one existed.
///
/// This is the req-correlation gate: [`crate::approval::handle_mcp_ingress_respond`]
/// only honours an `accept` when this returns `true`, so an UNSOLICITED or
/// replayed `ingress.respond` (one for which the broker never issued a prompt)
/// cannot prime trust. The marker is deleted on consume — both on accept and
/// on decline — so it is single-use and a decline does not leave a stale
/// marker a later unsolicited accept could ride.
///
/// Fail-closed: an empty `source_id`, a missing marker, or a host read error
/// all return `false`. A delete failure after a confirmed-present marker is
/// logged but still reported as consumed (the grant proceeds; the worst case
/// is one re-usable stale marker, never a denied legitimate grant).
pub(crate) fn take_ingress_pending(source_id: &str) -> bool {
    let Some(key) = ingress_pending_key(source_id) else {
        return false;
    };
    match kv::get_bytes_opt(&key) {
        Ok(Some(_)) => {
            if let Err(e) = kv::delete(&key) {
                log::warn(format!(
                    "{}: failed to clear ingress pending marker for source_id \
                     '{source_id}': {e}",
                    crate::profile::log_tag()
                ));
            }
            true
        }
        Ok(None) => false,
        Err(e) => {
            log::warn(format!(
                "{}: ingress pending read error for source_id '{source_id}', \
                 failing closed: {e}",
                crate::profile::log_tag()
            ));
            false
        }
    }
}

/// KV key marking an outstanding capsule-grant consent prompt for a
/// `(principal, capsule_id)` pair. KV is per-principal-scoped by the kernel,
/// so the capsule id alone disambiguates within the keyspace — the principal
/// is implicit in the storage scope, not the key suffix. Same empty-suffix
/// guard as [`ingress_pending_key`]: an empty `capsule_id` must never resolve
/// to a routable key (it would collapse to the bare prefix and let one marker
/// dedup every grant prompt for the principal). The `principal` is accepted
/// for symmetry with the call sites and to keep the dedup key intent explicit,
/// but is NOT stamped into the suffix (the KV scope already carries it).
fn grant_pending_key(_principal: &str, capsule_id: &str) -> Option<String> {
    if capsule_id.is_empty() {
        return None;
    }
    Some(format!("{GRANT_PENDING_KEY_PREFIX}{capsule_id}"))
}

/// Record that the broker has surfaced a `grant_required` prompt for
/// `(principal, capsule_id)`, so a duplicate ungranted call for the same pair
/// while one is pending is suppressed instead of spawning a second prompt, and
/// so [`take_grant_pending`] can clear it on respond.
///
/// The marker VALUE is the wall-clock ms it was written: [`grant_pending`]
/// treats a marker older than [`GRANT_PENDING_TTL_MS`] as stale and ignores it,
/// so the dedup self-heals even when the paired respond never clears it (a shim
/// crash, or a respond that arrives without the `capsule_id` the clear keys on).
/// If the clock read fails the marker is written as `0` — already stale — so the
/// failure degrades to "no dedup" (one extra prompt), never a stuck marker.
///
/// Best-effort: a write failure is logged, not fatal. A lost marker just means
/// a duplicate prompt could surface (one extra elicit), never a spurious grant
/// — the grant itself is still gated on a human approve flowing through
/// [`crate::approval::handle_mcp_grant_respond`].
pub(crate) fn mark_grant_pending(principal: &str, capsule_id: &str) {
    let Some(key) = grant_pending_key(principal, capsule_id) else {
        log::warn(format!(
            "{}: empty capsule_id; not marking a grant consent prompt as pending",
            crate::profile::log_tag()
        ));
        return;
    };
    let stamp = crate::discovery::wall_ms().to_string();
    if let Err(e) = kv::set_bytes(&key, stamp.as_bytes()) {
        log::warn(format!(
            "{}: failed to mark grant consent prompt pending for capsule_id \
             '{capsule_id}': {e}",
            crate::profile::log_tag()
        ));
    }
}

/// Whether a grant-pending marker value (the wall-clock ms it was written, per
/// [`crate::discovery::wall_ms`]) is still within [`GRANT_PENDING_TTL_MS`].
fn marker_is_fresh(value: &[u8]) -> bool {
    marker_is_fresh_at(value, crate::discovery::wall_ms())
}

/// Core of [`marker_is_fresh`] with the clock injected, so the freshness logic
/// is unit-testable without a host (mirrors [`crate::cache`]'s `is_fresh`).
///
/// Reads as NOT fresh — fail toward re-prompting (surface a fresh consent
/// prompt), never toward a stuck marker — when: the value does not parse; the
/// stored stamp is `0` (the mark-time clock was unavailable); `now` is `0` (the
/// clock is unavailable now); or the stamp is in the FUTURE relative to `now`
/// (`written > now`). A future stamp means the wall clock stepped backward (NTP
/// correction) or the stored value is corrupt/forged; reading it as fresh would
/// suppress prompts far beyond the TTL (until the clock caught back up), which
/// defeats the self-heal, so it too fails open to a re-prompt. With those guards
/// the freshness window is exactly `[written, written + TTL)` and `now - written`
/// can never underflow.
fn marker_is_fresh_at(value: &[u8], now: u64) -> bool {
    if now == 0 {
        return false;
    }
    let Ok(written) = std::str::from_utf8(value)
        .map(str::trim)
        .unwrap_or("")
        .parse::<u64>()
    else {
        return false;
    };
    if written == 0 || written > now {
        return false;
    }
    now - written < GRANT_PENDING_TTL_MS
}

/// Returns whether a grant-consent prompt is already outstanding for
/// `(principal, capsule_id)` WITHOUT consuming the marker.
///
/// Used at the broker's `grant_required` surface point: if a prompt is already
/// pending for this pair, the broker replies a benign "already pending"
/// terminal result instead of a fresh elicit (dedup). The marker is left in
/// place — it is consumed only by [`take_grant_pending`] on the respond, so the
/// dedup holds across every duplicate call until the user decides.
///
/// Fail-OPEN on a read error (returns `false` → the broker surfaces a fresh
/// prompt): a transient KV read failure must never SUPPRESS a consent prompt,
/// or an ungranted call could be silently swallowed with no way for the user to
/// approve it. The worst case is a duplicate prompt, never a swallowed call.
pub(crate) fn grant_pending(principal: &str, capsule_id: &str) -> bool {
    let Some(key) = grant_pending_key(principal, capsule_id) else {
        return false;
    };
    match kv::get_bytes_opt(&key) {
        Ok(Some(bytes)) => {
            if marker_is_fresh(&bytes) {
                true
            } else {
                // Stale (or unparseable / clock-unavailable) marker: the paired
                // respond never cleared it. Treat as not pending and best-effort
                // delete so the next ungranted call re-prompts — the self-heal
                // that keeps a dropped respond from wedging the pair forever.
                let _ = kv::delete(&key);
                false
            }
        }
        Ok(None) => false,
        Err(e) => {
            log::warn(format!(
                "{}: grant pending read error for capsule_id '{capsule_id}', \
                 surfacing a fresh prompt: {e}",
                crate::profile::log_tag()
            ));
            false
        }
    }
}

/// Consume the outstanding grant-consent prompt marker for
/// `(principal, capsule_id)`, returning whether one existed.
///
/// Called by [`crate::approval::handle_mcp_grant_respond`] on BOTH approve and
/// deny so the marker is single-use and can never stick: a declined prompt must
/// not leave a marker that suppresses every future grant prompt for the pair.
/// The return value is informational (the grant itself is driven by the
/// published decision, not this marker); clearing the marker is the effect that
/// matters here.
///
/// Fail-closed shape mirrors [`take_ingress_pending`]: an empty `capsule_id`, a
/// missing marker, or a read error all return `false`. A delete failure after a
/// confirmed-present marker is logged but reported as consumed.
pub(crate) fn take_grant_pending(principal: &str, capsule_id: &str) -> bool {
    let Some(key) = grant_pending_key(principal, capsule_id) else {
        return false;
    };
    match kv::get_bytes_opt(&key) {
        Ok(Some(_)) => {
            if let Err(e) = kv::delete(&key) {
                log::warn(format!(
                    "{}: failed to clear grant pending marker for capsule_id \
                     '{capsule_id}': {e}",
                    crate::profile::log_tag()
                ));
            }
            true
        }
        Ok(None) => false,
        Err(e) => {
            log::warn(format!(
                "{}: grant pending read error for capsule_id '{capsule_id}', \
                 failing closed: {e}",
                crate::profile::log_tag()
            ));
            false
        }
    }
}

/// Confused-deputy guard for state-mutating broker calls.
///
/// `source_id` is the kernel-set UUID of the capsule that originated the
/// inbound IPC message ([`astrid_sdk::runtime::caller`] →
/// `CallerContext::source_id`). It is NOT guest-settable — the kernel
/// stamps it from the publishing capsule's invocation context, so a
/// malicious guest cannot forge it the way it could forge a body field.
/// An ingress is trusted iff the per-(principal, source_id) KV key
/// `mcp.ingress.trust.<source_id>` exists — written ONLY by
/// [`crate::approval::handle_mcp_ingress_respond`] after the user
/// interactively consents via the shim's elicit prompt. There is no
/// operator-maintained allow-list and nothing a capsule computes; trust is
/// recorded purely from a human accept, keyed on the kernel-stamped
/// originating identity.
///
/// KV is scoped per-principal (and per-capsule) by the kernel, so this is
/// naturally per-(principal, source_id) — consent granted under one
/// principal does not leak to another. Fails CLOSED: an empty source_id, a
/// missing key, or a host read error all return `false`.
///
/// ## Why `principal.verified()` is insufficient here
///
/// The broker surface (`astrid.v1.request.mcp.tool.call`) is reached
/// through the cli proxy, which forwards client traffic with a plain
/// [`astrid_sdk::ipc::publish`] (see `capsule-cli`'s ingress path) — NOT
/// `publish_as`. The host therefore attributes the principal as
/// `Verified(<proxy's own invocation principal>)`: the host NEVER emits
/// `Claimed` on this path (that variant only appears behind `publish_as`,
/// which the proxy does not use for tool calls), and the proxy stamps
/// the default verified attribution. So `verified()` returning `Some`
/// proves only "*some* capsule published this in a verified invocation
/// context" — it does NOT identify *which* capsule, and every sibling
/// capsule on the bus would equally satisfy it. The confused-deputy
/// question is "did a TRUSTED ingress forward this?", which only
/// `source_id` (the originating capsule's identity) answers. We keep the
/// kernel-resolved principal for downstream capability checks but gate
/// admission on `source_id`, not trust marker.
pub(crate) fn is_ingress_trusted(source_id: &str) -> bool {
    let Some(key) = ingress_trust_key(source_id) else {
        return false;
    };
    // Present key (any value) → trusted; missing → not; host error → fail
    // closed.
    matches!(kv::get_bytes_opt(&key), Ok(Some(_)))
}

/// Tool-name charset gate. Same rule as the discovery validator —
/// non-empty, length-capped, ASCII alphanumeric plus `_ . -`. See
/// [`crate::discovery::is_valid_name`] for the source of the rule.
///
/// `pub(crate)` so the approval bridge ([`crate::approval`]) applies the
/// exact same gate to the `tool_name` the shim echoes back before it builds
/// the `tool.v1.execute.<name>.result` topic to drain — one definition, no
/// drift between the dispatch and resume legs.
pub(crate) fn is_valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_TOOL_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

#[cfg(test)]
mod tests {
    fn install_test_profile() {
        crate::profile::install_aos();
    }

    use super::*;

    #[test]
    fn tool_name_charset_rejects_path_traversal() {
        install_test_profile();
        assert!(is_valid_tool_name("read_file"));
        assert!(is_valid_tool_name("fs.read"));
        assert!(is_valid_tool_name("a-b-c"));
        assert!(!is_valid_tool_name(""));
        assert!(!is_valid_tool_name("foo/bar"));
        assert!(!is_valid_tool_name("foo bar"));
        assert!(!is_valid_tool_name("foo\nbar"));
        assert!(!is_valid_tool_name("foo*"));
    }

    #[test]
    fn tool_name_length_capped() {
        install_test_profile();
        let ok = "a".repeat(MAX_TOOL_NAME_LEN);
        let too_long = "a".repeat(MAX_TOOL_NAME_LEN + 1);
        assert!(is_valid_tool_name(&ok));
        assert!(!is_valid_tool_name(&too_long));
    }

    #[test]
    fn ingress_trust_key_applies_prefix() {
        install_test_profile();
        let id = "0191f3a2-b4c7-4d8e-9f01-234567890abc";
        assert_eq!(
            ingress_trust_key(id).as_deref(),
            Some("mcp.ingress.trust.0191f3a2-b4c7-4d8e-9f01-234567890abc")
        );
    }

    #[test]
    fn ingress_trust_key_rejects_empty_source() {
        install_test_profile();
        // An unattributed (empty) source_id must never resolve to a routable
        // trust key — otherwise the read would collapse to the bare prefix
        // and a write under an empty caller would grant blanket trust.
        assert_eq!(ingress_trust_key(""), None);
    }

    #[test]
    fn ingress_pending_key_applies_prefix() {
        install_test_profile();
        let id = "0191f3a2-b4c7-4d8e-9f01-234567890abc";
        assert_eq!(
            ingress_pending_key(id).as_deref(),
            Some("mcp.ingress.pending.0191f3a2-b4c7-4d8e-9f01-234567890abc")
        );
    }

    #[test]
    fn ingress_pending_key_rejects_empty_source() {
        install_test_profile();
        // Mirror the trust-key guard: an unattributed (empty) source must not
        // resolve to a routable pending key, or `mark`/`take` would collapse
        // to the bare prefix and a single marker would correlate every
        // unattributed respond.
        assert_eq!(ingress_pending_key(""), None);
    }

    #[test]
    fn take_ingress_pending_empty_source_fails_closed() {
        install_test_profile();
        // No host KV call is reached for an empty source_id — it short-circuits
        // to `false` via `ingress_pending_key` returning `None`, so an
        // unattributed accept can never satisfy the correlation gate. (The
        // non-empty path needs a live host and is exercised by integration.)
        assert!(!take_ingress_pending(""));
    }

    #[test]
    fn is_ingress_trusted_empty_source_fails_closed() {
        install_test_profile();
        // No host KV call is reached for an empty source_id — it short-circuits
        // to `false` via `ingress_trust_key` returning `None`. (The non-empty
        // path needs a live host and is exercised by integration, not here.)
        assert!(!is_ingress_trusted(""));
    }

    #[test]
    fn grant_pending_key_applies_prefix_on_capsule_only() {
        install_test_profile();
        // The key suffix is the capsule id alone — the principal is implicit
        // in the per-principal KV scope, never stamped into the suffix. Two
        // different principals naturally get separate markers via the scope,
        // so the same capsule id under each maps to the same suffix without
        // colliding across principals.
        let cap = "fs";
        assert_eq!(
            grant_pending_key("alice", cap).as_deref(),
            Some("mcp.grant.pending.fs")
        );
        // The principal does not change the key — proves it is scope-implicit,
        // not suffix-encoded.
        assert_eq!(
            grant_pending_key("bob", cap),
            grant_pending_key("alice", cap)
        );
    }

    #[test]
    fn grant_pending_key_rejects_empty_capsule() {
        install_test_profile();
        // Mirror the ingress-pending guard: an empty capsule_id must not
        // resolve to a routable key, or one marker would dedup every grant
        // prompt for the principal.
        assert_eq!(grant_pending_key("alice", ""), None);
    }

    #[test]
    fn grant_pending_empty_capsule_is_not_pending() {
        install_test_profile();
        // No host KV call is reached for an empty capsule_id — it short-circuits
        // to `false` via `grant_pending_key` returning `None`, so the broker
        // surfaces a fresh prompt rather than spuriously deduping. (The
        // non-empty path needs a live host and is exercised by integration.)
        assert!(!grant_pending("alice", ""));
    }

    #[test]
    fn take_grant_pending_empty_capsule_fails_closed() {
        install_test_profile();
        // No host KV call is reached for an empty capsule_id — short-circuits
        // to `false` via `grant_pending_key` returning `None`. (The non-empty
        // path needs a live host and is exercised by integration, not here.)
        assert!(!take_grant_pending("alice", ""));
    }

    #[test]
    fn marker_fresh_within_ttl() {
        install_test_profile();
        // A marker written `TTL - 1ms` ago is still within the window → fresh,
        // so the dedup holds and a duplicate call is suppressed.
        let now = 10_000_000;
        let written = now - (GRANT_PENDING_TTL_MS - 1);
        assert!(marker_is_fresh_at(written.to_string().as_bytes(), now));
    }

    #[test]
    fn marker_stale_at_or_past_ttl_self_heals() {
        install_test_profile();
        // At exactly the TTL and beyond, the marker is stale → not fresh, so
        // `grant_pending` ignores it and the next call re-prompts. This is the
        // self-heal that keeps a never-cleared marker (shim crash, or a respond
        // without `capsule_id`) from suppressing prompts forever.
        let now = 10_000_000;
        let at_ttl = now - GRANT_PENDING_TTL_MS;
        let past_ttl = now - (GRANT_PENDING_TTL_MS + 60_000);
        assert!(!marker_is_fresh_at(at_ttl.to_string().as_bytes(), now));
        assert!(!marker_is_fresh_at(past_ttl.to_string().as_bytes(), now));
    }

    #[test]
    fn marker_not_fresh_when_clock_unavailable_now_or_at_write() {
        install_test_profile();
        // `wall_ms() == 0` means the host clock is unavailable. Either a now of 0
        // or a stored stamp of 0 reads as NOT fresh — fail toward re-prompting,
        // never toward trusting an unageable marker.
        assert!(!marker_is_fresh_at(b"10000000", 0));
        assert!(!marker_is_fresh_at(b"0", 10_000_000));
    }

    #[test]
    fn marker_not_fresh_when_unparseable() {
        install_test_profile();
        // A non-numeric value (e.g. a legacy `b"1"` presence marker, or garbage)
        // cannot be aged → treated as not fresh so it cannot linger.
        assert!(!marker_is_fresh_at(b"1", 10_000_000));
        assert!(!marker_is_fresh_at(b"not-a-number", 10_000_000));
        assert!(!marker_is_fresh_at(b"", 10_000_000));
    }

    #[test]
    fn marker_future_stamp_is_stale() {
        install_test_profile();
        // A stamp dated in the FUTURE relative to `now` (the wall clock stepped
        // backward, or the stored value is corrupt/forged) must read as STALE —
        // fail open to a re-prompt — never as fresh. Reading it as fresh would
        // suppress prompts until the clock caught back up (potentially far beyond
        // the TTL), defeating the self-heal. The cost of failing open is at most
        // one extra prompt, never indefinite suppression.
        assert!(!marker_is_fresh_at(
            20_000_000u64.to_string().as_bytes(),
            10_000_000
        ));
    }
}
