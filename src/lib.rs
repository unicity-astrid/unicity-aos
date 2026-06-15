use astrid_sdk::net::{TcpStream, TryRecvError, bind_unix};
use astrid_sdk::prelude::*;

#[derive(Default)]
struct CliProxy;

/// A connected CLI client and the authenticated principal it presented.
///
/// `principal` is learned from the first principal-stamped ingress message
/// (the socket client stamps `IpcMessage.principal`), and stays `None` for a
/// pre-handshake / anonymous socket — those are not counted as connections.
/// It is the key the proxy uses to emit `client.v1.connect` once and the
/// matching `client.v1.disconnect` when the socket closes.
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
        // IPC events are broadcast to all connected clients. Any authenticated
        // client can send prompts - the daemon is a single agent.
        //
        // TcpStream is the post-#752 unified handle (Unix-domain accepts and
        // outbound TCP share the same resource type). Drop releases the
        // kernel-side stream entry, so we no longer need a manual close.
        //
        // The proxy is the authority on socket lifecycle: it emits
        // `client.v1.connect` once a client authenticates and the matching
        // `client.v1.disconnect` when the socket closes, both stamped with the
        // client's principal. The kernel connection tracker turns those into
        // the per-principal active-connection count that drives ephemeral
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
                        // Forward the message (if allowed) and learn the sender's
                        // principal. The first principal-stamped message marks the
                        // connection as established for that principal.
                        if let Some(principal) = handle_ingress(&bytes)
                            && client.principal.is_none()
                        {
                            log::info(format!("CLI client authenticated as principal {principal}"));
                            publish_client_connect(&principal);
                            client.principal = Some(principal);
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

/// Parse an incoming client message, forward it to the IPC bus if the topic
/// passes the ingress allowlist, and return the sender's principal (if the
/// envelope carried one) so the caller can track the connection.
fn handle_ingress(bytes: &[u8]) -> Option<String> {
    let msg = match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(v) => v,
        Err(_) => {
            log::warn("Received malformed IPC payload from socket");
            return None;
        }
    };

    // Surface the principal even when the message has no forwardable
    // topic/payload (e.g. a bare handshake) — it still establishes identity.
    let principal = msg
        .get("principal")
        .and_then(|p| p.as_str())
        .map(str::to_string);

    let (Some(topic), Some(payload)) = (
        msg.get("topic").and_then(|t| t.as_str()),
        msg.get("payload"),
    ) else {
        log::warn("Dropped ingress message: missing topic or payload");
        return principal;
    };

    if is_allowed_ingress_topic(topic) {
        // Forward under the client's authenticated principal so the kernel's
        // admin / capability gating sees the real caller. Publishing without it
        // attributes the request to the proxy capsule's own (admin-seeded)
        // identity — so any authenticated socket client could run admin
        // commands (privilege escalation) — or, if the router gates on the
        // envelope principal, denies every admin request because it's missing.
        let res = match principal.as_deref() {
            Some(p) => ipc::publish_json_as(topic, payload, p),
            None => ipc::publish_json(topic, payload),
        };
        if let Err(e) = res {
            log::error(format!("Failed to publish IPC: {e:?}"));
        }
    } else {
        log::warn(format!("Dropped ingress message to blocked topic: {topic}"));
    }

    principal
}

/// Publish `client.v1.connect` stamped with the authenticated principal so
/// the kernel connection tracker increments that principal's active count.
fn publish_client_connect(principal: &str) {
    if let Err(e) = ipc::publish_json_as("client.v1.connect", &serde_json::json!({}), principal) {
        log::error(format!("Failed to publish client.v1.connect: {e:?}"));
    }
}

/// Publish `client.v1.disconnect` (with a reason) for a client that has
/// authenticated. No-op for a socket that never presented a principal — it was
/// never counted, so there is nothing to decrement.
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

/// Broadcast each IPC message from a `PollResult` to every connected stream.
/// Tracks failed stream indices (into `clients`) in `dead`.
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

    // Pre-serialize each message once, then write to all streams.
    // Reconstruct the wire format the TUI expects: {topic, payload, source_id}.
    let serialized: Vec<Vec<u8>> = poll_result
        .messages
        .iter()
        .filter_map(|msg| {
            // Parse the payload string back to a JSON value so the TUI
            // receives an embedded object, not an escaped string.
            let payload = serde_json::from_str::<serde_json::Value>(&msg.payload)
                .unwrap_or(serde_json::Value::String(msg.payload.clone()));
            serde_json::to_vec(&serde_json::json!({
                "topic": msg.topic,
                "payload": payload,
                "source_id": msg.source_id,
            }))
            .ok()
        })
        .collect();

    for (i, client) in clients.iter().enumerate() {
        // Skip streams already marked dead by a previous subscription's broadcast.
        if dead.contains(&i) {
            continue;
        }
        for msg_bytes in &serialized {
            if let Err(e) = client.stream.send(msg_bytes) {
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
