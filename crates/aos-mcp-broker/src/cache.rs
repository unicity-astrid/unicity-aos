//! KV-backed tool descriptor cache.
//!
//! The cache is keyed by tool name (last-write-wins on duplicates) and
//! carries a monotonic-ish timestamp so [`super::discovery::describe_tools`]
//! can short-circuit on a fresh cache hit. Updates use `kv::cas` so the
//! event-driven `collect_tool_descriptors` handler and the on-demand
//! `describe_tools` fan-out don't clobber each other on concurrent writes.

use std::collections::BTreeMap;

use astrid_sdk::prelude::*;
use serde::{Deserialize, Serialize};

/// KV key for the cached tool descriptor map.
pub(crate) const CACHE_KEY: &str = "tools.cache";

/// Cache freshness window for `describe_tools` short-circuit. Anything
/// older than this triggers a fresh fan-out.
pub(crate) const CACHE_TTL_MS: u64 = 60_000;

/// CAS retry budget for cache updates. The cache is contended by the
/// describe fan-out and the broadcast collector; a handful of retries
/// covers contention without unbounded looping.
const CAS_MAX_RETRIES: u32 = 8;

/// Hard cap on cached descriptors. A hostile capsule broadcasting a
/// flood of fake tools cannot bloat the persistent cache past this.
/// On overflow the upsert path silently drops trailing entries from
/// the merged set — discovery will re-publish what survived.
pub(crate) const MAX_CACHED_TOOLS: usize = 512;

/// MCP tool descriptor.
///
/// Field names match the MCP `tools/list` response shape so the agent
/// runner can forward this verbatim. `capabilities` is host-specific
/// metadata (the source capsule's capability discriminator) and is
/// preserved through the cache for downstream capability checks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct McpToolDescriptor {
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) description: String,
    #[serde(rename = "inputSchema", default)]
    pub(crate) input_schema: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) capabilities: Option<serde_json::Value>,
}

/// On-disk cache envelope. `BTreeMap` for stable serialization order
/// (CAS depends on byte-equality of the prior value).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CacheState {
    /// Wall-clock millis (host clock) when this snapshot was last updated.
    pub(crate) updated_at_ms: u64,
    /// Descriptors keyed by tool name. Last-write-wins on collision.
    pub(crate) tools: BTreeMap<String, McpToolDescriptor>,
}

impl CacheState {
    /// Are the contents fresh enough to short-circuit describe_tools?
    ///
    /// `now_ms == 0` means the host clock is unavailable. We refuse to
    /// treat that as "fresh" — without a real wall clock the TTL is
    /// undefined and a degraded host would otherwise pin the cache
    /// forever. Likewise `updated_at_ms == 0` (cache was written under
    /// a degraded clock) bypasses the short-circuit.
    pub(crate) fn is_fresh(&self, now_ms: u64) -> bool {
        if now_ms == 0 || self.updated_at_ms == 0 || self.tools.is_empty() {
            return false;
        }
        now_ms.saturating_sub(self.updated_at_ms) < CACHE_TTL_MS
    }

    /// Descriptors as an ordered Vec.
    pub(crate) fn as_vec(&self) -> Vec<McpToolDescriptor> {
        self.tools.values().cloned().collect()
    }
}

/// Read the current cache snapshot. Missing / corrupt KV entries yield
/// a fresh default (the worst case is one extra discovery fan-out).
pub(crate) fn load() -> CacheState {
    match kv::get_bytes_opt(CACHE_KEY) {
        Ok(Some(bytes)) => serde_json::from_slice(&bytes).unwrap_or_default(),
        _ => CacheState::default(),
    }
}

/// Merge `incoming` into the cache via CAS so concurrent writers
/// (describe fan-out vs broadcast collector) don't clobber each other.
/// Last-write-wins per tool name. Returns the merged snapshot.
///
/// Enforces [`MAX_CACHED_TOOLS`] on the merged set so a hostile
/// broadcaster cannot grow the persistent cache unbounded — overflow
/// entries are dropped from the tail of the sorted `BTreeMap` (the
/// order is deterministic for byte-stable CAS).
pub(crate) fn upsert(incoming: Vec<McpToolDescriptor>) -> CacheState {
    if incoming.is_empty() {
        return load();
    }

    for _ in 0..CAS_MAX_RETRIES {
        let expected = kv::get_bytes_opt(CACHE_KEY).ok().flatten();
        let mut state: CacheState = expected
            .as_ref()
            .and_then(|b| serde_json::from_slice(b).ok())
            .unwrap_or_default();

        for desc in &incoming {
            // Upserting an existing name never grows the cache; only a
            // genuinely new name does. Cap insertion when at the limit.
            if state.tools.len() >= MAX_CACHED_TOOLS && !state.tools.contains_key(&desc.name) {
                continue;
            }
            state.tools.insert(desc.name.clone(), desc.clone());
        }
        state.updated_at_ms = now_ms();

        let Ok(new_bytes) = serde_json::to_vec(&state) else {
            log::warn(format!(
                "{}: failed to serialize tool cache",
                crate::profile::log_tag()
            ));
            return state;
        };

        match kv::cas(CACHE_KEY, expected.as_deref(), &new_bytes) {
            Ok(true) => return state,
            Ok(false) => continue, // Lost race — reload and retry.
            Err(e) => {
                log::warn(format!(
                    "{}: tool cache CAS failed: {e}",
                    crate::profile::log_tag()
                ));
                return state;
            }
        }
    }

    log::warn(format!(
        "{}: tool cache CAS exhausted retries",
        crate::profile::log_tag()
    ));
    load()
}

/// Replace the cache wholesale (used after a full describe fan-out so
/// stale entries from departed capsules don't linger forever). Still
/// goes through CAS so an in-flight broadcast update isn't dropped on
/// the floor; on the rare CAS miss we fall back to a per-tool merge.
///
/// Same [`MAX_CACHED_TOOLS`] cap as [`upsert`]: anything beyond is
/// dropped before write. Discovery already deduplicates by name, so
/// the only path to overflow is a single fan-out returning more
/// descriptors than the cap.
pub(crate) fn replace(snapshot: Vec<McpToolDescriptor>) -> CacheState {
    let mut state = CacheState {
        updated_at_ms: now_ms(),
        tools: BTreeMap::new(),
    };
    for desc in snapshot.iter() {
        if state.tools.len() >= MAX_CACHED_TOOLS {
            break;
        }
        state.tools.insert(desc.name.clone(), desc.clone());
    }

    let expected = kv::get_bytes_opt(CACHE_KEY).ok().flatten();
    let Ok(new_bytes) = serde_json::to_vec(&state) else {
        log::warn(format!(
            "{}: failed to serialize tool cache (replace)",
            crate::profile::log_tag()
        ));
        return state;
    };
    match kv::cas(CACHE_KEY, expected.as_deref(), &new_bytes) {
        Ok(true) => state,
        Ok(false) => {
            // Concurrent broadcast wrote between our read and CAS; merge
            // our snapshot into whatever's there instead of clobbering.
            upsert(snapshot)
        }
        Err(e) => {
            log::warn(format!(
                "{}: tool cache CAS (replace) failed: {e}",
                crate::profile::log_tag()
            ));
            state
        }
    }
}

/// Force the next `tools/list` to re-discover by emptying the cached
/// descriptor map.
///
/// Called when the loaded-capsule set changes — the kernel's
/// `astrid.v1.capsules_loaded` signal (install / live upgrade / live remove).
/// The event-driven merge path ([`upsert`], via `collect_tool_descriptors`)
/// only ever ADDS descriptors, so a removed or upgraded capsule's stale tools
/// would otherwise linger in the cache (callable, erroring) until the TTL
/// expires. An empty cache is never [`CacheState::is_fresh`] (the cold-start
/// guard), so the next [`super::discovery::collect_snapshot`] re-runs the
/// describe fan-out — a departed capsule no longer responds and its tools drop
/// out, while a freshly added one is picked up.
///
/// Best-effort: a failed write just leaves the prior cache to self-heal at the
/// TTL. This empties only the `tools/list` DESCRIPTOR cache — `tools/call`
/// routing never reads it — so an emptied cache cannot break a concurrent tool
/// call. The write is intentionally non-CAS: by the time the kernel publishes
/// `capsules_loaded` the registry change has already happened, so any fan-out
/// that runs after this invalidation observes the new set.
pub(crate) fn invalidate() {
    let Ok(bytes) = serde_json::to_vec(&CacheState::default()) else {
        log::warn(format!(
            "{}: failed to serialize empty tool cache (invalidate)",
            crate::profile::log_tag()
        ));
        return;
    };
    if let Err(e) = kv::set_bytes(CACHE_KEY, &bytes) {
        log::warn(format!(
            "{}: tool cache invalidate failed: {e}",
            crate::profile::log_tag()
        ));
    }
}

/// Wall-clock millis. The host clock is monotonic enough at the
/// 60-second TTL granularity we care about.
fn now_ms() -> u64 {
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

    use super::*;

    fn state(updated_at_ms: u64, n_tools: usize) -> CacheState {
        let mut tools = BTreeMap::new();
        for i in 0..n_tools {
            let name = format!("tool{i}");
            tools.insert(
                name.clone(),
                McpToolDescriptor {
                    name,
                    title: None,
                    description: String::new(),
                    input_schema: serde_json::Value::Null,
                    capabilities: None,
                },
            );
        }
        CacheState {
            updated_at_ms,
            tools,
        }
    }

    #[test]
    fn fresh_within_ttl() {
        install_test_profile();
        assert!(state(1_000, 3).is_fresh(1_000 + CACHE_TTL_MS - 1));
    }

    #[test]
    fn stale_past_ttl() {
        install_test_profile();
        assert!(!state(1_000, 3).is_fresh(1_000 + CACHE_TTL_MS + 1));
    }

    /// The `tools/list` reliability guard: an empty cache (cold, or left
    /// empty by a transient empty fan-out) must NEVER short-circuit, so the
    /// server always re-discovers rather than pinning an empty `tools/list`
    /// for the TTL.
    #[test]
    fn empty_cache_is_never_fresh() {
        install_test_profile();
        assert!(!state(1_000, 0).is_fresh(1_000 + 1));
    }

    /// A degraded host clock (`now == 0`) or a snapshot written under one
    /// (`updated_at == 0`) leaves the TTL undefined — never treat either as
    /// fresh, or a degraded host pins the cache forever.
    #[test]
    fn degraded_clock_is_never_fresh() {
        install_test_profile();
        assert!(!state(1_000, 3).is_fresh(0));
        assert!(!state(0, 3).is_fresh(1_000));
    }
}
