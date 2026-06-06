#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]

//! Context Engine capsule — pluggable compaction with interceptor hook support.
//!
//! Manages context window compaction and fires `context_engine.v1.hook.before_compaction` /
//! `context_engine.v1.hook.after_compaction` interceptors to plugin capsules via IPC. The default
//! strategy is simple token-budget trimming. A different capsule claiming
//! the same IPC topics can replace this one entirely.
//!
//! # IPC Protocol
//!
//! **Requests** (publish to these topics):
//! - `context_engine.v1.compact` — compact a session's messages
//! - `context_engine.v1.estimate_tokens` — estimate token count for messages
//!
//! **Responses** (published by context engine):
//! - `context_engine.v1.response.compact` — compacted messages and stats
//! - `context_engine.v1.response.estimate_tokens` — estimated token count
//!
//! # Interceptor Hooks (fired via IPC)
//!
//! - `context_engine.v1.hook.before_compaction` — request-response: plugins can mark messages as
//!   protected or skip compaction entirely. Plugins respond on a per-request
//!   response topic included in the hook payload.
//! - `context_engine.v1.hook.after_compaction` — fire-and-forget notification with stats.

mod strategy;

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

use astrid_sdk::prelude::*;
use serde::{Deserialize, Serialize};

// ── IPC payload types ───────────────────────────────────────────────

/// IPC request payload for `context_engine.v1.compact`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompactRequest {
    /// Session being compacted.
    session_id: String,
    /// Current conversation messages.
    messages: Vec<serde_json::Value>,
    /// Hard limit on context window size (tokens).
    max_tokens: u64,
    /// Target token count after compaction.
    target_tokens: u64,
}

/// IPC response payload for `context_engine.v1.response.compact`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompactResponse {
    /// Messages after compaction.
    messages: Vec<serde_json::Value>,
    /// Whether compaction actually occurred.
    compacted: bool,
    /// Number of messages removed.
    messages_removed: u32,
    /// Strategy that was used.
    strategy: String,
}

/// IPC request payload for `context_engine.v1.estimate_tokens`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EstimateRequest {
    /// Messages to estimate token count for.
    messages: Vec<serde_json::Value>,
}

/// IPC response payload for `context_engine.v1.response.estimate_tokens`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EstimateResponse {
    /// Estimated total token count.
    estimated_tokens: u64,
}

/// Payload sent to plugin capsules via the `context_engine.v1.hook.before_compaction` IPC hook.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BeforeCompactionPayload {
    /// Session being compacted.
    session_id: String,
    /// Current messages.
    messages: Vec<serde_json::Value>,
    /// Number of messages.
    message_count: u32,
    /// Estimated current token usage.
    estimated_tokens: u64,
    /// Maximum allowed tokens.
    max_tokens: u64,
    /// Topic where plugins should publish their hook responses.
    response_topic: String,
}

/// A single plugin's response to the `context_engine.v1.hook.before_compaction` hook.
///
/// All fields are optional. The context engine merges responses from
/// multiple plugins: any `skip: true` wins, `protected_message_ids`
/// are unioned.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BeforeCompactionHookResponse {
    /// If `true`, skip compaction entirely (plugin takes responsibility).
    #[serde(default)]
    skip: Option<bool>,
    /// Message IDs that must not be removed or summarized.
    #[serde(default, alias = "protected_message_ids")]
    pinned_message_ids: Vec<String>,
    /// Reserved for future use: plugin-provided compaction strategy name.
    #[serde(default)]
    custom_strategy: Option<String>,
}

impl BeforeCompactionHookResponse {
    /// Returns `true` if at least one field is set.
    fn has_any_field(&self) -> bool {
        self.skip.is_some() || !self.pinned_message_ids.is_empty() || self.custom_strategy.is_some()
    }
}

/// Fire-and-forget notification payload for `context_engine.v1.hook.after_compaction`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AfterCompactionPayload {
    /// Session that was compacted.
    session_id: String,
    /// Message count before compaction.
    messages_before: u32,
    /// Message count after compaction.
    messages_after: u32,
    /// Token estimate before compaction.
    tokens_before: u64,
    /// Token estimate after compaction.
    tokens_after: u64,
    /// Name of the strategy used.
    strategy_used: String,
}

// ── Configuration ───────────────────────────────────────────────────

/// Runtime configuration loaded from capsule config at startup.
struct Config {
    /// Maximum time (in milliseconds) to wait for plugin hook responses — the
    /// OUTER cap for the multi-responder accumulation case.
    hook_timeout_ms: u64,
    /// First-response window (ms): how long to wait for the FIRST hook response
    /// before proceeding with compaction. Defaults to [`HOOK_FIRST_RESPONSE_MS`];
    /// an operator with a genuinely slow compaction plugin can raise it (per
    /// principal) so its response isn't silently excluded.
    hook_first_response_ms: u64,
    /// Idle-grace window (ms) once at least one response has arrived. Defaults to
    /// [`HOOK_IDLE_GRACE_MS`].
    hook_idle_grace_ms: u64,
    /// Number of recent turns to always keep during compaction.
    keep_recent: usize,
}

impl Config {
    /// Load configuration from the capsule's config store, falling back to
    /// defaults.
    ///
    /// Read per invocation, **not** cached: `env::var` resolves the
    /// per-invocation, per-principal env overlay, so a global cache would pin
    /// one principal's tuning for every principal.
    fn load() -> Self {
        let keep_recent = env::var("keep_recent")
            .ok()
            .and_then(|s| s.trim().trim_matches('"').parse::<usize>().ok())
            .unwrap_or(DEFAULT_KEEP_RECENT);

        Self {
            hook_timeout_ms: env_u64("hook_timeout_ms", DEFAULT_HOOK_POLL_TIMEOUT_MS),
            hook_first_response_ms: env_u64("hook_first_response_ms", HOOK_FIRST_RESPONSE_MS),
            hook_idle_grace_ms: env_u64("hook_idle_grace_ms", HOOK_IDLE_GRACE_MS),
            keep_recent,
        }
    }
}

/// Read a `u64` capsule config value from `env`, falling back to `default` when
/// the key is missing or unparseable. Trims surrounding whitespace and quotes.
fn env_u64(key: &str, default: u64) -> u64 {
    env::var(key)
        .ok()
        .and_then(|s| s.trim().trim_matches('"').parse::<u64>().ok())
        .unwrap_or(default)
}

/// Pick the `recv` timeout (ms) for the next before-compaction hook poll, or
/// `None` when the fan-out loop should stop. Pure (no I/O) so the timing / exit
/// logic is unit-testable.
///
/// While waiting for the FIRST response the window is bounded to
/// `first_response_ms` measured from the START of the loop (via `elapsed_ms`),
/// **not** reset per poll — so a stream of non-matching polls can't extend it;
/// returns `None` once that window is spent with no response (the no-responder
/// backstop). Once a response has arrived a short `idle_grace_ms` applies. Every
/// window is also clamped to `remaining_ms` (the outer `hook_timeout_ms` cap),
/// and `remaining_ms == 0` stops the loop.
fn hook_recv_timeout(
    have_response: bool,
    elapsed_ms: u64,
    remaining_ms: u64,
    first_response_ms: u64,
    idle_grace_ms: u64,
) -> Option<u64> {
    if remaining_ms == 0 {
        return None;
    }
    if have_response {
        return Some(idle_grace_ms.min(remaining_ms));
    }
    let first_remaining = first_response_ms.saturating_sub(elapsed_ms);
    if first_remaining == 0 {
        None
    } else {
        Some(first_remaining.min(remaining_ms))
    }
}

// ── Constants ───────────────────────────────────────────────────────

/// Default maximum time to wait for plugin hook responses.
const DEFAULT_HOOK_POLL_TIMEOUT_MS: u64 = 2000;

/// Maximum number of hook responses to collect.
const MAX_HOOK_RESPONSES: usize = 50;

/// First-response window (ms) for the before-compaction fan-out. Hook
/// responders are local pooled capsules (~1ms round-trip), and the
/// common case is ZERO responders (no compaction plugin), which would
/// otherwise burn the full `hook_timeout_ms` (2s) on every compaction —
/// a per-prompt floor that, via react's synchronous compact wait, caps
/// orchestration throughput (astrid#816). Cap the first-response wait;
/// `hook_timeout_ms` stays the OUTER cap for multi-responder
/// accumulation.
const HOOK_FIRST_RESPONSE_MS: u64 = 250;

/// Idle-grace window (ms) once at least one before-compaction response
/// has arrived: poll only this long for stragglers, then proceed.
const HOOK_IDLE_GRACE_MS: u64 = 100;

/// Default number of recent turns to always keep during compaction.
const DEFAULT_KEEP_RECENT: usize = 10;

/// Monotonic counter for unique request IDs.
static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

// ── Main entry point ────────────────────────────────────────────────

#[derive(Default)]
struct ContextEngine;

#[capsule]
impl ContextEngine {
    #[astrid::run]
    fn run(&self) -> Result<(), SysError> {
        log::info("Context Engine capsule starting");

        let config = Config::load();
        log::info(format!(
            "Hook timeout: {}ms, keep_recent: {}",
            config.hook_timeout_ms, config.keep_recent
        ));

        // Subscribe to the two request topics this capsule serves. The
        // pre-refactor wildcard (`context_engine.v1.*`) is not legal
        // under the kernel ACL — the manifest's [subscribe] declares
        // specific exact-topic permissions, and a wildcard request
        // exceeds them. The dispatcher reads `result.messages[].topic`
        // and routes on it; receiving both topics on one Subscription
        // (which the wildcard enabled) was a convenience, not a
        // requirement.
        let compact_sub = ipc::subscribe("context_engine.v1.compact")
            .map_err(|e| SysError::ApiError(e.to_string()))?;
        let estimate_sub = ipc::subscribe("context_engine.v1.estimate_tokens")
            .map_err(|e| SysError::ApiError(e.to_string()))?;

        // Subscribe to our own hook topics so we can drain them.
        let hook_sub = ipc::subscribe("context_engine.v1.hook.before_compaction")
            .map_err(|e| SysError::ApiError(e.to_string()))?;
        let after_sub = ipc::subscribe("context_engine.v1.hook.after_compaction")
            .map_err(|e| SysError::ApiError(e.to_string()))?;

        // Signal readiness so the kernel can proceed with loading dependent capsules.
        // Best-effort: failure means the host mutex is poisoned (unrecoverable).
        let _ = runtime::signal_ready();

        log::info("Context Engine capsule ready");

        loop {
            // Block briefly on each request channel, then drain the
            // hook fan-out channels to prevent backpressure. The
            // pre-refactor single-wildcard subscription is fanned out
            // here into two short polls so cancellation latency stays
            // bounded by `HOOK_POLL_INTERVAL_MS * 2`.
            const HOOK_POLL_INTERVAL_MS: u64 = 1_000;
            // Timeout is normal — fall through. Real errors surface
            // when recv returns on a closed subscription, in which
            // case the loop exits when the host stops invoking run.
            if let Ok(result) = compact_sub.recv(HOOK_POLL_INTERVAL_MS) {
                dispatch_poll_result(&result, &config);
            }
            if let Ok(result) = estimate_sub.recv(HOOK_POLL_INTERVAL_MS) {
                dispatch_poll_result(&result, &config);
            }

            // Drain hook topics to prevent backpressure.
            let _ = hook_sub.poll();
            let _ = after_sub.poll();
        }
    }
}

// ── Envelope dispatch ───────────────────────────────────────────────

/// Returns `true` if the topic should be dispatched (not a self-echo).
fn should_dispatch_topic(topic: &str) -> bool {
    !topic.starts_with("context_engine.v1.response.")
        && !topic.starts_with("context_engine.v1.hook_response.")
        && topic != "context_engine.v1.hook.before_compaction"
        && topic != "context_engine.v1.hook.after_compaction"
}

/// Dispatch messages from a typed `PollResult`.
fn dispatch_poll_result(result: &ipc::PollResult, config: &Config) {
    if result.dropped > 0 {
        log::warn(format!(
            "Event bus dropped {} messages in context engine poll",
            result.dropped
        ));
    }

    for msg in &result.messages {
        if !should_dispatch_topic(&msg.topic) {
            continue;
        }

        let payload: serde_json::Value = match serde_json::from_str(&msg.payload) {
            Ok(v) => v,
            Err(e) => {
                log::warn(format!("failed to deserialize IPC message payload: {e}"));
                continue;
            }
        };

        // Extract from Custom payload envelope or direct.
        let request_value = payload.get("data").unwrap_or(&payload);

        match msg.topic.as_str() {
            "context_engine.v1.compact" => handle_compact(request_value, config),
            "context_engine.v1.estimate_tokens" => handle_estimate_tokens(request_value),
            _ => {}
        }
    }
}

// ── Compact handler ─────────────────────────────────────────────────

/// Handle a `context_engine.v1.compact` request.
///
/// 1. Parse the request
/// 2. Clamp `target_tokens` to not exceed `max_tokens`
/// 3. Fire `context_engine.v1.hook.before_compaction` hook via IPC
/// 4. Merge responses (any skip -> skip, union of pinned IDs)
/// 5. Run compaction strategy
/// 6. Fire `context_engine.v1.hook.after_compaction` notification
/// 7. Publish compacted result
fn handle_compact(payload: &serde_json::Value, config: &Config) {
    let request: CompactRequest = match serde_json::from_value(payload.clone()) {
        Ok(r) => r,
        Err(e) => {
            log::error(format!("Failed to parse compact request: {e}"));
            let _ = ipc::publish_json(
                "context_engine.v1.response.compact",
                &serde_json::json!({"error": format!("invalid request: {e}")}),
            );
            return;
        }
    };

    // Enforce: target_tokens must not exceed max_tokens.
    let target_tokens = request.target_tokens.min(request.max_tokens);

    let message_count = u32::try_from(request.messages.len()).unwrap_or(u32::MAX);
    let tokens_before = strategy::estimate_total_tokens(&request.messages);

    // Fire `before_compaction` hook via IPC.
    let merged = fire_before_compaction(&request, tokens_before, message_count, config);

    // If any plugin says skip, return messages unchanged.
    if merged.skip {
        log::info(format!(
            "Compaction skipped by plugin for session {}",
            request.session_id
        ));
        let response = CompactResponse {
            messages: request.messages,
            compacted: false,
            messages_removed: 0,
            strategy: "skipped".to_string(),
        };
        let _ = ipc::publish_json("context_engine.v1.response.compact", &response);
        return;
    }

    // Run default compaction strategy.
    let compacted_messages = strategy::summarize_and_truncate(
        &request.messages,
        target_tokens,
        &merged.protected_ids,
        config.keep_recent,
    );

    let messages_after = u32::try_from(compacted_messages.len()).unwrap_or(u32::MAX);
    let messages_removed = message_count.saturating_sub(messages_after);
    let tokens_after = strategy::estimate_total_tokens(&compacted_messages);
    let compacted = messages_removed > 0;
    let strategy_name = "summarize_and_truncate".to_string();

    // Fire `after_compaction` notification (fire-and-forget).
    fire_after_compaction(
        &request.session_id,
        message_count,
        messages_after,
        tokens_before,
        tokens_after,
        &strategy_name,
    );

    // Publish the compacted result.
    let response = CompactResponse {
        messages: compacted_messages,
        compacted,
        messages_removed,
        strategy: strategy_name,
    };
    let _ = ipc::publish_json("context_engine.v1.response.compact", &response);

    log::info(format!(
        "Compaction completed: session={}, removed={messages_removed}, \
         tokens {tokens_before} -> {tokens_after}",
        request.session_id
    ));
}

// ── Estimate handler ────────────────────────────────────────────────

/// Handle a `context_engine.v1.estimate_tokens` request.
fn handle_estimate_tokens(payload: &serde_json::Value) {
    let request: EstimateRequest = match serde_json::from_value(payload.clone()) {
        Ok(r) => r,
        Err(e) => {
            log::error(format!("Failed to parse estimate_tokens request: {e}"));
            let _ = ipc::publish_json(
                "context_engine.v1.response.estimate_tokens",
                &serde_json::json!({"error": format!("invalid request: {e}")}),
            );
            return;
        }
    };

    let estimated_tokens = strategy::estimate_total_tokens(&request.messages);
    let response = EstimateResponse { estimated_tokens };
    let _ = ipc::publish_json("context_engine.v1.response.estimate_tokens", &response);
}

// ── Interceptor hook firing via IPC ─────────────────────────────────

/// Merged result of all `before_compaction` hook responses.
struct MergedBeforeCompaction {
    /// If `true`, skip compaction entirely.
    skip: bool,
    /// Union of all pinned/protected message IDs.
    protected_ids: HashSet<String>,
}

/// Fire the `context_engine.v1.hook.before_compaction` hook via IPC and collect plugin responses.
///
/// Publishes the hook payload on the `context_engine.v1.hook.before_compaction` IPC topic with a
/// per-request response topic. Polls for plugin responses within the
/// configured timeout window.
fn fire_before_compaction(
    request: &CompactRequest,
    tokens_before: u64,
    message_count: u32,
    config: &Config,
) -> MergedBeforeCompaction {
    // `SystemTime::now()` panics on `wasm32-unknown-unknown`. Use the
    // monotonic host clock (which works) plus a per-capsule atomic
    // counter to produce a unique-enough request_id for the hook
    // fan-out reply-topic suffix.
    let request_id = format!(
        "compact-{}-{}",
        astrid_sdk::time::monotonic().as_nanos(),
        REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed)
    );

    let response_topic = format!("context_engine.v1.hook_response.{request_id}");

    // Subscribe BEFORE publishing to avoid missing fast responses.
    let sub = match ipc::subscribe(&response_topic) {
        Ok(h) => h,
        Err(e) => {
            log::error(format!("Failed to subscribe to hook response topic: {e}"));
            return MergedBeforeCompaction {
                skip: false,
                protected_ids: HashSet::new(),
            };
        }
    };

    let payload = BeforeCompactionPayload {
        session_id: request.session_id.clone(),
        messages: request.messages.clone(),
        message_count,
        estimated_tokens: tokens_before,
        max_tokens: request.max_tokens,
        response_topic: response_topic.clone(),
    };

    if let Err(e) = ipc::publish_json("context_engine.v1.hook.before_compaction", &payload) {
        log::error(format!(
            "Failed to publish context_engine.v1.hook.before_compaction event: {e}"
        ));
        // `sub` drops here, releasing the subscription.
        return MergedBeforeCompaction {
            skip: false,
            protected_ids: HashSet::new(),
        };
    }

    // Block-wait for hook responses within the configured timeout.
    // `std::time::Instant::now()` panics on `wasm32-unknown-unknown`;
    // use the monotonic host clock via `astrid_sdk::time` instead.
    let mut responses: Vec<BeforeCompactionHookResponse> = Vec::new();
    let start = astrid_sdk::time::monotonic();
    let timeout_dur = std::time::Duration::from_millis(config.hook_timeout_ms);

    while astrid_sdk::time::monotonic().saturating_sub(start) < timeout_dur
        && responses.len() < MAX_HOOK_RESPONSES
    {
        let elapsed = astrid_sdk::time::monotonic().saturating_sub(start);
        let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        let remaining_ms =
            u64::try_from(timeout_dur.saturating_sub(elapsed).as_millis()).unwrap_or(u64::MAX);
        // Short window for the FIRST response (no-responder backstop), bounded
        // from the loop START; idle-grace once we have one; `None` stops the
        // loop (window spent / outer cap reached). See `hook_recv_timeout`.
        let Some(timeout) = hook_recv_timeout(
            !responses.is_empty(),
            elapsed_ms,
            remaining_ms,
            config.hook_first_response_ms,
            config.hook_idle_grace_ms,
        ) else {
            break;
        };

        match sub.recv(timeout) {
            Ok(result) => {
                if result.messages.is_empty() {
                    break;
                }
                responses.extend(parse_hook_responses(&result));
            }
            _ => break,
        }
    }

    // Subscription drops at scope exit; no manual unsubscribe needed.

    if !responses.is_empty() {
        log::info(format!(
            "Collected {} context_engine.v1.hook.before_compaction responses",
            responses.len()
        ));
    }

    merge_before_compaction_responses(&responses)
}

/// Parse hook responses from a typed `PollResult`.
fn parse_hook_responses(result: &ipc::PollResult) -> Vec<BeforeCompactionHookResponse> {
    let mut responses = Vec::new();

    for msg in &result.messages {
        let payload: serde_json::Value = match serde_json::from_str(&msg.payload) {
            Ok(v) => v,
            Err(e) => {
                log::warn(format!(
                    "failed to deserialize compaction response payload: {e}"
                ));
                continue;
            }
        };

        // Try direct payload, then nested in Custom `data` envelope.
        let maybe_response =
            serde_json::from_value::<BeforeCompactionHookResponse>(payload.clone())
                .ok()
                .filter(BeforeCompactionHookResponse::has_any_field)
                .or_else(|| {
                    payload
                        .get("data")
                        .and_then(|data| {
                            serde_json::from_value::<BeforeCompactionHookResponse>(data.clone())
                                .ok()
                        })
                        .filter(BeforeCompactionHookResponse::has_any_field)
                });

        if let Some(response) = maybe_response {
            responses.push(response);
        }
    }

    responses
}

/// Merge `context_engine.v1.hook.before_compaction` responses from multiple plugins.
///
/// - `skip`: any `true` -> skip compaction
/// - `pinned_message_ids`: union of all responses
fn merge_before_compaction_responses(
    responses: &[BeforeCompactionHookResponse],
) -> MergedBeforeCompaction {
    let skip = responses.iter().any(|r| r.skip == Some(true));

    let protected_ids: HashSet<String> = responses
        .iter()
        .flat_map(|r| r.pinned_message_ids.iter().cloned())
        .collect();

    MergedBeforeCompaction {
        skip,
        protected_ids,
    }
}

/// Fire the `context_engine.v1.hook.after_compaction` notification (fire-and-forget).
fn fire_after_compaction(
    session_id: &str,
    messages_before: u32,
    messages_after: u32,
    tokens_before: u64,
    tokens_after: u64,
    strategy_used: &str,
) {
    let payload = AfterCompactionPayload {
        session_id: session_id.to_string(),
        messages_before,
        messages_after,
        tokens_before,
        tokens_after,
        strategy_used: strategy_used.to_string(),
    };
    let _ = ipc::publish_json("context_engine.v1.hook.after_compaction", &payload);
}

#[cfg(test)]
mod tests;
