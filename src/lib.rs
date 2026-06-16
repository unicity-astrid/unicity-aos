use astrid_sdk::net::{TcpStream, TryRecvError, bind_unix};
use astrid_sdk::prelude::*;

#[derive(Default)]
struct CliProxy;

/// A connected CLI client bound to exactly one principal.
///
/// A connection binds on its first ingress message and stays bound to that
/// single principal for its whole lifetime (one connection = one principal,
/// per `unicity-astrid/astrid#852`):
///
/// * First message carrying a valid `principal` binds to it.
/// * First message with no `principal` binds to `"default"` (auto-attribution
///   for un-stamped clients, which keeps the kernel connection tracker from
///   undercounting them).
/// * A first message whose principal is malformed is dropped and the
///   connection stays `None` (unbound) so a later well-formed message can bind.
///
/// Once bound, all of this connection's traffic attributes to its principal,
/// and it only receives outbound IPC stamped with that same principal (plus
/// unprincipaled system events). `principal` stays `None` only for a connection
/// that has not yet sent a usable message; such a connection is uncounted and
/// receives only unprincipaled events.
struct ProxyClient {
    stream: TcpStream,
    principal: Option<String>,
}

impl ProxyClient {
    fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            principal: None,
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

/// Pure outbound-demux decision: should an IPC message stamped with
/// `msg_principal` be delivered to a client bound to `client_binding`?
///
/// * Message principal `Some(p)` -> only clients bound to exactly `p`.
/// * Message principal `None` (system/broadcast) -> every client, including
///   still-unbound ones.
fn should_deliver(msg_principal: Option<&str>, client_binding: Option<&str>) -> bool {
    match msg_principal {
        None => true,
        Some(p) => client_binding == Some(p),
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
            "agent.v1.response",
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
        // (see `ProxyClient` / `decide_ingress`) and stays bound for life. A
        // connection's ingress always attributes to its bound principal; its
        // egress is demuxed so it only receives IPC stamped with that principal
        // (plus unprincipaled system events). There is no cross-principal
        // leakage in either direction and no broadcast-to-all of principaled
        // traffic.
        //
        // TcpStream is the post-#752 unified handle (Unix-domain accepts and
        // outbound TCP share the same resource type). Drop releases the
        // kernel-side stream entry, so we no longer need a manual close.
        //
        // The proxy is the authority on socket lifecycle: it emits
        // `client.v1.connect` once a connection binds and the matching
        // `client.v1.disconnect` when the socket closes, both stamped with the
        // bound principal. The kernel connection tracker turns those into the
        // per-principal active-connection count that drives ephemeral
        // idle-shutdown and `astrid who`.
        let mut clients: Vec<ProxyClient> = Vec::new();

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
                        // Apply the binding state machine, forward if allowed,
                        // and emit `client.v1.connect` exactly once when the
                        // connection first binds.
                        if let Some(bound) = handle_ingress(&bytes, client.principal.as_deref()) {
                            log::info(format!("CLI connection bound to principal {bound}"));
                            publish_client_connect(&bound);
                            client.principal = Some(bound);
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
            // Drop releases the host-side active_streams entry automatically -
            // no explicit close() needed (the pre-#752 manual close was a
            // workaround for the lack of resource Drop in the old ABI).
            for &i in dead_indices.iter().rev() {
                let client = clients.remove(i);
                announce_disconnect(&client, "socket closed");
            }

            // Phase D: poll IPC subscriptions and broadcast to all live streams.
            // NOTE: broadcast_dead indices are into clients AFTER Phase C removals.
            let mut broadcast_dead: Vec<usize> = Vec::new();
            for sub in &subs {
                match sub.poll() {
                    Ok(result) => {
                        if !result.messages.is_empty() {
                            broadcast_poll_messages(&clients, &result, &mut broadcast_dead);
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
                let client = clients.remove(i);
                log::info("CLI client disconnected during broadcast");
                announce_disconnect(&client, "broadcast send failed");
            }
        }

        // Reached only when an IPC subscription fails (break 'proxy above).
        Err(SysError::ApiError(
            "IPC subscription failed, proxy terminated".to_string(),
        ))
    }
}

/// Parse an incoming client message, apply the per-connection binding state
/// machine ([`decide_ingress`]), and forward it to the IPC bus if the binding
/// allows it and the topic passes the ingress allowlist.
///
/// `current_binding` is the connection's principal so far (`None` until the
/// first usable message binds it). Returns `Some(principal)` only when this
/// message *binds* a previously-unbound connection, so the caller can emit
/// `client.v1.connect` exactly once; returns `None` in every other case
/// (already bound, malformed, dropped).
fn handle_ingress(bytes: &[u8], current_binding: Option<&str>) -> Option<String> {
    let msg = match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(v) => v,
        Err(_) => {
            log::warn("Received malformed IPC payload from socket");
            return None;
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
            return None;
        }
    };

    let (Some(topic), Some(payload)) = (
        msg.get("topic").and_then(|t| t.as_str()),
        msg.get("payload"),
    ) else {
        // No forwardable body, but the principal still binds the connection
        // (e.g. a bare handshake establishes identity for connect-tracking).
        log::warn("Ingress message has no topic/payload; binding only, nothing forwarded");
        return newly_bound;
    };

    if is_allowed_ingress_topic(topic) {
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
    } else {
        log::warn(format!("Dropped ingress message to blocked topic: {topic}"));
    }

    newly_bound
}

/// Publish `client.v1.connect` stamped with the authenticated principal so
/// the kernel connection tracker increments that principal's active count.
fn publish_client_connect(principal: &str) {
    if let Err(e) = ipc::publish_json_as("client.v1.connect", &serde_json::json!({}), principal) {
        log::error(format!("Failed to publish client.v1.connect: {e:?}"));
    }
}

/// Publish `client.v1.disconnect` (with a reason) for a connection that bound
/// to a principal. No-op for a socket that never sent a usable message (never
/// bound, never counted), so there is nothing to decrement.
fn announce_disconnect(client: &ProxyClient, reason: &str) {
    let Some(principal) = client.principal.as_deref() else {
        return;
    };
    if let Err(e) = ipc::publish_json_as(
        "client.v1.disconnect",
        &serde_json::json!({ "reason": reason }),
        principal,
    ) {
        log::error(format!("Failed to publish client.v1.disconnect: {e:?}"));
    }
}

/// A polled IPC message ready for outbound delivery: the serialized wire bytes
/// the TUI expects, plus the principal it is attributed to (`None` = a
/// system/broadcast event with no principal).
struct OutboundMessage {
    bytes: Vec<u8>,
    target: Option<String>,
}

/// Fan a `PollResult` out to connected clients, demultiplexed by principal so a
/// bound connection only sees IPC stamped with its own principal (plus
/// unprincipaled system events). Tracks failed stream indices (into `clients`)
/// in `dead`.
fn broadcast_poll_messages(
    clients: &[ProxyClient],
    poll_result: &ipc::PollResult,
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
            let payload = serde_json::from_str::<serde_json::Value>(&msg.payload)
                .unwrap_or(serde_json::Value::String(msg.payload.clone()));
            let bytes = serde_json::to_vec(&serde_json::json!({
                "topic": msg.topic,
                "payload": payload,
                "source_id": msg.source_id,
            }))
            .ok()?;
            Some(OutboundMessage {
                bytes,
                target: attribution_target(&msg.principal).map(str::to_string),
            })
        })
        .collect();

    for (i, client) in clients.iter().enumerate() {
        // Skip streams already marked dead by a previous subscription's broadcast.
        if dead.contains(&i) {
            continue;
        }
        for msg in &outbound {
            // Demux: deliver a principaled message only to the matching bound
            // client; unprincipaled (system) messages go to everyone.
            if !should_deliver(msg.target.as_deref(), client.principal.as_deref()) {
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
/// proxy is the authority on socket lifecycle and emits them itself (see
/// [`publish_client_connect`] / [`announce_disconnect`]) keyed to the
/// stream's authenticated principal. Forwarding a client-sent copy would
/// double-count and would miss ungraceful disconnects (the socket dies without
/// the client getting a chance to send anything).
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
mod tests {
    use super::*;

    // --- principal format validation ---

    #[test]
    fn valid_principals_accepted() {
        assert!(is_valid_principal("default"));
        assert!(is_valid_principal("alice"));
        assert!(is_valid_principal("user_01-A"));
        assert!(is_valid_principal("x")); // 1 char
        assert!(is_valid_principal(&"a".repeat(64))); // boundary: 64 chars
    }

    #[test]
    fn invalid_principals_rejected() {
        assert!(!is_valid_principal("")); // empty
        assert!(!is_valid_principal(&"a".repeat(65))); // too long
        assert!(!is_valid_principal("has space"));
        assert!(!is_valid_principal("dot.sep"));
        assert!(!is_valid_principal("slash/x"));
        assert!(!is_valid_principal("emoji\u{1f600}"));
    }

    // --- ingress binding state machine ---

    #[test]
    fn first_message_with_valid_principal_binds() {
        assert_eq!(
            decide_ingress(None, Some("alice")),
            IngressDecision::Bind("alice".to_string())
        );
    }

    #[test]
    fn first_message_without_principal_binds_default() {
        assert_eq!(
            decide_ingress(None, None),
            IngressDecision::Bind(DEFAULT_PRINCIPAL.to_string())
        );
    }

    #[test]
    fn first_message_with_invalid_principal_drops_and_stays_unbound() {
        assert_eq!(
            decide_ingress(None, Some("bad principal")),
            IngressDecision::Drop {
                reason: DropReason::InvalidPrincipal("bad principal".to_string())
            }
        );
    }

    #[test]
    fn bound_connection_without_principal_forwards_as_bound() {
        // Auto-attribution: un-stamped traffic rides the bound principal.
        assert_eq!(
            decide_ingress(Some("alice"), None),
            IngressDecision::ForwardAs("alice".to_string())
        );
    }

    #[test]
    fn bound_connection_matching_principal_forwards() {
        assert_eq!(
            decide_ingress(Some("alice"), Some("alice")),
            IngressDecision::ForwardAs("alice".to_string())
        );
    }

    #[test]
    fn bound_connection_conflicting_principal_drops_without_rebind() {
        // Conflict drops the message and yields no Bind/ForwardAs, so the
        // caller never mutates the binding.
        assert_eq!(
            decide_ingress(Some("alice"), Some("mallory")),
            IngressDecision::Drop {
                reason: DropReason::PrincipalConflict {
                    bound: "alice".to_string(),
                    claimed: "mallory".to_string(),
                }
            }
        );
    }

    #[test]
    fn post_conflict_connection_still_forwards_as_original() {
        // After a conflict (binding unchanged), a subsequent matching/empty
        // message still forwards under the original principal.
        let binding = Some("alice");
        let _conflict = decide_ingress(binding, Some("mallory"));
        assert_eq!(
            decide_ingress(binding, None),
            IngressDecision::ForwardAs("alice".to_string())
        );
        assert_eq!(
            decide_ingress(binding, Some("alice")),
            IngressDecision::ForwardAs("alice".to_string())
        );
    }

    // --- outbound demux decision ---

    #[test]
    fn principaled_message_delivers_only_to_matching_bound_client() {
        assert!(should_deliver(Some("alice"), Some("alice")));
        assert!(!should_deliver(Some("alice"), Some("bob")));
    }

    #[test]
    fn principaled_message_not_delivered_to_unbound_client() {
        assert!(!should_deliver(Some("alice"), None));
    }

    #[test]
    fn unprincipaled_message_delivers_to_everyone() {
        assert!(should_deliver(None, Some("alice")));
        assert!(should_deliver(None, None)); // even an unbound client
    }

    // --- attribution target mapping ---

    #[test]
    fn attribution_target_extracts_principal_for_verified_and_claimed() {
        assert_eq!(
            attribution_target(&ipc::PrincipalAttribution::Verified("alice".to_string())),
            Some("alice")
        );
        assert_eq!(
            attribution_target(&ipc::PrincipalAttribution::Claimed("bob".to_string())),
            Some("bob")
        );
    }

    #[test]
    fn attribution_target_is_none_for_system() {
        assert_eq!(attribution_target(&ipc::PrincipalAttribution::System), None);
    }
}
