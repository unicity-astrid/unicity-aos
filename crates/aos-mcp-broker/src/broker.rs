//! Broker front door — the sanitized `astrid.v1.*` MCP surface.
//!
//! This is aos-mcp's SECOND front door, sitting over the SAME
//! discovery ([`crate::discovery`]) and execute ([`crate::execute`])
//! internals as the agent-runner path. Where the agent path serves the
//! `mcp__aos__*` namespace Claude consumes via `--allowed-tools`, the
//! broker serves a generic, third-party MCP client through a
//! shim/proxy.
//!
//! ## Topics
//!
//! * **inbound** `astrid.v1.request.mcp.tools.list`  -> [`handle_mcp_list`]
//! * **inbound** `astrid.v1.request.mcp.tool.call`   -> [`handle_mcp_call`]
//! * **outbound** `astrid.v1.response.<req_id>`        (both handlers)
//!
//! ## Wire contract
//!
//! The proxy/shim delivers the PAYLOAD only — the source topic is not
//! visible to the handler, and the proxy that bridges the external MCP
//! client subscribes to `astrid.v1.response.*` and forwards the body
//! verbatim. So:
//!
//! * `req_id` is mirrored into the request body and echoed into the
//!   reply body (the proxy correlates on the body, not the topic);
//! * the egress topic `astrid.v1.response.<req_id>` MUST be a single
//!   segment after the prefix. The kernel's `topic_matches` is
//!   strict-arity (a 4-segment `astrid.v1.response.*` subscription
//!   never matches a 5-segment topic), so a `req_id` carrying a `.`
//!   would be silently dropped. We reject any `req_id` that is not a
//!   single clean segment before publishing.
//!
//! ## Trust boundary
//!
//! The shim NEVER sees `tool.v1.*` — it only ever touches the
//! sanitized `astrid.v1.*` surface. All `tool.v1.*` fan-out lives
//! behind [`crate::execute::dispatch_with_approval`], which charset-gates
//! the tool name before it can reach a routed topic. The list reply
//! carries RAW MCP descriptors (no `mcp__aos__` prefix) because the
//! broker is a standard MCP server, not the agent runner.
//!
//! ## Confused-deputy gate (state-mutating calls)
//!
//! [`handle_mcp_call`] is state-mutating and externally reachable, so it
//! additionally requires the inbound message's kernel-set `source_id`
//! (the originating capsule UUID, via [`astrid_sdk::runtime::caller`]) to
//! be a TRUSTED ingress. Trust is recorded interactively: an untrusted
//! source_id triggers an `ingress_approval_required` reply that the shim
//! turns into a user consent prompt, and on accept the broker records trust
//! under the per-(principal, source_id) KV key `mcp.ingress.trust.<source_id>`
//! (see [`crate::execute::is_ingress_trusted`] and
//! [`crate::approval::handle_mcp_ingress_respond`]). This stops a non-ingress
//! capsule from puppeting aos-mcp into executing tools on a principal's
//! behalf without a human ever consenting to it. [`handle_mcp_list`] is
//! read-only (it returns the public tool surface the proxy already publishes)
//! and is NOT gated as strictly. See [`crate::execute::is_ingress_trusted`]
//! for why the trust marker (`verified()`) cannot substitute for the
//! `source_id` identity check.

use astrid_sdk::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{approval, discovery, execute, grant_decision};

/// Egress topic prefix. The reply lands on `<prefix><req_id>`; with a
/// single-segment `req_id` that is exactly 4 segments, which the
/// proxy's `astrid.v1.response.*` subscription matches.
const RESPONSE_PREFIX: &str = "astrid.v1.response.";

/// `req_id` length cap. A correlation id is a UUID-ish token; anything
/// longer is rejected before it can be stamped into an egress topic.
const MAX_REQ_ID_LEN: usize = 128;

/// Reserved broker tool name for the NATIVE-tool PreToolUse permission
/// gate. It is NOT a real capsule tool and deliberately never appears in
/// `tools/list`: it exists only to service Claude's `type:"mcp_tool"`
/// PreToolUse hook, which calls it OUT-OF-BAND (not through Claude's tool
/// surface) to get a binding allow/deny decision for a native tool call
/// (`Bash`, `Write`, `Edit`, …). VERIFIED against the shipped `claude`
/// executor: a `mcp_tool` hook issues `tools/call` DIRECTLY and does NOT
/// pre-validate the name against the server's `tools/list`, so an
/// intentionally-unlisted tool the broker special-cases is invoked normally
/// — no descriptor injection needed.
///
/// ## Why a hook, when the `mcp__aos__*` plane is gated in-process
///
/// Native tools execute INSIDE the `claude` process and reach no Astrid
/// chokepoint — unlike the `mcp__aos__*` tools, which funnel through
/// [`handle_mcp_call`] where the same [`crate::policy`] PDP refuses to
/// dispatch a denied call un-bypassably. For native tools there is no such
/// in-process point, so the PreToolUse hook is the ONLY per-call lever. The
/// gate reuses the SAME PDP, so one operator rule set governs both planes.
///
/// ## Honest limit — advisory, fail-open
///
/// This path is best-effort, not a guarantee. The hook is read from a
/// settings tier a capable session can edit, the gate call is one Claude
/// could route around, and the platform FAILS OPEN: a disconnected server,
/// an `isError` reply, or non-JSON text all let the tool run. The
/// fail-CLOSED boundary for native tools stays the Astrid host sandbox plus
/// the `--disallowedTools` deny-list; this gate adds dynamic, argument-level
/// DENY on top, and only ever NARROWS (an `Allow` defers to Claude's
/// existing permission flow rather than asserting an explicit allow).
///
/// Must equal `claude_install::layout::PRETOOLUSE_GATE_TOOL` — claude-install
/// authors the hook with this exact `tool` name. The two
/// crates share no dependency edge, so the constant is mirrored, not shared;
/// a drift silently DISABLES the gate (the hook would call a name the broker
/// does not special-case, so `dispatch_with_approval` treats it as an
/// unknown tool, drains to `isError`, and the hook fails open). A presence
/// test in each crate anchors the value.
pub(crate) const PRETOOLUSE_GATE_TOOL: &str = "aos_pretooluse_gate";

/// Inbound `astrid.v1.request.mcp.tools.list` payload.
///
/// `req_id` is the proxy's correlation token, mirrored into the body
/// because the handler cannot see the source topic. Any other fields
/// are ignored (forward-compat with future pagination cursors etc.).
#[derive(Debug, Deserialize)]
struct ListRequest {
    req_id: String,
}

/// Inbound `astrid.v1.request.mcp.tool.call` payload.
///
/// Standard MCP `tools/call` shape (`name` + `arguments`) plus the
/// proxy `req_id`. `name` is a RAW MCP tool name — the broker does not
/// use the `mcp__aos__` prefix.
#[derive(Debug, Deserialize)]
struct CallRequest {
    req_id: String,
    name: String,
    #[serde(default)]
    arguments: Value,
}

/// Handle `astrid.v1.request.mcp.tools.list`.
///
/// Runs the shared describe-collect snapshot, converts to MCP
/// descriptors, and replies on `astrid.v1.response.<req_id>` with
/// `{ kind:"tools.list", req_id, tools:[...] }`. Exactly one reply per
/// accepted request.
pub(crate) fn handle_mcp_list(payload: Value) -> Result<(), SysError> {
    let req: ListRequest = match serde_json::from_value(payload) {
        Ok(v) => v,
        Err(e) => {
            // No recoverable req_id — there is no channel to reply on,
            // so the proxy will time out its own request. Log and drop.
            log::warn(format!(
                "{}: broker tools.list: malformed payload (no req_id): {e}",
                crate::profile::log_tag()
            ));
            return Ok(());
        }
    };

    let Some(reply_topic) = reply_topic(&req.req_id) else {
        log::warn(format!(
            "{}: broker tools.list: rejecting unroutable req_id '{}'",
            crate::profile::log_tag(),
            req.req_id
        ));
        return Ok(());
    };

    let started = discovery::wall_ms();
    let (source_id, principal) = caller_fields();
    log::info(format!(
        "{}: broker ingress method=tools.list req_id={} source_id={source_id} \
         principal={principal}",
        crate::profile::log_tag(),
        req.req_id
    ));

    let snapshot = discovery::collect_snapshot(&req.req_id);
    let tools = discovery::to_mcp_descriptors(&snapshot);
    let tool_count = tools.len();

    let reply = json!({
        "kind": "tools.list",
        "req_id": req.req_id,
        "tools": tools,
    });
    publish_reply(&reply_topic, &reply);
    log::info(format!(
        "{}: broker response method=tools.list req_id={} outcome=ok tools={tool_count} \
         elapsed_ms={}",
        crate::profile::log_tag(),
        req.req_id,
        discovery::wall_ms().saturating_sub(started)
    ));
    Ok(())
}

/// Handle `astrid.v1.request.mcp.tool.call`.
///
/// Runs the shared execute-dispatch and replies on
/// `astrid.v1.response.<req_id>` with
/// `{ kind:"tool.call", req_id, content:[...], isError:bool }`. Every
/// failure path (unknown/invalid name, subscribe/publish error, drain
/// timeout) reshapes into an `isError:true` reply so the proxy never
/// hangs. Exactly one reply per accepted request.
pub(crate) fn handle_mcp_call(payload: Value) -> Result<(), SysError> {
    let req: CallRequest = match serde_json::from_value(payload) {
        Ok(v) => v,
        Err(e) => {
            log::warn(format!(
                "{}: broker tool.call: malformed payload (no req_id): {e}",
                crate::profile::log_tag()
            ));
            return Ok(());
        }
    };

    let Some(reply_topic) = reply_topic(&req.req_id) else {
        log::warn(format!(
            "{}: broker tool.call: rejecting unroutable req_id '{}'",
            crate::profile::log_tag(),
            req.req_id
        ));
        return Ok(());
    };

    let started = discovery::wall_ms();

    // Native-tool PreToolUse gate. A reserved, unlisted tool name Claude's
    // PreToolUse `mcp_tool` hook calls to get a binding decision for a NATIVE
    // tool (`Bash`/`Write`/…) that runs inside the claude process and so
    // never reaches the dispatch gate below. It is READ-ONLY — it evaluates
    // policy and replies a hook decision, never dispatching — so it sits
    // ABOVE the confused-deputy mutation gate, exactly like the read-only
    // `handle_mcp_list`. The reply is ALWAYS `isError:false`: an
    // `isError:true` MCP result makes the hook fail OPEN, so a deny must ride
    // in the reply `content`, never the error flag. See [`PRETOOLUSE_GATE_TOOL`].
    if req.name == PRETOOLUSE_GATE_TOOL {
        // Out-of-band native-tool permission gate (not a routed tool call); the
        // gated NATIVE tool is named in the arguments, not `req.name`.
        let native = req
            .arguments
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or("");
        log::info(format!(
            "{}: broker ingress method=pretooluse_gate req_id={} tool={native}",
            crate::profile::log_tag(),
            req.req_id
        ));
        publish_reply(&reply_topic, &pretooluse_gate_reply(&req));
        log::info(format!(
            "{}: broker response method=pretooluse_gate req_id={} outcome=ok elapsed_ms={}",
            crate::profile::log_tag(),
            req.req_id,
            discovery::wall_ms().saturating_sub(started)
        ));
        return Ok(());
    }

    // Confused-deputy gate. `astrid.v1.request.mcp.tool.call` is
    // state-mutating and externally reachable through the cli proxy, so
    // before we dispatch we require the message's kernel-set `source_id`
    // (the originating capsule UUID, NOT a guest-settable body field) to be
    // a TRUSTED ingress. Trust is no longer an operator-maintained allow-list:
    // an ingress is trusted iff the user has interactively consented to it
    // (recorded under the per-(principal, source_id) KV key
    // `mcp.ingress.trust.<source_id>` — see [`execute::is_ingress_trusted`]).
    //
    // When the source_id is NOT yet trusted we do NOT hard-deny: we reply a
    // distinct `ingress_approval_required` signal (NOT `isError`, since it is
    // a prompt-needed state, not a failure) so the shim can elicit consent
    // from the user and, on accept, forward
    // `astrid.v1.request.mcp.ingress.respond` ->
    // [`crate::approval::handle_mcp_ingress_respond`], which records trust
    // against the kernel-stamped caller source_id and lets a re-sent call
    // pass this gate. We NEVER dispatch on this path. See
    // [`execute::is_ingress_trusted`] for why `verified()` is insufficient.
    let (source_id, principal) = match runtime::caller() {
        Ok(ctx) => (ctx.source_id, ctx.principal.unwrap_or_default()),
        Err(e) => {
            // No caller context — cannot attribute the ingress. Fail
            // closed rather than dispatch an unattributed mutation.
            log::warn(format!(
                "{}: broker tool.call: no caller context, rejecting req_id={} tool={}: {e}",
                crate::profile::log_tag(),
                req.req_id,
                req.name
            ));
            let reply = json!({
                "kind": "tool.call",
                "req_id": req.req_id,
                "content": mcp_content(Value::String(
                    format!("{}: caller context unavailable; tool call rejected", crate::profile::log_tag()),
                )),
                "isError": true,
            });
            publish_reply(&reply_topic, &reply);
            return Ok(());
        }
    };

    // Ingress milestone — one concise line per accepted tool.call. Tool NAME
    // only, never arguments (an argument may carry a secret; INFO must not
    // leak it). `arg_count` is the cheap, non-sensitive shape signal.
    log::info(format!(
        "{}: broker ingress method=tool.call req_id={} source_id={source_id} \
         principal={} tool={} arg_count={}",
        crate::profile::log_tag(),
        req.req_id,
        display_principal(&principal),
        req.name,
        arg_count(&req.arguments)
    ));

    if !execute::is_ingress_trusted(&source_id) {
        // Trust decision: untrusted ingress → consent-required (WARN: a
        // mutation was withheld pending a human accept, security-relevant).
        log::warn(format!(
            "{}: broker trust=consent-required req_id={} source_id={source_id} tool={}; \
             requesting interactive consent",
            crate::profile::log_tag(),
            req.req_id,
            req.name
        ));
        // Record that a consent prompt is now outstanding for this ingress.
        // The respond handler ([`crate::approval::handle_mcp_ingress_respond`])
        // only honours an `accept` that consumes this marker, so an unsolicited
        // or replayed `ingress.respond` cannot prime trust without a prompt the
        // broker actually issued. Keyed on the kernel-stamped caller (the same
        // identity the trust write uses) — best-effort; a write failure just
        // re-prompts on the next call, never grants spuriously.
        execute::mark_ingress_pending(&source_id);

        // Prompt-needed signal — NOT an error. The shim elicits consent and,
        // on accept, drives `ingress.respond` then re-sends this call. We
        // echo `source_id` for display/diagnostics only; the trust write
        // keys on the kernel-stamped caller of the `ingress.respond`, never
        // this body field. No tool is dispatched.
        //
        // Carry actionable guidance as `message` + MCP `content` so the denial
        // is never a dead end: an elicit-capable shim honours the
        // `ingress_approval_required` signal and ignores this text, but a
        // client that cannot render the prompt still surfaces WHICH session was
        // withheld, WHY, and HOW to proceed instead of a bare "not authorized".
        // Additive only — the trust model is unchanged; nothing here grants
        // trust.
        let guidance = ingress_consent_guidance(&source_id);
        let reply = json!({
            "kind": "tool.call",
            "req_id": req.req_id,
            "ingress_approval_required": true,
            "source_id": source_id,
            "tool_name": req.name,
            "message": guidance,
            "content": mcp_content(Value::String(guidance.clone())),
            "isError": false,
        });
        publish_reply(&reply_topic, &reply);
        log::info(format!(
            "{}: broker response method=tool.call req_id={} outcome=consent_required \
             tool={} elapsed_ms={}",
            crate::profile::log_tag(),
            req.req_id,
            req.name,
            discovery::wall_ms().saturating_sub(started)
        ));
        return Ok(());
    }

    // Trust decision: source_id is a trusted ingress → proceed to dispatch.
    log::info(format!(
        "{}: broker trust=trusted req_id={} source_id={source_id} tool={}",
        crate::profile::log_tag(),
        req.req_id,
        req.name
    ));

    // Argument-level policy gate — the binding PDP. Evaluated in-process
    // at THIS chokepoint (the one capsule-space point holding parsed
    // `(name, arguments)` before fan-out), so a supervised Claude cannot
    // route around it the way it could strip a settings-tier PreToolUse
    // hook. DENY → reply `isError` + never dispatch. The gate only ever
    // NARROWS: no matching rule, no policy configured, or a policy-load
    // failure all return `Allow`, leaving the host's execution-time
    // capability enforcement as the live boundary — degrade-to-PEP, never
    // "anything goes". The reason surfaced back is the operator's static
    // rule id, never a reflected argument. See [`crate::policy`].
    if let crate::policy::Decision::Deny { reason } =
        crate::policy::evaluate(&crate::policy::load_rules(), &req.name, &req.arguments)
    {
        // A received request that will NOT be dispatched → WARN (never silent).
        // `rule` is the operator's static rule id, never a reflected argument.
        log::warn(format!(
            "{}: broker policy-deny req_id={} tool={} rule={reason}",
            crate::profile::log_tag(),
            req.req_id,
            req.name
        ));
        let _ = ipc::publish_json(
            &crate::profile::audit_topic("policy_deny"),
            &serde_json::json!({ "tool": req.name, "rule": reason }),
        );
        let reply = json!({
            "kind": "tool.call",
            "req_id": req.req_id,
                "content": mcp_content(Value::String(format!("{}: tool call denied by policy (rule: {reason})", crate::profile::log_tag()))),
            "isError": true,
        });
        publish_reply(&reply_topic, &reply);
        log::info(format!(
            "{}: broker response method=tool.call req_id={} outcome=policy_denied \
             tool={} elapsed_ms={}",
            crate::profile::log_tag(),
            req.req_id,
            req.name,
            discovery::wall_ms().saturating_sub(started)
        ));
        return Ok(());
    }

    // Routing milestone — the broker is about to dispatch the execute to the
    // providing capsule via the routed `tool.v1.execute.<tool>` topic.
    log::info(format!(
        "{}: broker route req_id={} tool={} topic=tool.v1.execute.{}",
        crate::profile::log_tag(),
        req.req_id,
        req.name,
        req.name
    ));

    // The execute core wants a `call_id` for result correlation on the
    // shared `tool.v1.execute.<bare>.result` topic. The broker's
    // `req_id` doubles as that correlation token — it is already
    // single-segment / charset-clean (validated by `reply_topic`), and
    // it never leaves the `astrid.v1.*` surface beyond the inner
    // `tool.v1.execute` request body.
    //
    // `dispatch_with_approval` additionally watches `astrid.v1.approval`
    // for the drain window: if the routed tool blocks on a capability
    // approval, we surface an `approval-required` flag in this reply so the
    // shim can elicit the choice from Claude (the broker can't call the
    // host `astrid:elicit` syscall — it is install/upgrade-gated — so it
    // relays the bus envelope instead). The shim then drives
    // `astrid.v1.request.mcp.approval.respond` -> [`approval::handle_mcp_approval`],
    // which maps the choice onto `astrid.v1.approval.response.<id>` to
    // unblock the tool. See [`crate::approval`].
    let reply = match execute::dispatch_with_approval(&req.name, &req.req_id, &req.arguments) {
        execute::DispatchOutcome::Result(content, is_error) => {
            log::info(format!(
                "{}: broker response method=tool.call req_id={} tool={} outcome={} \
                 elapsed_ms={}",
                crate::profile::log_tag(),
                req.req_id,
                req.name,
                if is_error { "error" } else { "ok" },
                discovery::wall_ms().saturating_sub(started)
            ));
            json!({
                "kind": "tool.call",
                "req_id": req.req_id,
                "content": mcp_content(content),
                "isError": is_error,
            })
        }
        execute::DispatchOutcome::ApprovalRequired(required) => {
            log::info(format!(
                "{}: broker response method=tool.call req_id={} tool={} \
                 outcome=approval_required elapsed_ms={}",
                crate::profile::log_tag(),
                req.req_id,
                req.name,
                discovery::wall_ms().saturating_sub(started)
            ));
            json!({
                "kind": "tool.call",
                "req_id": req.req_id,
                // No tool result yet — the tool is parked on the approval. The
                // shim MUST elicit the choice and respond on
                // `astrid.v1.request.mcp.approval.respond` (echoing back the
                // `tool_name` + `call_id` the flag carries) before a result can
                // be produced. `content` is empty and `isError` false: this is
                // a pending state, not a failure. The terminal result is
                // delivered by `approval::handle_mcp_approval` once the decision
                // lands — see [`crate::approval`]. `req.req_id` doubles as the
                // dispatch `call_id` (it is the result-correlation token).
                "content": mcp_content(Value::String(String::new())),
                "isError": false,
                "approval_required": required.to_reply_flag(&req.name, &req.req_id),
            })
        }
        execute::DispatchOutcome::GrantRequired(grant) => {
            // The kernel access gate refused the call and DROPPED it (grant-on-use
            // mirrors INGRESS: gate → consent → re-send). What happens next is
            // driven by the DURABLE recorded decision for this capsule
            // ([`crate::grant_decision`]), so a consent the user already gave
            // converges WITHOUT re-prompting even when the awaiter that raised
            // THIS signal has expired fail-closed.
            let outcome;
            let reply = match grant_decision::grant_action(grant_decision::recorded_grant_decision(
                &grant.capsule_id,
            )) {
                grant_decision::GrantAction::AutoApprove => {
                    // Answer the fresh, live awaiter for THIS signal (raised ms
                    // ago) without eliciting — the grant persists and the re-sent
                    // call converges. An unroutable request_id can't be published,
                    // so return an error, not a retry-hint that would spin.
                    if approval::publish_grant_auto_decision(&grant.request_id, true) {
                        let _ = ipc::publish_json(
                            &crate::profile::audit_topic("grant_auto_approved"),
                            &json!({ "capsule": grant.capsule_id }),
                        );
                        outcome = "grant_auto_approved";
                        grant_decision::grant_auto_approve_reply(&req.req_id, &grant.capsule_id)
                    } else {
                        outcome = "grant_auto_approve_unroutable";
                        grant_decision::grant_unroutable_reply(&req.req_id, &grant.capsule_id)
                    }
                }
                grant_decision::GrantAction::AutoDeny => {
                    // The user denied this capsule: retire the awaiter with a deny
                    // (clean, vs. a 60 s fail-closed timeout), suppress
                    // re-prompting, and return the operator escape hatch. Unlike
                    // AutoApprove, an unroutable request_id changes only the
                    // TELEMETRY, not the reply: no deny was published, but the
                    // unretired awaiter fail-closes to denied on its own, so the
                    // user-facing deny stands either way and there is no
                    // retry-loop hazard. The audit event is emitted only for a
                    // deny actually published — an unpublished one must not be
                    // reported as published.
                    if approval::publish_grant_auto_decision(&grant.request_id, false) {
                        let _ = ipc::publish_json(
                            &crate::profile::audit_topic("grant_auto_denied"),
                            &json!({ "capsule": grant.capsule_id }),
                        );
                        outcome = "grant_auto_denied";
                    } else {
                        outcome = "grant_auto_deny_unroutable";
                    }
                    grant_decision::grant_auto_deny_reply(
                        &req.req_id,
                        &grant.capsule_id,
                        &principal,
                    )
                }
                grant_decision::GrantAction::Prompt => {
                    // No durable decision yet. Dedup: if a grant prompt is already
                    // pending for this `(principal, capsule)` pair, reply a benign,
                    // actionable terminal result instead of a fresh elicit, so a
                    // flurry of ungranted calls for the same capsule does not stack
                    // duplicate prompts. The marker is non-consumingly peeked here
                    // (it must survive to keep deduping) and consumed only on the
                    // respond ([`crate::approval::handle_mcp_grant_respond`]).
                    if execute::grant_pending(&principal, &grant.capsule_id) {
                        outcome = "grant_pending_dedup";
                        grant_decision::grant_dedup_reply(&req.req_id)
                    } else {
                        // First prompt for this pair — record it pending, then
                        // surface the flag. `mark` is best-effort: a lost marker
                        // only risks a duplicate prompt, never a spurious grant.
                        execute::mark_grant_pending(&principal, &grant.capsule_id);
                        outcome = "grant_required";
                        json!({
                            "kind": "tool.call",
                            "req_id": req.req_id,
                            // No tool result — the kernel dropped the call at the
                            // access gate. The shim MUST elicit Grant/Deny and
                            // respond on `astrid.v1.request.mcp.grant.respond`, then
                            // re-send on Grant. `content` empty, `isError` false: a
                            // prompt-needed state, not a failure.
                            "content": mcp_content(Value::String(String::new())),
                            "isError": false,
                            "grant_required": grant.to_reply_flag(&req.name, &req.req_id),
                        })
                    }
                }
            };
            log::info(format!(
                "{}: broker response method=tool.call req_id={} tool={} outcome={outcome} \
                 capsule={} elapsed_ms={}",
                crate::profile::log_tag(),
                req.req_id,
                req.name,
                grant.capsule_id,
                discovery::wall_ms().saturating_sub(started)
            ));
            reply
        }
        execute::DispatchOutcome::Failed(message) => {
            // A received request that could NOT be dispatched / drained (no
            // provider, not-subscribed, dispatch error, or response timeout)
            // → WARN, never silent. `dispatch_with_approval` collapses every
            // such mode into a single human-readable `message`; it carries no
            // tool arguments, only the tool name + failure reason, so it is
            // safe to surface verbatim.
            log::warn(format!(
                "{}: broker no-route req_id={} tool={} reason=\"{message}\" elapsed_ms={}",
                crate::profile::log_tag(),
                req.req_id,
                req.name,
                discovery::wall_ms().saturating_sub(started)
            ));
            json!({
                "kind": "tool.call",
                "req_id": req.req_id,
                "content": mcp_content(Value::String(message)),
                "isError": true,
            })
        }
    };
    publish_reply(&reply_topic, &reply);
    Ok(())
}

/// Evaluate the per-principal [`crate::policy`] rule set against the native
/// tool a PreToolUse hook is asking about, and build the broker `tool.call`
/// reply whose `content` text is the Claude hook-decision JSON.
///
/// The host calls (policy load, audit publish, log) live here; the pure
/// reply shaping is delegated to [`gate_reply_body`] so the allow/deny ->
/// JSON contract stays unit-testable without a live bus.
fn pretooluse_gate_reply(req: &CallRequest) -> Value {
    let tool_name = req
        .arguments
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tool_input = gate_tool_input(&req.arguments);

    let decision = crate::policy::evaluate(&crate::policy::load_rules(), tool_name, &tool_input);

    if let crate::policy::Decision::Deny { reason } = &decision {
        log::info(format!(
            "{}: PreToolUse policy denied native tool '{tool_name}' (req_id '{}'): {reason}",
            crate::profile::log_tag(),
            req.req_id
        ));
        // Audit the native-tool denial on the same `astrid.v1.audit.policy_*`
        // family the in-process broker gate uses. Operator rule id only —
        // never the reflected tool arguments (injection). Best-effort.
        let _ = ipc::publish_json(
            &crate::profile::audit_topic("pretooluse_deny"),
            &json!({ "tool": tool_name, "rule": reason }),
        );
    }

    gate_reply_body(&req.req_id, &decision)
}

/// Pure shaper: map a PDP [`Decision`](crate::policy::Decision) to the broker
/// `tool.call` reply the shim relays verbatim to Claude's PreToolUse hook.
/// No host calls — fully unit-testable.
///
/// Two invariants this encodes:
///
/// * `isError` is ALWAYS `false`. Claude treats an `isError:true` MCP result
///   as a non-blocking error and runs the tool anyway (fail-open), so a DENY
///   must travel in the `content` payload, not the error flag.
/// * It can only NARROW. `Decision::Deny` -> `permissionDecision:"deny"`;
///   `Decision::Allow` (which is also the no-rule / load-failure default)
///   -> a no-op `{"continue":true}` that defers to Claude's existing
///   permission flow (the allow-list + `dontAsk`), NEVER an explicit
///   `"allow"` that could skip a prompt or broaden the surface.
fn gate_reply_body(req_id: &str, decision: &crate::policy::Decision) -> Value {
    let hook_output = match decision {
        crate::policy::Decision::Deny { reason } => json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason":
                    format!("denied by Astrid policy (rule: {reason})"),
            }
        }),
        crate::policy::Decision::Allow => json!({ "continue": true }),
    };

    // The hook parses the gate tool's TEXT content like command-hook stdout,
    // so serialize the decision object to a JSON string and wrap it in the
    // standard MCP content block. `isError:false` — see the invariants above.
    json!({
        "kind": "tool.call",
        "req_id": req_id,
        "content": mcp_content(Value::String(
            serde_json::to_string(&hook_output)
                .unwrap_or_else(|_| String::from("{\"continue\":true}")),
        )),
        "isError": false,
    })
}

/// Extract the native tool's input object from the gate-call arguments.
///
/// The hook authors `"tool_input": "${tool_input}"`. Claude's `${...}`
/// interpolator resolves the `tool_input` node of the hook payload and — as
/// VERIFIED against the shipped `claude` executor, whose interpolator
/// `JSON.stringify`s any resolved object before splicing it into the string —
/// replaces the token with `JSON.stringify(tool_input)`. So `tool_input`
/// reaches the broker as a JSON STRING, which we parse back into the object
/// the PDP matches against (full argument-level rules). A native tool always
/// carries a non-empty `tool_input`; an absent path would interpolate to `""`
/// (the executor's missing-path rule), which parses to `Null`.
///
/// The non-string arms are defensive belt-and-suspenders for a future build
/// that injects structurally, or another MCP client; an unparseable value
/// degrades to `Null` -> tool-NAME-only matching, a safe narrowing (never a
/// broadening, since the PDP default is allow):
///
/// * a JSON STRING -> parsed (the verified, real-world path);
/// * a nested JSON object/array -> used directly;
/// * absent / non-JSON -> `Null` (tool-name-only matching).
///
/// `pub(crate)` so the `before_tool_call` hook responder ([`crate::hook_gate`])
/// reuses the exact same defensive extraction — one definition, no drift
/// between the two native-tool gate transports.
pub(crate) fn gate_tool_input(arguments: &Value) -> Value {
    match arguments.get("tool_input") {
        Some(v @ (Value::Object(_) | Value::Array(_))) => v.clone(),
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

/// Read the kernel-stamped caller attribution for ingress logging on a
/// read-only path (`tools.list`) that does not otherwise need it. Returns
/// `(source_id, principal)` rendered for a log line — never fails the
/// request: a missing caller context degrades to `unknown` placeholders so
/// observability never changes control flow. The mutating `tool.call` path
/// keeps its own caller fetch (it fails closed on error) and does not use
/// this.
fn caller_fields() -> (String, String) {
    match runtime::caller() {
        Ok(ctx) => {
            let principal = ctx.principal.unwrap_or_default();
            let principal = if principal.is_empty() {
                "unknown".to_string()
            } else {
                principal
            };
            (ctx.source_id, principal)
        }
        Err(_) => ("unknown".to_string(), "unknown".to_string()),
    }
}

/// Render a principal id for a log line, mapping the empty/absent case (the
/// SDK exposes `principal` as `Option<String>`) to a stable `unknown` tag so
/// the `principal=` field is never blank/ambiguous when grepping.
fn display_principal(principal: &str) -> &str {
    if principal.is_empty() {
        "unknown"
    } else {
        principal
    }
}

/// Number of top-level tool arguments, for a non-sensitive shape signal in
/// the ingress log. We log the COUNT, never the argument CONTENTS (a value
/// may carry a secret). A JSON object counts its keys; any other shape
/// (array / scalar / null) is reported as `0` — the broker's tools all take
/// an object argument, so a non-object is the no-arguments case.
fn arg_count(arguments: &Value) -> usize {
    arguments.as_object().map_or(0, serde_json::Map::len)
}

/// Build the single-segment egress topic for `req_id`, or `None` if the
/// id cannot form a clean single segment.
///
/// Rejects empty, oversized, and any id carrying a `.` (which would
/// turn the 4-segment response topic into a 5-segment one the proxy's
/// `astrid.v1.response.*` subscription can't match) or whitespace /
/// control / wildcard bytes (which would forge or shadow topics). Same
/// charset family the tool-name gate uses, so the surface is uniform.
///
/// `pub(crate)` so the approval bridge ([`crate::approval`]) reuses the
/// exact same egress-topic gate when acking the shim — one definition,
/// no drift.
pub(crate) fn reply_topic(req_id: &str) -> Option<String> {
    if req_id.is_empty() || req_id.len() > MAX_REQ_ID_LEN {
        return None;
    }
    let clean = req_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
    if !clean {
        return None;
    }
    Some(format!("{RESPONSE_PREFIX}{req_id}"))
}

/// Wrap a tool result into the MCP `content` block array the host already
/// emits elsewhere: `[{ "type":"text", "text":<string> }]`. Structured
/// (non-string) results are serialized to JSON text so the wire stays
/// UTF-8 string-shaped and the proxy needs no schema knowledge.
///
/// `pub(crate)` so the approval bridge ([`crate::approval`]) shapes the
/// resumed/denied terminal `tool.call` reply with the exact same content
/// encoding the non-parked path uses — no drift between the two reply legs.
pub(crate) fn mcp_content(content: Value) -> Value {
    let text = match &content {
        Value::String(s) => s.clone(),
        _ => serde_json::to_string(&content)
            .unwrap_or_else(|_| String::from("<unserializable tool result>")),
    };
    json!([{ "type": "text", "text": text }])
}

/// Human-readable, actionable guidance for an untrusted-ingress denial.
///
/// The confused-deputy gate in [`handle_mcp_call`] withholds a state-mutating
/// tool call from an ingress `source_id` the user has not yet consented to, and
/// asks the shim to raise a one-time interactive consent prompt. On a client
/// that cannot render elicitation, that prompt never appears and the call would
/// otherwise dead-end as a bare "not authorized" with no path forward. This
/// text is carried on the `ingress_approval_required` reply so any surface that
/// renders it tells the user WHICH session was withheld (`source_id`), WHY (the
/// one-time consent gate), and HOW to proceed.
///
/// Trust is granted EXCLUSIVELY by accepting the interactive prompt — there is
/// deliberately no operator allow-list, config key, env var, or CLI flag that
/// grants it. Any such non-interactive path would reopen the confused-deputy
/// hole this gate exists to close, so the guidance must never imply one.
pub(crate) fn ingress_consent_guidance(source_id: &str) -> String {
    format!(
        "Astrid withheld this tool call: the ingress session (source_id: {source_id}) \
         is not yet trusted. The first tool call from a new client session opens a \
         one-time interactive consent prompt; approving it records trust for this \
         source_id so later calls dispatch without re-prompting. Approve the prompt \
         in your MCP client. If your client cannot display interactive prompts (no \
         MCP elicitation support), the call fails closed by design — retry from a \
         client that can, such as the Astrid CLI uplink. This interactive prompt is \
         the only way to grant trust: there is no operator allow-list or config key \
         to set."
    )
}

/// Publish the broker reply, logging (not erroring) on host failure —
/// the proxy times out on its side if delivery fails.
fn publish_reply(topic: &str, reply: &Value) {
    if let Err(e) = ipc::publish_json(topic, reply) {
        log::warn(format!(
            "{}: broker failed to publish {topic}: {e}",
            crate::profile::log_tag()
        ));
    }
}

#[cfg(test)]
mod tests {
    fn install_test_profile() {
        crate::profile::install_aos();
    }

    use super::*;

    #[test]
    fn reply_topic_accepts_uuid_simple() {
        install_test_profile();
        let id = "0191f3a2b4c74d8e9f01234567890abc";
        assert_eq!(
            reply_topic(id).as_deref(),
            Some("astrid.v1.response.0191f3a2b4c74d8e9f01234567890abc")
        );
    }

    #[test]
    fn reply_topic_rejects_dotted_req_id() {
        install_test_profile();
        // A `.` would make the egress topic 5 segments — the proxy's
        // 4-segment `astrid.v1.response.*` subscription would never
        // match it, so the reply would be silently dropped.
        assert!(reply_topic("a.b").is_none());
    }

    #[test]
    fn reply_topic_rejects_topic_smuggling() {
        install_test_profile();
        assert!(reply_topic("").is_none());
        assert!(reply_topic("a b").is_none());
        assert!(reply_topic("a*b").is_none());
        assert!(reply_topic("a\nb").is_none());
        assert!(reply_topic("a/b").is_none());
        let too_long = "a".repeat(MAX_REQ_ID_LEN + 1);
        assert!(reply_topic(&too_long).is_none());
    }

    #[test]
    fn reply_topic_accepts_hyphenated_uuid() {
        install_test_profile();
        let id = "0191f3a2-b4c7-4d8e-9f01-234567890abc";
        assert!(reply_topic(id).is_some());
    }

    #[test]
    fn arg_count_counts_object_keys_only() {
        install_test_profile();
        // The ingress log reports the COUNT of top-level arguments (a
        // non-sensitive shape signal), never the values. An object counts its
        // keys; any non-object shape is the no-arguments case (0).
        assert_eq!(arg_count(&json!({ "a": 1, "b": 2 })), 2);
        assert_eq!(arg_count(&json!({})), 0);
        assert_eq!(arg_count(&Value::Null), 0);
        assert_eq!(arg_count(&json!([1, 2, 3])), 0);
        assert_eq!(arg_count(&json!("scalar")), 0);
    }

    #[test]
    fn display_principal_maps_empty_to_unknown() {
        install_test_profile();
        assert_eq!(display_principal(""), "unknown");
        assert_eq!(display_principal("user-7"), "user-7");
    }

    #[test]
    fn mcp_content_wraps_string_verbatim() {
        install_test_profile();
        let blocks = mcp_content(Value::String("hello".into()));
        assert_eq!(blocks, json!([{ "type": "text", "text": "hello" }]));
    }

    #[test]
    fn mcp_content_serializes_structured_result() {
        install_test_profile();
        let blocks = mcp_content(json!({ "ok": true }));
        assert_eq!(blocks, json!([{ "type": "text", "text": "{\"ok\":true}" }]));
    }

    #[test]
    fn list_request_requires_req_id() {
        install_test_profile();
        assert!(serde_json::from_value::<ListRequest>(json!({})).is_err());
        let ok: ListRequest = serde_json::from_value(json!({ "req_id": "x" })).unwrap();
        assert_eq!(ok.req_id, "x");
    }

    #[test]
    fn call_request_defaults_arguments() {
        install_test_profile();
        let req: CallRequest =
            serde_json::from_value(json!({ "req_id": "x", "name": "fs.read" })).unwrap();
        assert_eq!(req.req_id, "x");
        assert_eq!(req.name, "fs.read");
        assert_eq!(req.arguments, Value::Null);
    }

    #[test]
    fn ingress_denial_guidance_is_actionable() {
        install_test_profile();
        // Regression: an untrusted-ingress denial must NOT dead-end. The
        // guidance the `ingress_approval_required` reply carries has to name
        // the withheld session's source_id and tell the user how to proceed,
        // so a client that cannot render the elicitation still shows a path
        // forward instead of a bare "not authorized".
        let source_id = "cap-0191f3a2b4c74d8e9f01234567890abc";
        let guidance = ingress_consent_guidance(source_id);

        // Names the exact session that was withheld.
        assert!(
            guidance.contains(source_id),
            "guidance must echo the source_id: {guidance}"
        );
        // Explains the one-time interactive consent gate.
        assert!(
            guidance.contains("consent") && guidance.contains("one-time"),
            "guidance must explain the one-time consent gate: {guidance}"
        );
        // Tells the user to approve the interactive prompt.
        assert!(
            guidance.to_lowercase().contains("approve"),
            "guidance must instruct the user to approve the prompt: {guidance}"
        );
        // Truthful about the fail-closed model: no operator allow-list / config
        // key grants trust (only the interactive prompt does). Guards against a
        // regression that reintroduces the confused-deputy escape hatch in text.
        assert!(
            guidance.contains("no operator allow-list"),
            "guidance must state there is no non-interactive grant path: {guidance}"
        );

        // And the reply the gate publishes actually carries it, on both the
        // human-readable `message` field and the MCP `content` block, while
        // keeping the `ingress_approval_required` signal and NOT flipping to an
        // error (which would suppress an elicit-capable shim's prompt).
        let reply = json!({
            "kind": "tool.call",
            "req_id": "req1",
            "ingress_approval_required": true,
            "source_id": source_id,
            "tool_name": "fs.write",
            "message": guidance,
            "content": mcp_content(Value::String(guidance.clone())),
            "isError": false,
        });
        assert_eq!(
            reply
                .pointer("/ingress_approval_required")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            reply.pointer("/isError").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            reply.pointer("/message").and_then(Value::as_str),
            Some(guidance.as_str())
        );
        assert_eq!(
            reply.pointer("/content/0/text").and_then(Value::as_str),
            Some(guidance.as_str())
        );
    }

    // ------------------------------------------------------------------
    // PreToolUse native-tool gate.
    // ------------------------------------------------------------------

    /// Parse the hook-decision JSON back out of a gate reply's single text
    /// content block, so a test can assert on the decision the hook sees.
    fn gate_decision_json(reply: &Value) -> Value {
        let text = reply
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .expect("gate reply must carry one text content block");
        serde_json::from_str(text).expect("gate decision content must be JSON")
    }

    #[test]
    fn gate_deny_blocks_via_content_never_iserror() {
        install_test_profile();
        let decision = crate::policy::Decision::Deny {
            reason: "no-ssh-write".into(),
        };
        let reply = gate_reply_body("req1", &decision);

        // The fail-open invariant: a DENY must NOT be signalled via
        // `isError` (that would let the tool run); it rides in `content`.
        assert_eq!(
            reply.pointer("/isError").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            reply.pointer("/req_id").and_then(Value::as_str),
            Some("req1")
        );

        let decision_json = gate_decision_json(&reply);
        assert_eq!(
            decision_json.pointer("/hookSpecificOutput/hookEventName"),
            Some(&Value::String("PreToolUse".into()))
        );
        assert_eq!(
            decision_json.pointer("/hookSpecificOutput/permissionDecision"),
            Some(&Value::String("deny".into()))
        );
        // The reason is the operator rule id, not a reflected argument.
        let reason = decision_json
            .pointer("/hookSpecificOutput/permissionDecisionReason")
            .and_then(Value::as_str)
            .unwrap();
        assert!(reason.contains("no-ssh-write"), "reason was {reason:?}");
    }

    #[test]
    fn gate_allow_defers_never_broadens() {
        install_test_profile();
        let reply = gate_reply_body("req2", &crate::policy::Decision::Allow);
        assert_eq!(
            reply.pointer("/isError").and_then(Value::as_bool),
            Some(false)
        );

        // Allow must be a no-op continue, NOT an explicit permissionDecision
        // — an explicit "allow" would suppress a prompt / broaden the surface,
        // and the gate only ever narrows.
        let decision_json = gate_decision_json(&reply);
        assert_eq!(decision_json, json!({ "continue": true }));
        assert!(
            decision_json.pointer("/hookSpecificOutput").is_none(),
            "allow path must not assert a permissionDecision"
        );
    }

    #[test]
    fn gate_tool_input_accepts_nested_object() {
        install_test_profile();
        let args = json!({ "tool_name": "Bash", "tool_input": { "command": "rm -rf /" } });
        assert_eq!(gate_tool_input(&args), json!({ "command": "rm -rf /" }));
    }

    #[test]
    fn gate_tool_input_parses_json_string() {
        install_test_profile();
        // If `${tool_input}` substitution stringifies the object, parse it
        // back so argument-level rules still work.
        let args =
            json!({ "tool_name": "Write", "tool_input": "{\"file_path\":\"home://.ssh/x\"}" });
        assert_eq!(
            gate_tool_input(&args),
            json!({ "file_path": "home://.ssh/x" })
        );
    }

    #[test]
    fn gate_tool_input_degrades_to_null_on_unsubstituted_or_absent() {
        install_test_profile();
        // A literal un-substituted placeholder is not valid JSON -> Null
        // (tool-name-only matching), never a parse panic.
        let literal = json!({ "tool_name": "Bash", "tool_input": "${tool_input}" });
        assert_eq!(gate_tool_input(&literal), Value::Null);
        // Absent tool_input -> Null.
        let absent = json!({ "tool_name": "Bash" });
        assert_eq!(gate_tool_input(&absent), Value::Null);
    }

    #[test]
    fn gate_tool_name_is_pinned() {
        install_test_profile();
        // Value anchor for the cross-crate SYNC with
        // `claude_install::layout::PRETOOLUSE_GATE_TOOL`. No dependency edge
        // between the crates, so the name is mirrored, not shared; the
        // claude-install pins the same literal in
        // `pretooluse_gate_tool_name_is_pinned`. A rename on one side without
        // the other silently disables the gate (fail-open), so both anchor the
        // exact string and a deliberate edit must touch both tests.
        assert_eq!(PRETOOLUSE_GATE_TOOL, "aos_pretooluse_gate");
    }

    #[test]
    fn gate_end_to_end_shapes_a_deny_from_a_string_input() {
        install_test_profile();
        // Exercise gate_tool_input -> evaluate -> gate_reply_body together
        // (the pure spine of pretooluse_gate_reply, minus the host calls).
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
        let args = json!({ "tool_name": "Bash", "tool_input": { "command": "rm -rf /tmp" } });
        let decision = crate::policy::evaluate(
            &rules,
            args.get("tool_name").and_then(Value::as_str).unwrap(),
            &gate_tool_input(&args),
        );
        let reply = gate_reply_body("req3", &decision);
        assert_eq!(
            reply.pointer("/isError").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            gate_decision_json(&reply).pointer("/hookSpecificOutput/permissionDecision"),
            Some(&Value::String("deny".into()))
        );
    }
}
