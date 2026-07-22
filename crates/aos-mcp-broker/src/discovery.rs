//! Tool descriptor discovery and the `astrid.v1.tools.list` publish path.
//!
//! Two entry points feed the cache:
//!
//! * [`describe_tools`] — on-demand fan-out (subscribe-before-publish
//!   ordering, mirrors the registry capsule). Replaces the cached
//!   snapshot wholesale so descriptors from departed capsules don't
//!   linger. Cache-hit short-circuits the fan-out when the snapshot is
//!   fresher than [`super::cache::CACHE_TTL_MS`].
//! * [`collect_tool_descriptors`] — event-driven; merges every
//!   broadcast `tool.v1.response.describe.*` into the cache via CAS.
//!
//! Both publish `astrid.v1.tools.list` with MCP-shaped descriptors whose
//! names are prefixed `mcp__aos__<original>` so the agent runner can
//! pass them straight to Claude via `--allowed-tools mcp__aos__*`.

use astrid_sdk::prelude::*;

use crate::cache::{self, McpToolDescriptor};

/// Fan-out topic — every tool-providing capsule subscribes to this and
/// replies on its own `tool.v1.response.describe.<source_id>`.
const DESCRIBE_REQUEST_TOPIC: &str = "tool.v1.request.describe";
/// Wildcard pattern for the per-source response topics.
const DESCRIBE_RESPONSE_PATTERN: &str = "tool.v1.response.describe.*";
/// Topic on which the agent runner consumes the assembled MCP tool list.
fn tools_list_topic() -> &'static str {
    crate::profile::tools_list_topic()
}

/// MCP tool name prefix Astrid exposes to Claude. The `--allowed-tools
/// mcp__aos__*` flag on the agent subprocess matches against this.
fn mcp_tool_prefix() -> &'static str {
    crate::profile::mcp_tool_prefix()
}

/// Total drain window for the describe fan-out. Sized to cover a COLD
/// start: the first `tools/list` after a plain daemon boot races the
/// fan-out against WASM instantiation + kernel broadcast latency, and the
/// old 500 ms lost that race — which matters because an MCP client fetches
/// `tools/list` once at connect, so an empty first answer sticks for the
/// session. The loop below still returns as soon as responders go quiet
/// AFTER replying, so this is headroom for a slow cold start, not a fixed
/// wait on the warm path.
const DISCOVERY_TIMEOUT_MS: u64 = 2_500;
/// Slice size for the drain loop. A single `recv(timeout)` would only
/// pick up the first batch; the loop keeps polling in shorter slices
/// until the budget closes.
const DISCOVERY_SLICE_MS: u64 = 100;

/// Per-tool name length cap. MCP+claude accept long names but a
/// hostile capsule could publish kilobyte names — reject anything
/// past this before it reaches the cache.
const MAX_TOOL_NAME_LEN: usize = 128;
/// Per-tool description cap.
const MAX_DESCRIPTION_LEN: usize = 4_096;
/// Per-tool serialized `inputSchema` cap (JSON bytes). Bigger than a
/// realistic tool schema needs but small enough to bound the cache
/// regardless of provider behaviour.
const MAX_INPUT_SCHEMA_BYTES: usize = 16_384;
/// Per-tool serialized `capabilities` cap.
const MAX_CAPABILITIES_BYTES: usize = 2_048;
/// Hard cap on tools accepted from a single `describe` response /
/// broadcast. Caches further cap the merged state via
/// `cache::MAX_CACHED_TOOLS`.
const MAX_TOOLS_PER_RESPONSE: usize = 256;

/// Handle `astrid.v1.tools.describe`.
///
/// Cache-fresh path: republish the cached list and return. Cache-miss /
/// stale path: subscribe-before-publish fan-out, dedupe by name
/// (last-write-wins), replace the cache, publish the new list.
pub(crate) fn describe_tools() {
    // The agent-runner describe path has no proxy `req_id`; tag its fan-out
    // logs with a stable synthetic id so they are still distinguishable from
    // the broker `tools.list` path when grepping a shared log.
    let snapshot = collect_snapshot("describe_tools");
    publish_tools_list(&snapshot);
}

/// Assemble the current tool-descriptor snapshot, running the
/// describe-collect fan-out only when the cache is stale.
///
/// Shared by the agent-facing `describe_tools` publish path and the
/// broker-facing `astrid.v1.request.mcp.tools.list` handler so the two
/// front doors run the exact same discovery + cache logic — no
/// duplicated fan-out, dedupe, or TTL handling.
///
/// `req_id` tags the fan-out lifecycle logs (start / per-responder / complete
/// / incomplete) so a single `tools.list` is greppable end to end. It is a
/// log tag only — when the cache is fresh no fan-out runs and it goes unused.
pub(crate) fn collect_snapshot(req_id: &str) -> Vec<McpToolDescriptor> {
    let cached = cache::load();
    let now = wall_ms();
    if cached.is_fresh(now) {
        return cached.as_vec();
    }

    let descriptors = discover(req_id);
    match snapshot_outcome(&descriptors) {
        // Keep the prior cache untouched and let the next call retry — see
        // [`snapshot_outcome`].
        SnapshotOutcome::Keep => cached.as_vec(),
        SnapshotOutcome::Replace => cache::replace(descriptors).as_vec(),
    }
}

/// Whether a fresh fan-out result should REPLACE the cache or be discarded in
/// favour of the prior cache.
#[derive(Debug, PartialEq, Eq)]
enum SnapshotOutcome {
    /// Discard the fan-out; serve the prior cache and write nothing. An EMPTY
    /// fan-out is a cold-start race-loss (WASM instantiation + broadcast
    /// latency beat the drain window), NOT a genuine "no tools" answer:
    /// caching it would clobber a prior good snapshot and pin an empty
    /// `tools/list` for the session (the MCP client fetches it once at
    /// connect). `is_fresh` already refuses to short-circuit an empty cache,
    /// so a cold cache simply re-discovers on the next call.
    Keep,
    /// Persist the fan-out and serve it.
    Replace,
}

/// Pure reconciliation decision for [`collect_snapshot`] — the host calls
/// (load / discover / replace) stay in the caller, so this is unit-testable.
fn snapshot_outcome(discovered: &[McpToolDescriptor]) -> SnapshotOutcome {
    if discovered.is_empty() {
        SnapshotOutcome::Keep
    } else {
        SnapshotOutcome::Replace
    }
}

/// Convert internal descriptors to the standard MCP `tools/list`
/// descriptor shape (`name`, `description`, `inputSchema`, plus optional
/// `title`/`capabilities`) for the broker reply body.
///
/// Unlike [`publish_tools_list`], names are emitted RAW — the broker is
/// a generic MCP front door, not Claude's `mcp__aos__*` namespace, so
/// it must not stamp the agent-runner prefix onto the descriptors a
/// third-party MCP client consumes.
pub(crate) fn to_mcp_descriptors(descriptors: &[McpToolDescriptor]) -> Vec<serde_json::Value> {
    descriptors.iter().map(mcp_descriptor).collect()
}

/// Shape one internal descriptor into an MCP tool-descriptor object.
/// `prefix` is prepended to the name (empty for the broker surface,
/// `mcp__aos__` for the agent-runner surface).
fn mcp_descriptor_with_prefix(d: &McpToolDescriptor, prefix: &str) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert(
        "name".to_string(),
        serde_json::Value::String(format!("{prefix}{}", d.name)),
    );
    if let Some(title) = &d.title {
        obj.insert(
            "title".to_string(),
            serde_json::Value::String(title.clone()),
        );
    }
    obj.insert(
        "description".to_string(),
        serde_json::Value::String(d.description.clone()),
    );
    obj.insert("inputSchema".to_string(), d.input_schema.clone());
    if let Some(caps) = &d.capabilities {
        obj.insert("capabilities".to_string(), caps.clone());
    }
    serde_json::Value::Object(obj)
}

/// Broker-surface MCP descriptor: raw (unprefixed) name.
fn mcp_descriptor(d: &McpToolDescriptor) -> serde_json::Value {
    mcp_descriptor_with_prefix(d, "")
}

/// Handle an inbound `tool.v1.response.describe.*` broadcast.
///
/// The kernel routes every matching message to this action. We
/// extract descriptors, merge them into the cache via CAS, and
/// re-publish the assembled list so downstream consumers see fresh
/// additions immediately.
pub(crate) fn collect_tool_descriptors(payload: serde_json::Value) {
    let descriptors = parse_describe_response(&payload);
    if descriptors.is_empty() {
        return;
    }

    let merged = cache::upsert(descriptors).as_vec();
    publish_tools_list(&merged);
}

/// Handle the kernel's `astrid.v1.capsules_loaded` signal.
///
/// The kernel includes each loaded capsule's installed `meta.json` (opaque) in
/// the payload under `capsules[].meta`. When EVERY loaded capsule has a
/// **captured** tool surface (`meta.tools` present — whether `[..]` or `[]`),
/// the broker rebuilds its cache purely from that static set: deterministic, no
/// describe fan-out, so the first `tools/list` after boot is already complete
/// (this is where the cold-start fan-out race dies). If ANY capsule's surface
/// is uncaptured (built before tool-baking, or its `meta` could not be read),
/// the static set would be incomplete, so we fall back to invalidating the
/// cache and let the next `tools/list` re-discover via the fan-out. As the
/// fleet rebuilds with baked tools it tips into the deterministic path
/// automatically, with no missing-tool window in between.
pub(crate) fn on_capsules_loaded(payload: serde_json::Value) {
    match static_tools_from_payload(&payload) {
        Some(descriptors) => {
            // Authoritative, complete surface — replace the cache and publish
            // so the agent-runner view refreshes too (the broker shim
            // separately re-fetches `tools/list` on this same signal).
            let count = descriptors.len();
            let snapshot = cache::replace(descriptors).as_vec();
            log::info(format!(
                "{}: tool cache rebuilt from static capsule metadata ({count} tools); describe fan-out skipped",
                crate::profile::log_tag()
            ));
            publish_tools_list(&snapshot);
        }
        None => {
            // At least one uncaptured surface — discover at runtime.
            log::info(format!(
                "{}: capsule-set change includes an uncaptured tool surface; cache invalidated for fan-out rediscovery",
                crate::profile::log_tag()
            ));
            cache::invalidate();
        }
    }
}

/// Assemble the union of every loaded capsule's build-captured tool descriptors
/// from a `capsules_loaded` payload — but only if EVERY capsule's surface is
/// captured.
///
/// Returns `None` (caller falls back to the fan-out) when the payload predates
/// the enriched-metadata kernel (no `capsules` array), or when any capsule's
/// `meta` is missing / `null` (the kernel could not read it) or carries no
/// `tools` array (a capsule built before tool-baking). A capsule with
/// `tools: []` is authoritatively tool-less — it contributes nothing without
/// disqualifying the static path.
fn static_tools_from_payload(payload: &serde_json::Value) -> Option<Vec<McpToolDescriptor>> {
    let capsules = payload.get("capsules")?.as_array()?;
    let mut all = Vec::new();
    for capsule in capsules {
        // `meta` absent / null => the kernel could not read this capsule's
        // `meta.json` => unknown surface => disqualify the static path.
        let meta = capsule.get("meta").filter(|m| !m.is_null())?;
        // `tools` absent / non-array => not captured => unknown.
        let tools = meta.get("tools")?.as_array()?;
        for tool in tools {
            let Some(name) = tool.get("name").and_then(serde_json::Value::as_str) else {
                continue;
            };
            all.push(McpToolDescriptor {
                name: name.to_string(),
                title: None,
                description: tool
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                input_schema: tool
                    .get("input_schema")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
                capabilities: None,
            });
        }
    }
    Some(all)
}

/// Subscribe-before-publish fan-out. Mirrors the registry capsule
/// pattern: open the subscription, fire the empty `{}` request, drain
/// up to `DISCOVERY_TIMEOUT_MS` in `DISCOVERY_SLICE_MS` slices.
///
/// `req_id` is the correlation token for the originating describe request
/// (the broker's `req_id` for a `tools.list`, or a synthesized id for the
/// agent-runner `describe_tools` path) so the whole fan-out lifecycle —
/// start, each responder collected, completion, and any timeout/drop — is
/// greppable end to end. It is a LOG TAG only and never reaches the wire.
fn discover(req_id: &str) -> Vec<McpToolDescriptor> {
    let started = wall_ms();
    let sub = match ipc::subscribe(DESCRIBE_RESPONSE_PATTERN) {
        Ok(s) => s,
        Err(e) => {
            log::warn(format!(
                "{}: broker fan-out subscribe failed req_id={req_id} \
                 topic={DESCRIBE_RESPONSE_PATTERN}: {e}",
                crate::profile::log_tag()
            ));
            return Vec::new();
        }
    };

    log::info(format!(
        "{}: broker fan-out start req_id={req_id} deadline_ms={DISCOVERY_TIMEOUT_MS}",
        crate::profile::log_tag()
    ));

    if let Err(e) = ipc::publish(DESCRIBE_REQUEST_TOPIC, "{}") {
        log::warn(format!(
            "{}: broker fan-out publish failed req_id={req_id} \
             topic={DESCRIBE_REQUEST_TOPIC}: {e}",
            crate::profile::log_tag()
        ));
        return Vec::new();
    }

    let mut acc: Vec<McpToolDescriptor> = Vec::new();
    let mut seen_any = false;
    // Count of distinct responder source_ids whose describe broadcast we
    // collected, plus the cumulative dropped/lagged signal the bus reports —
    // these feed the completion / incomplete WARN below so a first-call
    // fan-out drop is visible rather than silent.
    let mut responders: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut dropped: u64 = 0;
    let mut lagged: u64 = 0;
    let mut remaining = DISCOVERY_TIMEOUT_MS;
    loop {
        let step = remaining.min(DISCOVERY_SLICE_MS);
        match sub.recv(step) {
            Ok(result) if !result.messages.is_empty() => {
                seen_any = true;
                dropped = dropped.saturating_add(result.dropped);
                lagged = lagged.saturating_add(result.lagged);
                for msg in &result.messages {
                    let Ok(value) = serde_json::from_str::<serde_json::Value>(&msg.payload) else {
                        continue;
                    };
                    let tools = parse_describe_response(&value);
                    responders.insert(msg.source_id.clone());
                    log::debug(format!(
                        "{}: broker fan-out collected req_id={req_id} \
                         responder={} tool_count={}",
                        crate::profile::log_tag(),
                        msg.source_id,
                        tools.len()
                    ));
                    acc.extend(tools);
                }
            }
            // A quiet slice — the host returned early-empty, OR `recv` timed
            // out on this slice. Two cases: if responders have ALREADY replied,
            // this is quiescence → stop. If NOT, we're still racing a cold
            // start (WASM instantiation + broadcast latency), so keep polling
            // until the budget closes rather than breaking at the first 100 ms
            // gap — otherwise widening `DISCOVERY_TIMEOUT_MS` buys nothing,
            // because the loop would bail before the first response arrives.
            Ok(result) => {
                // Quiet but non-error slice may still carry drop/lag counters.
                dropped = dropped.saturating_add(result.dropped);
                lagged = lagged.saturating_add(result.lagged);
                if seen_any {
                    break;
                }
            }
            Err(_) => {
                if seen_any {
                    break;
                }
            }
        }
        remaining = remaining.saturating_sub(step);
        if remaining == 0 {
            break;
        }
    }

    // Dedupe by name, last-write-wins. We iterate in reverse so the
    // final retain preserves the last occurrence.
    acc.reverse();
    let mut seen = std::collections::HashSet::new();
    acc.retain(|d| seen.insert(d.name.clone()));
    acc.reverse();

    let elapsed = wall_ms().saturating_sub(started);
    let timed_out = remaining == 0;
    // Incomplete iff the bus dropped/lagged messages (a responder's reply was
    // missed), OR we exhausted the whole budget without quiescence (a slow /
    // never-replying responder — the known first-call fan-out drop). Either
    // way the answer below may be missing tools, so surface it at WARN rather
    // than letting an under-count look like a clean result.
    if dropped > 0 || lagged > 0 || (timed_out && !seen_any) {
        log::warn(format!(
            "{}: broker fan-out incomplete req_id={req_id} responders={} tools={} \
             dropped={dropped} lagged={lagged} timed_out={timed_out} elapsed_ms={elapsed}",
            crate::profile::log_tag(),
            responders.len(),
            acc.len()
        ));
    } else {
        log::info(format!(
            "{}: broker fan-out complete req_id={req_id} responders={} tools={} \
             elapsed_ms={elapsed}",
            crate::profile::log_tag(),
            responders.len(),
            acc.len()
        ));
    }

    acc
}

/// Extract descriptors from a `tool.v1.response.describe.*` payload.
///
/// Honours both the direct envelope (`{ "tools": [...] }`, emitted by
/// the SDK `tool_describe` macro arm) and the wrapped Custom envelope
/// (`{ "data": { "tools": [...] } }`). Each entry is deserialized
/// independently — malformed entries are skipped without aborting the
/// whole response. Untrusted-input gates:
///
/// * names that are empty or don't match the `^[A-Za-z0-9_.-]+$`
///   charset are dropped (prevents path-style names, unicode bidi
///   overrides, control chars from reaching the wire);
/// * descriptors with oversized name / description / schema /
///   capabilities are dropped — a hostile broadcaster cannot DoS the
///   cache by inflating individual entries;
/// * the per-response array is hard-capped at
///   [`MAX_TOOLS_PER_RESPONSE`].
fn parse_describe_response(value: &serde_json::Value) -> Vec<McpToolDescriptor> {
    let tools = value
        .get("tools")
        .or_else(|| value.get("data").and_then(|d| d.get("tools")))
        .and_then(|t| t.as_array());

    let Some(tools) = tools else {
        return Vec::new();
    };

    let take = tools.len().min(MAX_TOOLS_PER_RESPONSE);
    let mut out = Vec::with_capacity(take);
    for raw in tools.iter().take(take) {
        let Ok(desc) = serde_json::from_value::<McpToolDescriptor>(remap_input_schema(raw.clone()))
        else {
            continue;
        };
        if !is_valid_descriptor(&desc) {
            continue;
        }
        out.push(desc);
    }
    out
}

/// Allowed tool-name charset. Matches `^[A-Za-z0-9_.-]+$` — same shape
/// MCP uses for tool identifiers. Anything else (path separators,
/// whitespace, unicode bidi, control chars) is rejected.
fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_TOOL_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// Full descriptor validation: charset on the name, byte-size caps on
/// description / schema / capabilities. Discard anything that violates;
/// the broadcaster has no claim on cache state.
fn is_valid_descriptor(desc: &McpToolDescriptor) -> bool {
    if !is_valid_name(&desc.name) {
        return false;
    }
    if desc.description.len() > MAX_DESCRIPTION_LEN {
        return false;
    }
    if let Some(t) = &desc.title
        && t.len() > MAX_DESCRIPTION_LEN
    {
        return false;
    }
    if json_byte_len(&desc.input_schema) > MAX_INPUT_SCHEMA_BYTES {
        return false;
    }
    if let Some(caps) = &desc.capabilities
        && json_byte_len(caps) > MAX_CAPABILITIES_BYTES
    {
        return false;
    }
    true
}

/// Serialized-byte length of a JSON value. Used for hostile-payload
/// size checks. `serde_json::to_vec` on an in-memory `Value` should
/// not fail in practice; treat a serializer error as "oversized" so
/// the guard remains fail-closed.
fn json_byte_len(value: &serde_json::Value) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |v| v.len())
}

/// SDK-generated tool schemas use the field name `input_schema` while
/// MCP uses `inputSchema`. Accept both at parse time by renaming on
/// the fly so downstream code can rely on the MCP shape.
fn remap_input_schema(mut value: serde_json::Value) -> serde_json::Value {
    if let Some(obj) = value.as_object_mut()
        && !obj.contains_key("inputSchema")
        && let Some(schema) = obj.remove("input_schema")
    {
        obj.insert("inputSchema".to_string(), schema);
    }
    value
}

/// Publish the assembled MCP tool list. Names are prefixed
/// `mcp__aos__<original>` so the agent runner can pass them through
/// `--allowed-tools mcp__aos__*`. The cache stores raw names; the
/// prefix is purely a wire concern for the agent-facing topic.
fn publish_tools_list(descriptors: &[McpToolDescriptor]) {
    let mcp_shaped: Vec<serde_json::Value> = descriptors
        .iter()
        .map(|d| mcp_descriptor_with_prefix(d, mcp_tool_prefix()))
        .collect();

    if let Err(e) = ipc::publish_json(tools_list_topic(), &mcp_shaped) {
        let topic = tools_list_topic();
        log::warn(format!(
            "{}: failed to publish {topic}: {e}",
            crate::profile::log_tag()
        ));
    }
}

/// Wall-clock millis used for cache TTL bookkeeping and lifecycle-log
/// elapsed measurement. `pub(crate)` so the broker / execute paths measure
/// elapsed against the same monotone-ish wall clock — one definition.
pub(crate) fn wall_ms() -> u64 {
    time::now()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    fn install_test_profile() {
        crate::profile::install_aos();
    }

    use serde_json::json;

    use super::*;

    #[test]
    fn static_tools_all_captured_unions_descriptors() {
        install_test_profile();
        // Two capsules, both captured: one with a tool, one authoritatively
        // tool-less (`tools: []`). The union is the single tool; the empty
        // capsule does not disqualify the static path.
        let payload = json!({
            "status": "ready",
            "capsules": [
                { "name": "fs", "meta": { "tools": [
                    { "name": "read_file", "description": "Read a file",
                      "input_schema": { "type": "object" } }
                ] } },
                { "name": "cli", "meta": { "tools": [] } },
            ],
        });
        let tools = static_tools_from_payload(&payload).expect("all captured -> Some");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "read_file");
        assert_eq!(tools[0].description, "Read a file");
        assert_eq!(tools[0].input_schema, json!({ "type": "object" }));
    }

    #[test]
    fn static_tools_any_uncaptured_is_none() {
        install_test_profile();
        // One captured, one with no `tools` key (built before tool-baking).
        let payload = json!({
            "status": "ready",
            "capsules": [
                { "name": "fs", "meta": { "tools": [ { "name": "read_file" } ] } },
                { "name": "legacy", "meta": { "version": "0.1.0" } },
            ],
        });
        assert!(static_tools_from_payload(&payload).is_none());
    }

    #[test]
    fn static_tools_null_meta_is_none() {
        install_test_profile();
        // The kernel could not read this capsule's meta.json.
        let payload = json!({
            "status": "ready",
            "capsules": [ { "name": "broken", "meta": serde_json::Value::Null } ],
        });
        assert!(static_tools_from_payload(&payload).is_none());
    }

    #[test]
    fn static_tools_null_tools_is_none() {
        install_test_profile();
        // Explicit `tools: null` is the wire form of `None` (uncaptured).
        let payload = json!({
            "status": "ready",
            "capsules": [ { "name": "x", "meta": { "tools": serde_json::Value::Null } } ],
        });
        assert!(static_tools_from_payload(&payload).is_none());
    }

    #[test]
    fn static_tools_legacy_payload_without_capsules_is_none() {
        install_test_profile();
        // A pre-enrichment kernel publishes a bare `{status:"ready"}`.
        let payload = json!({ "status": "ready" });
        assert!(static_tools_from_payload(&payload).is_none());
    }

    #[test]
    fn static_tools_no_capsules_loaded_is_empty_some() {
        install_test_profile();
        // An empty (but present) capsules array is "all captured, zero tools".
        let payload = json!({ "status": "ready", "capsules": [] });
        assert_eq!(static_tools_from_payload(&payload), Some(Vec::new()));
    }

    fn desc(name: &str) -> McpToolDescriptor {
        McpToolDescriptor {
            name: name.to_string(),
            title: None,
            description: String::new(),
            input_schema: serde_json::Value::Null,
            capabilities: None,
        }
    }

    /// Regression for the `tools/list` empty-cache pin: a cold-start
    /// race-loss returns no descriptors, which must KEEP the prior cache and
    /// never replace it. Replacing on empty clobbered a good snapshot and
    /// pinned an empty `tools/list` for the whole session (the MCP client
    /// fetches the list once at connect).
    #[test]
    fn empty_fanout_keeps_cache() {
        install_test_profile();
        assert_eq!(snapshot_outcome(&[]), SnapshotOutcome::Keep);
    }

    /// A non-empty fan-out is a genuine answer and replaces the cache.
    #[test]
    fn non_empty_fanout_replaces_cache() {
        install_test_profile();
        assert_eq!(
            snapshot_outcome(&[desc("fs.read")]),
            SnapshotOutcome::Replace
        );
    }
}
