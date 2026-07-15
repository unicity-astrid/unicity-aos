use std::collections::HashMap;

use astrid_sdk::net::{TcpStream, TryRecvError, bind_unix};
use astrid_sdk::prelude::*;

#[derive(Default)]
struct CliProxy;

/// A connected CLI client bound to exactly one principal.
///
/// A connection binds on its first ingress message and stays bound to that
/// single principal for its whole lifetime (one connection = one principal,
/// per `astrid-runtime/astrid#852`):
///
/// * First message carrying a valid `principal` binds to it.
/// * First message with no `principal` binds to `"default"` (auto-attribution
///   for un-stamped clients).
/// * A first message whose principal is malformed is dropped and the
///   connection stays `None` (unbound) so a later well-formed message can bind.
///
/// Once bound, all of this connection's traffic attributes to its principal,
/// and it only receives outbound IPC stamped with that same principal (plus
/// unprincipaled system events). The outbound demux ([`should_deliver`]) routes
/// on two keys: `principal` AND `session_id`. It stays `None` only for a
/// connection that has not yet sent a usable message; such a connection
/// receives only unprincipaled events.
///
/// `session_id` is the conversation this connection is on, learned only from a
/// forwarded chat prompt (`user.v1.prompt`). Only a chat response
/// (`agent.v1.response`) is session-demuxed: it is delivered only to the
/// connection on its session, so two connections sharing a principal but on
/// different sessions never cross-talk. Correlated request/response and system
/// traffic stay principal-routed (and are correlation-id filtered by the TUI),
/// so a connection that has not yet bound a session is never starved of them.
/// It stays `None` until the connection sends a prompt, and tracks the latest
/// session observed, so a connection that switches session re-targets its demux.
///
/// This binding is independent of connection-count tracking: the kernel's
/// per-principal connection counter is driven by host-emitted
/// `client.v1.connect` / `client.v1.disconnect`, not by these fields.
struct ProxyClient {
    stream: TcpStream,
    principal: Option<String>,
    session_id: Option<String>,
}

impl ProxyClient {
    fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            principal: None,
            session_id: None,
        }
    }
}

/// Decision produced by the per-connection binding state machine, separated
/// from the IPC side effects so the accept/drop matrix is unit-testable.
#[derive(Debug, PartialEq, Eq)]
enum IngressDecision {
    /// Bind the (currently unbound) connection to this principal and forward
    /// the message stamped with it. Emitted only for the first usable message.
    Bind(String),
    /// Forward the message stamped with the already-bound principal.
    ForwardAs(String),
    /// Drop the message without forwarding; do not mutate the binding.
    Drop { reason: DropReason },
}

/// Why an ingress message was dropped, for logging.
#[derive(Debug, PartialEq, Eq)]
enum DropReason {
    /// First message carried a principal that failed format validation.
    InvalidPrincipal(String),
    /// Message claimed a principal different from the bound one.
    PrincipalConflict { bound: String, claimed: String },
}

/// Default principal for connections whose first message carries no principal.
const DEFAULT_PRINCIPAL: &str = "default";

/// Validate a principal string before binding/forwarding: 1-64 chars from the
/// `[A-Za-z0-9_-]` set. The host's `publish_as` would reject an invalid one
/// anyway, but pre-validating gives a clean log and avoids a partial forward.
fn is_valid_principal(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 64
        && p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Pure binding decision: given the connection's current binding and the
/// principal field of the incoming message, decide whether to bind, forward,
/// or drop. No IPC, no logging — the caller performs the effects.
///
/// First message (`current == None`):
/// * `Some(p)` valid   -> `Bind(p)`
/// * `Some(p)` invalid -> `Drop(InvalidPrincipal)` (stays unbound)
/// * `None`            -> `Bind("default")`
///
/// Bound connection (`current == Some(b)`):
/// * `None`            -> `ForwardAs(b)`        (auto-attribution)
/// * `Some(p) == b`    -> `ForwardAs(b)`
/// * `Some(p) != b`    -> `Drop(PrincipalConflict)` (binding unchanged)
fn decide_ingress(
    current_binding: Option<&str>,
    message_principal: Option<&str>,
) -> IngressDecision {
    match (current_binding, message_principal) {
        (None, Some(p)) => {
            if is_valid_principal(p) {
                IngressDecision::Bind(p.to_string())
            } else {
                IngressDecision::Drop {
                    reason: DropReason::InvalidPrincipal(p.to_string()),
                }
            }
        }
        (None, None) => IngressDecision::Bind(DEFAULT_PRINCIPAL.to_string()),
        (Some(b), None) => IngressDecision::ForwardAs(b.to_string()),
        (Some(b), Some(p)) if p == b => IngressDecision::ForwardAs(b.to_string()),
        (Some(b), Some(p)) => IngressDecision::Drop {
            reason: DropReason::PrincipalConflict {
                bound: b.to_string(),
                claimed: p.to_string(),
            },
        },
    }
}

/// Pure outbound-demux decision: should an IPC message attributed to
/// `msg_principal` and scoped to `msg_session` be delivered to a client bound to
/// `client_principal` on session `client_session`?
///
/// Two gates, both must pass:
///
/// * PRINCIPAL — `Some(p)` delivers only to clients bound to exactly `p`;
///   `None` (system/broadcast) passes for every client, including unbound ones.
/// * SESSION — `Some(s)` (a session-scoped message, e.g. a chat
///   `agent.v1.response`) delivers only to the client currently on session `s`;
///   `None` (not session-scoped: correlated request/response, system events)
///   passes for every client that cleared the principal gate. A session-scoped
///   message is therefore never delivered to a connection on a different
///   session — or to one with no session yet — which is what prevents
///   same-principal, multi-session cross-talk.
fn should_deliver(
    msg_principal: Option<&str>,
    msg_session: Option<&str>,
    client_principal: Option<&str>,
    client_session: Option<&str>,
) -> bool {
    let principal_ok = match msg_principal {
        None => true,
        Some(p) => client_principal == Some(p),
    };
    if !principal_ok {
        return false;
    }
    match msg_session {
        None => true,
        Some(s) => client_session == Some(s),
    }
}

/// Topic carrying a connection's chat input. The `session_id` on a *forwarded*
/// message of this topic is the authoritative source for which conversation the
/// connection is on (paired with [`CHAT_RESPONSE_TOPIC`]).
const CHAT_REQUEST_TOPIC: &str = "user.v1.prompt";

/// Topic carrying streamed chat responses — session-demuxed, together with
/// [`CHAT_DELTA_TOPIC`]. Everything else routes by principal alone (correlated
/// request/response topics are already correlation-id filtered by the TUI), so
/// a non-chat response that merely happens to carry a `session_id` is never
/// dropped for a connection that has not bound that session.
const CHAT_RESPONSE_TOPIC: &str = "agent.v1.response";

/// Topic carrying incremental chat tokens (`is_final: false` `AgentResponse`s)
/// while the agent generates. Session-demuxed exactly like [`CHAT_RESPONSE_TOPIC`]
/// — a streamed token must reach only the connection on that session, never a
/// same-principal connection on a different one. Forwarded live for real-time
/// display; the proxy accumulates them per session so the terminal
/// [`CHAT_RESPONSE_TOPIC`] can be reconciled (see [`reconcile_stream_payload`]).
const CHAT_DELTA_TOPIC: &str = "agent.v1.stream.delta";

/// Upper bound on concurrently-streaming sessions the proxy accumulates text
/// for. A turn always drops its entry on its terminal response, so this is only
/// a defensive ceiling against a turn that never finalizes; far above the
/// host-enforced 8-connection cap.
const MAX_STREAM_SESSIONS: usize = 64;

/// Extract the top-level `"session_id"` string from a message payload, if any.
///
/// Low-level helper: the `IpcPayload` enum is internally tagged, so `session_id`
/// sits beside `"type"`. Callers gate on the *topic* before applying it (see
/// [`ingress_session_bind`] / [`outbound_session_scope`]) — session routing is
/// scoped to the chat request/response pair, never to every payload that
/// happens to carry a session id.
fn payload_session_id(payload: &serde_json::Value) -> Option<&str> {
    payload.get("session_id").and_then(|v| v.as_str())
}

/// The conversation session a connection should bind from an *ingress* message,
/// or `None` to leave the current binding unchanged.
///
/// Only a chat prompt ([`CHAT_REQUEST_TOPIC`]) retargets the connection's
/// session. The caller applies this solely to forwarded (allowlisted) traffic,
/// so a dropped, blocked, or no-body message can never spoof a connection onto
/// another session.
fn ingress_session_bind<'a>(topic: &str, payload: &'a serde_json::Value) -> Option<&'a str> {
    (topic == CHAT_REQUEST_TOPIC)
        .then(|| payload_session_id(payload))
        .flatten()
}

/// The conversation session an *outbound* message is scoped to for demux, or
/// `None` to route by principal alone.
///
/// Only streamed chat responses ([`CHAT_RESPONSE_TOPIC`]) and their incremental
/// deltas ([`CHAT_DELTA_TOPIC`]) are session-scoped. Correlated request/response
/// replies keep principal routing even when their payload carries a `session_id`,
/// so a connection awaiting such a reply (whose own session may not yet be bound)
/// is never starved.
fn outbound_session_scope<'a>(topic: &str, payload: &'a serde_json::Value) -> Option<&'a str> {
    (topic == CHAT_RESPONSE_TOPIC || topic == CHAT_DELTA_TOPIC)
        .then(|| payload_session_id(payload))
        .flatten()
}

/// Reconcile a streamed chat payload against the per-session accumulator so the
/// CLI TUI — which appends every `AgentResponse.text` it receives and flushes
/// the buffer on `is_final` — renders the reply exactly once.
///
/// - A delta ([`CHAT_DELTA_TOPIC`]): record its text into the session's
///   accumulator and forward it verbatim (the TUI appends it live).
/// - The terminal ([`CHAT_RESPONSE_TOPIC`], `is_final: true`): the TUI has
///   already received the streamed prefix, so rewrite the body to only the
///   not-yet-streamed remainder (normally empty; a non-empty tail only when a
///   delta was dropped) and drop the accumulator. A non-streaming turn (no
///   deltas were seen for the session) keeps the full body untouched.
///
/// Mutates `payload` in place and updates `accum`. Relies on the delta
/// subscription being polled before the response subscription each loop turn,
/// so all deltas for a turn are accumulated before its terminal is reconciled.
///
/// Keyed by session, not connection: this is correct because a session is bound
/// to a single connection (one client per session — a session retargets only
/// from that connection's own chat prompt, see [`ingress_session_bind`]), so the
/// one connection on a session has received every delta the terminal reconciles
/// against. There is no second client on the same session to under-serve.
fn reconcile_stream_payload(
    topic: &str,
    payload: &mut serde_json::Value,
    accum: &mut HashMap<String, String>,
) {
    let Some(session) = payload_session_id(payload).map(str::to_string) else {
        return;
    };

    if topic == CHAT_DELTA_TOPIC {
        if let Some(text) = payload.get("text").and_then(|v| v.as_str()) {
            // Bound growth: a turn always closes with a terminal response that
            // drops its entry, but cap defensively so a turn that never
            // finalizes can't leak unboundedly across the proxy's lifetime.
            // At the cap, evict an arbitrary old entry rather than refusing the
            // new session: refusing would leave every subsequent session
            // unreconciled (double-render) once leaked entries fill the map.
            // The evicted entry is by construction a leaked/stale turn; if its
            // terminal ever does arrive it renders in full (at worst
            // duplicating deltas already shown for that stale session).
            if accum.len() >= MAX_STREAM_SESSIONS
                && !accum.contains_key(&session)
                && let Some(stale) = accum.keys().next().cloned()
            {
                accum.remove(&stale);
            }
            accum.entry(session).or_default().push_str(text);
        }
        return;
    }

    if topic == CHAT_RESPONSE_TOPIC
        && payload
            .get("is_final")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        && let Some(streamed) = accum.remove(&session)
    {
        // The streamed prefix already reached the TUI; send only the remainder.
        // `strip_prefix` is `None` only if a delta was dropped mid-stream (the
        // proxy logs such drops separately) — fall back to an empty body rather
        // than re-sending the whole reply on top of the streamed text.
        let full = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let remainder = full
            .strip_prefix(streamed.as_str())
            .unwrap_or("")
            .to_string();
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("text".to_string(), serde_json::Value::String(remainder));
        }
    }
}

/// Collapse an SDK [`ipc::PrincipalAttribution`] to the target principal for
/// outbound routing. Both `Verified` and `Claimed` name a concrete principal a
/// message is attributed to; `System` events have no principal and broadcast.
///
/// Routing intentionally does not distinguish verified from claimed here: this
/// is fan-out of internally published responses (`publish_as` from trusted
/// capsules yields `Verified`), and the demux question is only "which client's
/// principal does this belong to", not a capability check.
fn attribution_target(attr: &ipc::PrincipalAttribution) -> Option<&str> {
    match attr {
        ipc::PrincipalAttribution::Verified(p) | ipc::PrincipalAttribution::Claimed(p) => Some(p),
        ipc::PrincipalAttribution::System => None,
    }
}

#[capsule]
impl CliProxy {
    #[astrid::run]
    fn run(&self) -> Result<(), SysError> {
        // 1. Subscribe to TUI-relevant IPC topics only.
        // IMPORTANT: If a new event topic is consumed by the TUI, add it here.
        // Internal pipeline events (LLM requests, tool dispatch, identity builds)
        // must NOT be forwarded to the CLI socket.
        let topics = [
            // ORDER MATTERS: the delta topic precedes the response topic so the
            // accumulator is filled before a turn's terminal is reconciled
            // (subscriptions are polled in this order; see `reconcile_stream_payload`).
            CHAT_DELTA_TOPIC,
            CHAT_RESPONSE_TOPIC,
            "astrid.v1.onboarding.required",
            "astrid.v1.elicit.*",
            "astrid.v1.approval",
            "astrid.v1.response.*",
            "astrid.v1.admin.response.*",
            "astrid.v1.capsules_loaded",
            "registry.v1.response.*",
            "registry.v1.active_model_changed",
            "registry.v1.selection.*",
            "session.v1.response.*",
            // Forwards capsule CLI verb results (`aos capsule <verb>`) back to
            // the requesting socket client (astrid#891).
            "cli.v1.command.result.*",
        ];
        // Subscriptions are RAII handles - drop releases the kernel-side
        // resource. Keep them owned by the run loop for the proxy's lifetime.
        let subs: Vec<ipc::Subscription> = topics
            .iter()
            .map(|t| ipc::subscribe(t))
            .collect::<Result<Vec<_>, _>>()?;

        // Signal readiness so the kernel can proceed with loading dependent capsules.
        // Best-effort: failure means the host mutex is poisoned (unrecoverable).
        let _ = runtime::signal_ready();

        // 2. Resolve the socket path from the kernel-injected config.
        // bind_unix is a no-op on the host side (the kernel pre-binds the socket),
        // but the path is used for logging and future diagnostics.
        let path = runtime::socket_path()
            .map_err(|e| SysError::ApiError(format!("Failed to resolve socket path: {e}")))?;

        log::info(format!("CLI Proxy: accepting connections on {path}"));
        let listener = bind_unix()?;

        // 3. Multi-connection accept loop.
        // Supports up to 8 concurrent CLI clients (enforced at host level).
        //
        // Each connection binds to exactly one principal on its first message
        // (see `ProxyClient` / `decide_ingress`) and stays bound for life, and
        // tracks the conversation session it is on. A connection's ingress
        // always attributes to its bound principal; its egress is demuxed on
        // both principal AND session, so it only receives IPC stamped with that
        // principal (plus unprincipaled system events), and a session-scoped
        // response only when it is on that session. There is no cross-principal
        // leakage, no same-principal cross-session leakage, and no
        // broadcast-to-all of principaled traffic.
        //
        // TcpStream is the post-#752 unified handle (Unix-domain accepts and
        // outbound TCP share the same resource type). Drop releases the
        // kernel-side stream entry, so we no longer need a manual close.
        //
        // Connection lifecycle tracking (`client.v1.connect` /
        // `client.v1.disconnect`, which drive the kernel's per-principal
        // active-connection count for ephemeral idle-shutdown and `astrid
        // who`) is emitted HOST-side, not here. The host owns the inbound
        // socket and holds the handshake-verified principal for the whole
        // connection lifetime, so connect and disconnect always pair on the
        // identical principal. The proxy used to emit them, but its disconnect
        // fired after the connection's verified identity was already gone — the
        // host stamped it `anonymous`, so the real principal's count leaked.
        let mut clients: Vec<ProxyClient> = Vec::new();

        // Per-session accumulator of streamed chat tokens, so the terminal
        // `agent.v1.response` can be reconciled against what the TUI already
        // rendered live (see `reconcile_stream_payload`). Entries are dropped on
        // each turn's terminal response.
        let mut stream_accum: HashMap<String, String> = HashMap::new();

        'proxy: loop {
            // Phase A: block until at least one client is connected.
            if clients.is_empty() {
                let stream = match listener.accept() {
                    Ok(s) => s,
                    Err(e) => {
                        log::warn(format!("Accept error: {e:?}, backing off"));
                        // `std::thread::sleep` panics on wasm32-unknown-unknown
                        // ("can't sleep" — the unsupported thread shim), which
                        // would kill the proxy run loop on the first accept
                        // error. Use the host-backed sleep instead. Propagate a
                        // sleep failure (`?`) rather than swallowing it: a failed
                        // host sleep would otherwise let this arm `continue` with
                        // no delay and busy-spin if `accept()` keeps erroring. The
                        // host only errs here when tearing the capsule down, so
                        // returning ends the loop cleanly.
                        astrid_sdk::time::sleep(std::time::Duration::from_millis(100))?;
                        continue;
                    }
                };
                log::info("CLI client connected to proxy");
                clients.push(ProxyClient::new(stream));
            }

            // Phase B: poll for one additional connection (non-blocking).
            // Max one per iteration to bound handshake stall to ~5s worst case.
            // The new try_accept takes a timeout - 0 means non-blocking, matching
            // the pre-#752 semantics.
            if let Ok(Some(new_stream)) = listener.try_accept(0) {
                log::info("Additional CLI client connected to proxy");
                clients.push(ProxyClient::new(new_stream));
            }

            // Phase C: read from all streams.
            // NOTE: 50ms timeout per stream = linear scaling (N*50ms per iteration).
            // Acceptable for CLI use (2-3 typical, 8 max = 400ms worst case).
            let mut dead_indices: Vec<usize> = Vec::new();
            for (i, client) in clients.iter_mut().enumerate() {
                match client.stream.try_recv() {
                    Ok(bytes) => {
                        // Apply the binding state machine and forward if allowed.
                        // The first usable message binds the connection's
                        // principal, which the outbound demux (`should_deliver`)
                        // then keys on. Connection lifecycle tracking
                        // (`client.v1.connect` / `client.v1.disconnect`) is NOT
                        // emitted here — the host owns it (see below).
                        let outcome = handle_ingress(&bytes, client.principal.as_deref());
                        if let Some(bound) = outcome.newly_bound {
                            log::info(format!("CLI connection bound to principal {bound}"));
                            client.principal = Some(bound);
                        }
                        // Track the latest conversation session so outbound
                        // responses route only to the connection on that session.
                        if let Some(session) = outcome.session_id {
                            client.session_id = Some(session);
                        }
                    }
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Closed) => {
                        log::info("CLI client disconnected from proxy");
                        dead_indices.push(i);
                    }
                }
            }

            // Remove dead streams in reverse order to preserve indices.
            // Dropping the `ProxyClient` drops its `TcpStream`, which releases
            // the host-side stream entry — and that drop is exactly where the
            // host emits `client.v1.disconnect` for the kernel connection
            // tracker, stamped with the connection's verified principal. The
            // proxy no longer emits it (the old proxy-side emission fired after
            // the connection's verified identity was gone, so the kernel
            // stamped it `anonymous` and the per-principal count leaked).
            for &i in dead_indices.iter().rev() {
                clients.remove(i);
            }

            // Phase D: poll IPC subscriptions and broadcast to all live streams.
            // NOTE: broadcast_dead indices are into clients AFTER Phase C removals.
            let mut broadcast_dead: Vec<usize> = Vec::new();
            for sub in &subs {
                match sub.poll() {
                    Ok(result) => {
                        if !result.messages.is_empty() {
                            broadcast_poll_messages(
                                &clients,
                                &result,
                                &mut stream_accum,
                                &mut broadcast_dead,
                            );
                        }
                    }
                    Err(_) => {
                        log::error("IPC subscription error, proxy shutting down");
                        break 'proxy;
                    }
                }
            }

            // Remove streams that failed during broadcast.
            // Multiple subscriptions may flag the same stream as dead in one
            // iteration. sort + dedup before removal prevents double-removal panics.
            broadcast_dead.sort_unstable();
            broadcast_dead.dedup();
            for &i in broadcast_dead.iter().rev() {
                clients.remove(i);
                log::info("CLI client disconnected during broadcast");
                // See the Phase-C removal above: the host emits
                // `client.v1.disconnect` when the dropped stream's resource is
                // torn down, so the proxy does not.
            }
        }

        // Reached only when an IPC subscription fails (break 'proxy above).
        Err(SysError::ApiError(
            "IPC subscription failed, proxy terminated".to_string(),
        ))
    }
}

/// Outcome of applying the ingress state machine to one client message — the
/// two pieces of connection state the caller folds back onto the [`ProxyClient`].
struct IngressOutcome {
    /// `Some(principal)` only when this message *binds* a previously-unbound
    /// connection, so the caller logs the bind once; `None` otherwise
    /// (already bound, malformed, dropped).
    newly_bound: Option<String>,
    /// The conversation `session_id` this message binds the connection to, set
    /// only by a forwarded chat prompt ([`CHAT_REQUEST_TOPIC`]). The caller folds
    /// it onto the connection so outbound chat responses on that session route
    /// back here. `None` for every other message (dropped, blocked, no-body, or
    /// any non-prompt topic), which leaves the connection's session unchanged.
    session_id: Option<String>,
}

/// Parse an incoming client message, apply the per-connection binding state
/// machine ([`decide_ingress`]), and forward it to the IPC bus if the binding
/// allows it and the topic passes the ingress allowlist.
///
/// `current_binding` is the connection's principal so far (`None` until the
/// first usable message binds it). Returns an [`IngressOutcome`] carrying the
/// newly-bound principal (only on the binding message) and the conversation
/// session observed on this message, both of which the caller folds onto the
/// connection. A dropped/malformed message yields an empty outcome.
fn handle_ingress(bytes: &[u8], current_binding: Option<&str>) -> IngressOutcome {
    let empty = IngressOutcome {
        newly_bound: None,
        session_id: None,
    };

    let msg = match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(v) => v,
        Err(_) => {
            log::warn("Received malformed IPC payload from socket");
            return empty;
        }
    };

    let message_principal = msg.get("principal").and_then(|p| p.as_str());

    // Resolve the binding decision first — a conflicting or malformed
    // principal is dropped before any forward, and never mutates the binding.
    let (forward_as, newly_bound) = match decide_ingress(current_binding, message_principal) {
        IngressDecision::Bind(p) => (p.clone(), Some(p)),
        IngressDecision::ForwardAs(p) => (p, None),
        IngressDecision::Drop { reason } => {
            match reason {
                DropReason::InvalidPrincipal(p) => log::warn(format!(
                    "Dropped ingress message: malformed principal {p:?}; connection stays unbound"
                )),
                DropReason::PrincipalConflict { bound, claimed } => log::warn(format!(
                    "Dropped ingress message: connection bound to {bound:?} but message claimed {claimed:?}"
                )),
            }
            return empty;
        }
    };

    let (Some(topic), Some(payload)) = (
        msg.get("topic").and_then(|t| t.as_str()),
        msg.get("payload"),
    ) else {
        // No forwardable body, but the principal still binds the connection
        // (e.g. a bare handshake establishes identity for connect-tracking).
        // Nothing is forwarded, so the connection's session is never retargeted.
        log::warn("Ingress message has no topic/payload; binding only, nothing forwarded");
        return IngressOutcome {
            newly_bound,
            session_id: None,
        };
    };

    if !is_allowed_ingress_topic(topic) {
        // A blocked-topic message is neither forwarded nor allowed to retarget
        // the connection's session — otherwise a client could spoof itself onto
        // another session's stream with an unforwarded message.
        log::warn(format!("Dropped ingress message to blocked topic: {topic}"));
        return IngressOutcome {
            newly_bound,
            session_id: None,
        };
    }

    // Always forward under the connection's bound principal. There is no
    // `publish_json` (proxy self-identity) fallback for client traffic:
    // publishing without a principal would attribute the request to the
    // proxy capsule's own (admin-seeded) identity, so any socket client
    // could run admin commands (privilege escalation) — or, if the router
    // gates on the envelope principal, every admin request would be denied
    // for lacking one. A bound connection's traffic always attributes to
    // its principal (auto-attribution for un-stamped messages).
    if let Err(e) = ipc::publish_json_as(topic, payload, &forward_as) {
        log::error(format!("Failed to publish IPC: {e:?}"));
    }

    // Learn the connection's conversation session only from a forwarded chat
    // prompt — the authoritative, allowlisted source. Latest-wins so a
    // connection that starts a new session (clear/compact) re-targets its
    // outbound demux; any other forwarded topic leaves the binding unchanged.
    IngressOutcome {
        newly_bound,
        session_id: ingress_session_bind(topic, payload).map(str::to_string),
    }
}

/// A polled IPC message ready for outbound delivery: the serialized wire bytes
/// the TUI expects, the principal it is attributed to (`None` = a
/// system/broadcast event with no principal), and the conversation session it
/// is scoped to (`None` = not session-scoped, routes by principal alone).
struct OutboundMessage {
    bytes: Vec<u8>,
    target: Option<String>,
    session: Option<String>,
}

/// Fan a `PollResult` out to connected clients, demultiplexed by principal AND
/// session so a bound connection only sees IPC stamped with its own principal
/// (plus unprincipaled system events), and a chat response only when it is on
/// that session. Tracks failed stream indices (into `clients`) in `dead`.
///
/// `stream_accum` carries streamed chat tokens across calls so a turn's terminal
/// response can be reconciled against what was already streamed live (see
/// [`reconcile_stream_payload`]).
fn broadcast_poll_messages(
    clients: &[ProxyClient],
    poll_result: &ipc::PollResult,
    stream_accum: &mut HashMap<String, String>,
    dead: &mut Vec<usize>,
) {
    if poll_result.dropped > 0 {
        log::warn(format!(
            "Event bus dropped {} messages - TUI may be stale",
            poll_result.dropped
        ));
    }

    // Pre-serialize each message once and compute its principal target once
    // (not per client). Reconstruct the wire format the TUI expects:
    // {topic, payload, source_id}.
    let outbound: Vec<OutboundMessage> = poll_result
        .messages
        .iter()
        .filter_map(|msg| {
            // Parse the payload string back to a JSON value so the TUI
            // receives an embedded object, not an escaped string.
            let mut payload = serde_json::from_str::<serde_json::Value>(&msg.payload)
                .unwrap_or(serde_json::Value::String(msg.payload.clone()));
            // Accumulate streamed tokens and reconcile the terminal response so
            // the TUI (append-then-flush) renders the reply exactly once.
            reconcile_stream_payload(&msg.topic, &mut payload, stream_accum);
            // Scope to a session only for chat responses + deltas (free of the
            // already-parsed payload) so a streamed reply routes only to the
            // connection on that session; correlated/system replies stay
            // principal-routed.
            let session = outbound_session_scope(&msg.topic, &payload).map(str::to_string);
            let bytes = serde_json::to_vec(&serde_json::json!({
                "topic": msg.topic,
                "payload": payload,
                "source_id": msg.source_id,
            }))
            .ok()?;
            Some(OutboundMessage {
                bytes,
                target: attribution_target(&msg.principal).map(str::to_string),
                session,
            })
        })
        .collect();

    for (i, client) in clients.iter().enumerate() {
        // Skip streams already marked dead by a previous subscription's broadcast.
        if dead.contains(&i) {
            continue;
        }
        for msg in &outbound {
            // Demux on principal AND session: a principaled message reaches only
            // the matching bound client, and a session-scoped one only the
            // client on that session (so same-principal connections on different
            // sessions don't cross-talk). Unprincipaled/unsessioned messages go
            // to everyone that clears the gates.
            if !should_deliver(
                msg.target.as_deref(),
                msg.session.as_deref(),
                client.principal.as_deref(),
                client.session_id.as_deref(),
            ) {
                continue;
            }
            if let Err(e) = client.stream.send(&msg.bytes) {
                log::warn(format!(
                    "Socket send error, client likely disconnected: {e:?}"
                ));
                dead.push(i);
                break; // Skip remaining messages for this dead stream.
            }
        }
    }
}

/// Exact topics a client may publish *through* the proxy to the internal bus.
///
/// `client.v1.connect` / `client.v1.disconnect` are deliberately absent: the
/// HOST emits them from the inbound-socket accept/drop path, stamped with the
/// connection's handshake-verified principal. A client (or the proxy) cannot
/// forge them — the proxy does not publish them at all, and a client-sent copy
/// over this allowlist would let an untrusted socket move the kernel's
/// connection counter.
const ALLOWED_INGRESS_EXACT: &[&str] = &["user.v1.prompt", "cli.v1.command.execute"];

/// Topic prefixes the CLI is allowed to publish (suffix-routed topics).
/// IMPORTANT: Update this list when adding new CLI-originated topic prefixes.
const ALLOWED_INGRESS_PREFIXES: &[&str] = &[
    "astrid.v1.request.",
    "astrid.v1.admin.",
    "astrid.v1.elicit.response.",
    "astrid.v1.approval.response.",
    "registry.v1.selection.",
    "session.v1.request.",
    // Provider-targeted run requests for capsule CLI verbs (astrid#891). The
    // provider id suffix means each capsule subscribes only its own command
    // traffic; the trailing dot keeps bare `cli.v1.command.run` (no provider
    // segment) blocked.
    "cli.v1.command.run.",
];

/// Prefixes a socket client may NEVER publish, even when they fall under an
/// allowed prefix above. Admin RESPONSE topics (`astrid.v1.admin.response.…`)
/// are kernel-originated; the `astrid.v1.admin.` allow-prefix is for *request*
/// topics. The allowlist is a plain `starts_with`, so without this carve-out a
/// socket client could publish `astrid.v1.admin.response.*` — spoofing or
/// flooding admin responses on the bus and racing the real kernel replies.
const BLOCKED_INGRESS_PREFIXES: &[&str] = &["astrid.v1.admin.response."];

fn is_allowed_ingress_topic(topic: &str) -> bool {
    if BLOCKED_INGRESS_PREFIXES
        .iter()
        .any(|p| topic.starts_with(p))
    {
        return false;
    }
    ALLOWED_INGRESS_EXACT.contains(&topic)
        || ALLOWED_INGRESS_PREFIXES
            .iter()
            .any(|p| topic.starts_with(p))
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
