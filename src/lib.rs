#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]
#![warn(missing_docs)]

//! ReAct loop capsule for Astrid OS.
//!
//! Stateless coordinator that drives the reasoning-and-action loop:
//! fetch history from session, run it through identity + prompt builder,
//! send to LLM, collect response, dispatch tools, loop. Sends clean
//! results back to the session capsule at turn boundaries.
//!
//! # State Machine
//!
//! ```text
//! Idle -> AwaitingIdentity -> AwaitingPromptBuild -> Streaming -> AwaitingTools -> Streaming -> ... -> Idle
//! ```
//!
//! The react loop contains no inference logic. It defines the control
//! flow that coordinates Session, Identity, Prompt Builder, Provider,
//! and Tool Router capsules over the event bus.

use astrid_sdk::prelude::*;
use astrid_sdk::types::{
    IpcPayload, LlmToolDefinition, Message, MessageContent, MessageRole, StreamEvent, ToolCall,
    ToolCallResult,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// KV key prefix for the persisted turn state.
///
/// Keyed as `react.turn.{session_id}`. This is ephemeral per-turn
/// control flow state, not conversation history (that lives in the
/// session capsule).
const TURN_KEY_PREFIX: &str = "react.turn";

/// Default session ID used when the IPC payload does not specify one.
const DEFAULT_SESSION_ID: &str = "default";

/// KV key prefix for request_id -> session_id correlation.
const REQUEST_SESSION_PREFIX: &str = "react.req2sess";

/// KV key prefix for call_id -> session_id correlation.
const CALL_SESSION_PREFIX: &str = "react.call2sess";

/// KV key for the set of active (non-Idle) session IDs.
///
/// Stored as a JSON array of strings. The watchdog iterates this set
/// to check timeouts across all sessions, not just the default one.
///
/// Thread safety: the read-modify-write on this key is safe because all
/// WASM interceptor calls within a single capsule are serialized by the
/// plugin mutex in `WasmEngine::invoke_interceptor`. Concurrent dispatches
/// to the same capsule block on the mutex, so no lost-update race is possible.
const ACTIVE_SESSIONS_KEY: &str = "react.active_sessions";

/// Default timeout in milliseconds for session capsule requests.
const DEFAULT_SESSION_TIMEOUT_MS: u64 = 2_000;

/// Default timeout in milliseconds for context engine compact requests.
const DEFAULT_COMPACT_TIMEOUT_MS: u64 = 5_000;

/// Max times an orchestration response is bounced back through the bus to wait
/// out a `TurnState` read-after-write visibility race (astrid#816): a previous
/// handler's `state.save()` may not yet be visible to this handler's
/// `TurnState::load()` when they run on different pooled Store instances close
/// together. Each re-drive is one bus round-trip, so this spans roughly that
/// many round-trips before giving up.
const MAX_REDRIVE_RETRIES: u64 = 20;

/// KV key for cached provider context window size (tokens).
const KV_CONTEXT_WINDOW: &str = "react.context_window";

/// KV key for cached provider max output tokens.
const KV_MAX_OUTPUT_TOKENS: &str = "react.max_output_tokens";

/// IPC topic for context engine compact requests.
const COMPACT_REQUEST_TOPIC: &str = "context_engine.v1.compact";

/// IPC topic for context engine compact responses.
const COMPACT_RESPONSE_TOPIC: &str = "context_engine.v1.response.compact";

/// Fraction of context budget used as the compaction target (90%).
/// Leaves headroom for system prompt and tool schemas.
const COMPACTION_TARGET_NUM: u64 = 9;
const COMPACTION_TARGET_DENOM: u64 = 10;

/// Current wall-clock time as milliseconds since UNIX epoch, or 0 if unavailable.
fn now_ms() -> u64 {
    time::now()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64)
}

/// Build the KV key for a session's turn state.
fn turn_key(session_id: &str) -> String {
    format!("{TURN_KEY_PREFIX}.{session_id}")
}

/// Store a request_id -> session_id mapping so LLM stream handlers
/// can resolve the owning session from the stream's request_id.
fn store_request_session(request_id: &Uuid, session_id: &str) -> Result<(), SysError> {
    let key = format!("{REQUEST_SESSION_PREFIX}.{request_id}");
    kv::set_bytes(&key, session_id.as_bytes())
}

/// Look up session_id from a request_id.
fn lookup_session_by_request(request_id: &Uuid) -> Option<String> {
    let key = format!("{REQUEST_SESSION_PREFIX}.{request_id}");
    kv::get_bytes(&key)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
}

/// Store call_id -> session_id mappings so tool result handlers
/// can resolve the owning session from the tool's call_id.
fn store_call_sessions(call_ids: &[String], session_id: &str) -> Result<(), SysError> {
    for call_id in call_ids {
        let key = format!("{CALL_SESSION_PREFIX}.{call_id}");
        kv::set_bytes(&key, session_id.as_bytes())?;
    }
    Ok(())
}

/// Look up session_id from a tool call_id.
fn lookup_session_by_call(call_id: &str) -> Option<String> {
    let key = format!("{CALL_SESSION_PREFIX}.{call_id}");
    kv::get_bytes(&key)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
}

/// Clean up a request_id -> session_id mapping after the LLM stream completes.
fn delete_request_session(request_id: &Uuid) {
    let key = format!("{REQUEST_SESSION_PREFIX}.{request_id}");
    if let Err(e) = kv::delete(&key) {
        log::warn(format!("Failed to delete req2sess key '{key}': {e}"));
    }
}

/// Clean up call_id -> session_id mappings after all tool results are collected.
fn delete_call_sessions(call_ids: &[String]) {
    for call_id in call_ids {
        let key = format!("{CALL_SESSION_PREFIX}.{call_id}");
        if let Err(e) = kv::delete(&key) {
            log::warn(format!("Failed to delete call2sess key '{key}': {e}"));
        }
    }
}

/// Load the set of active session IDs from KV.
fn load_active_sessions() -> Vec<String> {
    // Missing key = no sessions registered yet (the cold-start norm), not an
    // error — `get_json_opt` returns `None` rather than the old `get_json`'s
    // spurious "EOF while parsing" on an absent key. Only a genuine parse
    // failure warrants a warning.
    match kv::get_json_opt::<Vec<String>>(ACTIVE_SESSIONS_KEY) {
        Ok(Some(v)) => v,
        Ok(None) => Vec::new(),
        Err(e) => {
            log::warn(format!(
                "Corrupt active-sessions list in KV, defaulting to empty: {e}"
            ));
            Vec::new()
        }
    }
}

/// Add a session ID to the active sessions set.
fn register_active_session(session_id: &str) {
    let mut sessions = load_active_sessions();
    if !sessions.iter().any(|s| s == session_id) {
        sessions.push(session_id.to_string());
        if let Err(e) = kv::set_json(ACTIVE_SESSIONS_KEY, &sessions) {
            log::error(format!(
                "Failed to register active session '{session_id}': {e}"
            ));
        }
    }
}

/// Clear all ephemeral keys left over from a previous incarnation.
///
/// Called on capsule restart to prevent stale turn state, correlation
/// mappings, and active-session lists from persisting across restarts.
fn clear_ephemeral_keys() {
    for prefix in ["react.turn.", "react.req2sess.", "react.call2sess."] {
        match kv::clear_prefix(prefix) {
            Ok(n) if n > 0 => {
                log::info(format!("Cleared {n} ephemeral keys with prefix '{prefix}'"));
            }
            Err(e) => {
                log::warn(format!("Failed to clear ephemeral keys '{prefix}': {e}"));
            }
            _ => {}
        }
    }
    if let Err(e) = kv::delete(ACTIVE_SESSIONS_KEY) {
        log::warn(format!("Failed to clear active sessions key: {e}"));
    }
}

/// Remove a session ID from the active sessions set.
fn unregister_active_session(session_id: &str) {
    let mut sessions = load_active_sessions();
    if let Some(pos) = sessions.iter().position(|s| s == session_id) {
        sessions.swap_remove(pos);
        if let Err(e) = kv::set_json(ACTIVE_SESSIONS_KEY, &sessions) {
            log::error(format!(
                "Failed to unregister active session '{session_id}': {e}"
            ));
        }
    }
}

/// Read a `u64` config value from capsule config, with warning on parse failure.
fn get_config_u64(key: &str, default: u64) -> u64 {
    match env::var(key) {
        Ok(s) if !s.trim().is_empty() => match s.trim().trim_matches('"').parse::<u64>() {
            Ok(v) => v,
            Err(e) => {
                log::warn(format!(
                    "Invalid config for '{key}': \"{s}\", using default {default}. Error: {e}"
                ));
                default
            }
        },
        _ => default,
    }
}

/// Resolve the session timeout from capsule config, falling back to default.
fn session_timeout_ms() -> u64 {
    get_config_u64("session_timeout_ms", DEFAULT_SESSION_TIMEOUT_MS)
}

/// State machine phase for the react loop.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum Phase {
    /// No active turn. Waiting for user input.
    Idle,
    /// Waiting for the identity capsule to return the system prompt.
    AwaitingIdentity,
    /// Waiting for the prompt builder capsule to assemble the final prompt.
    AwaitingPromptBuild,
    /// Streaming tokens/tool calls from the LLM provider.
    Streaming,
    /// Waiting for all pending tool executions to complete.
    AwaitingTools,
}

/// A tool call being accumulated from stream deltas.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PendingToolCall {
    /// Tool call ID from the LLM.
    id: String,
    /// Tool name.
    name: String,
    /// Accumulated JSON argument string (appended from deltas).
    args_json: String,
    /// Whether this tool call's stream has ended (ContentBlockStop received).
    complete: bool,
}

/// A tool call that has been dispatched and is awaiting a result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DispatchedToolCall {
    /// Tool call ID.
    id: String,
    /// Tool name.
    name: String,
    /// Parsed arguments.
    arguments: serde_json::Value,
    /// Result, filled in when `tool.execute.result` arrives.
    result: Option<ToolCallResult>,
    /// The `request_id` of the turn that dispatched this tool.
    /// Used to detect stale tool results arriving after a turn reset.
    #[serde(default)]
    turn_request_id: Uuid,
}

/// Default maximum number of ReAct loop iterations before forced stop.
const DEFAULT_MAX_ITERATIONS: u32 = 25;

/// Default timeout in seconds for identity/prompt builder phases.
const DEFAULT_IDENTITY_TIMEOUT_SECS: u64 = 30;

/// Default timeout in seconds for the LLM streaming phase.
const DEFAULT_STREAMING_TIMEOUT_SECS: u64 = 120;

/// Default timeout in seconds for the tool execution phase.
const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 60;

/// Ephemeral per-turn state for the react loop.
///
/// This is control flow state, not conversation history. History
/// lives in the session capsule and is fetched on demand.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TurnState {
    /// Schema version for forward-compatible deserialization.
    /// TurnState is ephemeral so no migration logic is needed;
    /// unrecognized versions simply fall back to `Default`.
    #[serde(default)]
    schema_version: u32,
    /// Session ID for this conversation.
    session_id: String,
    /// Current state machine phase.
    phase: Phase,
    /// System prompt from the identity capsule (ephemeral, for this turn only).
    system_prompt: String,
    /// Request ID for the current LLM generation.
    request_id: Uuid,
    /// Accumulated response text from the current LLM stream.
    response_text: String,
    /// Tool calls being accumulated from stream deltas.
    pending_stream_tools: Vec<PendingToolCall>,
    /// Tool calls that have been dispatched for execution.
    dispatched_tools: Vec<DispatchedToolCall>,
    /// Tool schemas for the current turn, provided by the prompt builder.
    /// Persisted across tool iterations so the same tool set is used for
    /// continuation requests after tool results arrive.
    #[serde(default)]
    current_tools: Vec<LlmToolDefinition>,
    /// Number of Streaming -> AwaitingTools -> Streaming iterations this turn.
    #[serde(default)]
    iteration_count: u32,
    /// Millisecond timestamp when the current phase was entered.
    #[serde(default)]
    phase_entered_at_ms: u64,
}

impl Default for TurnState {
    fn default() -> Self {
        Self {
            schema_version: 1,
            session_id: DEFAULT_SESSION_ID.into(),
            phase: Phase::Idle,
            system_prompt: String::new(),
            request_id: Uuid::nil(),
            response_text: String::new(),
            pending_stream_tools: Vec::new(),
            dispatched_tools: Vec::new(),
            current_tools: Vec::new(),
            iteration_count: 0,
            phase_entered_at_ms: 0,
        }
    }
}

impl TurnState {
    /// Load turn state from KV, or create default if not present.
    ///
    /// TurnState is ephemeral - no migration logic needed. If the schema
    /// version is unrecognized (e.g. binary downgrade), reset to default
    /// rather than risking misinterpreted fields.
    fn load(session_id: &str) -> Self {
        let key = turn_key(session_id);
        // Distinguish a *missing* key (a brand-new session — the normal cold
        // path, not an error) from *corrupt* stored bytes. The old
        // `get_json` collapsed both into "EOF while parsing" via
        // `get_bytes`'s `unwrap_or_default()`, so every cold load logged a
        // spurious ERROR and reset to default. With genuine concurrency
        // (the Store pool, #816) those cold loads are frequent, so the
        // distinction matters: only a real parse failure is worth a warning.
        let mut state = match kv::get_json_opt::<Self>(&key) {
            Ok(Some(s)) => s,
            Ok(None) => Self::default(),
            Err(e) => {
                log::warn(format!(
                    "Corrupt turn state for session '{session_id}', resetting: {e}"
                ));
                Self::default()
            }
        };

        if !matches!(state.schema_version, 0 | 1) {
            log::warn(format!(
                "TurnState has unknown schema version {}, resetting to default",
                state.schema_version
            ));
            state = Self::default();
        }

        // Ensure session_id matches what was requested (handles default case)
        state.session_id = session_id.into();
        state
    }

    /// Persist turn state to KV, keyed by the actual session ID.
    fn save(&self) -> Result<(), SysError> {
        let key = turn_key(&self.session_id);
        kv::set_json(&key, self)
    }

    /// Reset per-iteration accumulators for a new LLM generation round.
    ///
    /// Note: `iteration_count` is NOT reset here because this is called
    /// between ReAct loop iterations. Use `reset_conversation_turn()` to
    /// fully reset for a new user prompt.
    fn reset_turn(&mut self) {
        self.response_text.clear();
        self.pending_stream_tools.clear();
        self.dispatched_tools.clear();
        self.request_id = Uuid::new_v4();
    }

    /// Fully reset turn state for a new conversation turn (new user prompt).
    fn reset_conversation_turn(&mut self) {
        self.reset_turn();
        self.current_tools.clear();
        self.iteration_count = 0;
    }

    /// Set the phase and record the wall-clock timestamp.
    ///
    /// When transitioning to Idle, automatically unregisters the session
    /// from the active sessions set so the watchdog stops checking it.
    fn set_phase(&mut self, phase: Phase) {
        self.phase = phase;
        self.phase_entered_at_ms = now_ms();
        if self.phase == Phase::Idle {
            unregister_active_session(&self.session_id);
        }
    }

    /// Check if the current phase has exceeded its timeout.
    ///
    /// Returns `true` if a timeout was detected and the state was reset to Idle
    /// (with an error response published to the frontend). Returns `false` if
    /// the phase is within limits or the clock is unavailable.
    fn check_phase_timeout(&mut self) -> Result<bool, SysError> {
        if self.phase == Phase::Idle {
            return Ok(false);
        }
        let now = now_ms();
        if now == 0 || self.phase_entered_at_ms == 0 {
            log::warn(
                "clock_ms unavailable or phase timestamp missing - phase timeouts disabled for this check",
            );
            return Ok(false);
        }
        let elapsed_secs = now.saturating_sub(self.phase_entered_at_ms) / 1000;

        let (default_secs, config_key) = match self.phase {
            Phase::AwaitingIdentity | Phase::AwaitingPromptBuild => {
                (DEFAULT_IDENTITY_TIMEOUT_SECS, "identity_timeout_secs")
            }
            Phase::Streaming => (DEFAULT_STREAMING_TIMEOUT_SECS, "streaming_timeout_secs"),
            Phase::AwaitingTools => (DEFAULT_TOOL_TIMEOUT_SECS, "tool_timeout_secs"),
            Phase::Idle => return Ok(false),
        };
        let timeout = get_config_u64(config_key, default_secs);

        if elapsed_secs >= timeout {
            let phase_name = format!("{:?}", self.phase);
            log::error(format!(
                "Phase {phase_name} timed out after {elapsed_secs}s"
            ));
            let _ = ipc::publish_json(
                "agent.v1.response",
                &IpcPayload::AgentResponse {
                    text: format!(
                        "Request timed out ({phase_name} phase exceeded {timeout}s limit)"
                    ),
                    is_final: true,
                    session_id: self.session_id.clone(),
                },
            );
            self.reset_conversation_turn();
            self.set_phase(Phase::Idle);
            self.save()?;
            return Ok(true);
        }
        Ok(false)
    }
}

/// ReAct loop capsule.
#[derive(Default)]
pub struct ReactLoop;

#[capsule]
impl ReactLoop {
    /// Handles lifecycle restart events from the kernel.
    ///
    /// Called after the capsule is reloaded during a restart. Clears
    /// all ephemeral KV keys (turn state, correlation mappings, active
    /// sessions) to prevent stale state from a previous incarnation.
    ///
    /// Intentionally parameterless: the kernel dispatches this with an
    /// empty payload (`&[]`). A typed parameter would cause the macro
    /// to attempt `serde_json::from_slice(&[])`, which always fails.
    #[astrid::interceptor("handle_lifecycle_restart")]
    pub fn handle_lifecycle_restart(&self) -> Result<(), SysError> {
        log::info("Lifecycle restart: clearing ephemeral keys");
        clear_ephemeral_keys();
        Ok(())
    }

    /// Handles session clear requests from frontends.
    ///
    /// Forwards the clear request to the session capsule (the authority
    /// on session lifecycle), which creates a new session with a
    /// `parent_session_id` pointer to the old one. React then updates
    /// its turn state to use the new session ID.
    #[astrid::interceptor("handle_session_clear")]
    pub fn handle_session_clear(&self, payload: serde_json::Value) -> Result<(), SysError> {
        let old_session_id = payload
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_SESSION_ID);

        let timeout = session_timeout_ms();

        // Use the SDK's request_response helper: it injects a correlation
        // id, subscribes to the scoped reply topic before publishing, and
        // drops the subscription on every return path (Drop on
        // `Subscription`). Session publishes `{correlation_id, new_session_id,
        // old_session_id}` at the root of the reply payload (see
        // `astrid-capsule-session::handle_clear`) — no envelope wrapper.
        let response: serde_json::Value = ipc::request_response(
            "session.v1.request.clear",
            "session.v1.response.clear",
            &serde_json::json!({
                "session_id": old_session_id,
            }),
            timeout,
        )?;

        let new_session_id = response
            .get("new_session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                SysError::ApiError("Session clear response missing new_session_id".into())
            })?
            .to_string();

        // Delete the old session's turn state - the session is done.
        let old_key = turn_key(old_session_id);
        if let Err(e) = kv::delete(&old_key) {
            log::warn(format!(
                "Failed to delete old turn state key '{old_key}': {e}"
            ));
        }
        unregister_active_session(old_session_id);

        // Initialize a fresh turn state for the new session.
        let new_state = TurnState {
            session_id: new_session_id.clone(),
            ..TurnState::default()
        };
        let key = turn_key(&new_session_id);
        kv::set_json(&key, &new_state)?;

        log::info(format!(
            "Session cleared: '{old_session_id}' -> '{new_session_id}'"
        ));

        // Notify frontends of the session change.
        ipc::publish_json(
            "agent.v1.session_changed",
            &serde_json::json!({
                "old_session_id": old_session_id,
                "new_session_id": new_session_id,
            }),
        )?;

        Ok(())
    }

    /// Handles periodic watchdog tick events from the kernel.
    ///
    /// Iterates all active sessions and checks if any phase has exceeded
    /// its timeout. This is the primary timeout enforcement mechanism.
    #[astrid::interceptor("handle_watchdog_tick")]
    pub fn handle_watchdog_tick(&self) -> Result<(), SysError> {
        for session_id in load_active_sessions() {
            let mut state = TurnState::load(&session_id);
            if let Err(e) = Self::check_timeout_with_cleanup(&mut state) {
                log::error(format!(
                    "Watchdog timeout check failed for session '{session_id}': {e}"
                ));
            }
        }
        Ok(())
    }

    /// Handles `system.event_bus.lagged` events from the dispatcher.
    ///
    /// Logs a warning when the bus overflows while any session is actively
    /// waiting for tool results - lost messages could cause the turn to hang.
    /// Actual recovery is handled by the watchdog timeout (B1/B2).
    ///
    /// Note: this notification itself may be lost if the bus is severely
    /// overloaded. The watchdog is the actual recovery mechanism.
    #[astrid::interceptor("handle_bus_lag")]
    pub fn handle_bus_lag(&self) -> Result<(), SysError> {
        for session_id in load_active_sessions() {
            let state = TurnState::load(&session_id);
            if state.phase == Phase::AwaitingTools {
                log::warn(format!(
                    "Event bus lagged while session '{session_id}' awaits tool results - watchdog will recover if results were lost"
                ));
            }
        }
        Ok(())
    }

    /// Handles `user.v1.prompt` events from platforms (CLI, Telegram, etc.).
    ///
    /// Appends the user message to the session capsule, fetches history,
    /// then requests the system prompt from the identity capsule.
    #[astrid::interceptor("handle_user_prompt")]
    pub fn handle_user_prompt(&self, payload: IpcPayload) -> Result<(), SysError> {
        let (text, session_id, context) = match payload {
            IpcPayload::UserInput {
                text,
                session_id,
                context,
            } => (text, session_id, context),
            _ => return Ok(()),
        };

        // Check for cancel signal before the empty-text guard, since
        // cancel is sent as empty text with context.action = "cancel_turn".
        if let Some(ref ctx) = context
            && ctx.get("action").and_then(|v| v.as_str()) == Some("cancel_turn")
        {
            return Self::handle_cancel(&session_id);
        }

        if text.trim().is_empty() {
            return Ok(());
        }

        // Warn when using the default session ID - may indicate an
        // unpatched frontend that doesn't send session_id yet.
        if session_id == DEFAULT_SESSION_ID {
            log::warn(
                "UserInput using default session_id - frontend may not be sending session_id",
            );
        }

        // Load or create TurnState keyed by the actual session ID.
        let mut state = TurnState::load(&session_id);

        // Append the user message to session atomically. The returned
        // history is not cached - downstream handlers fetch fresh.
        Self::fetch_messages_with_append(
            &state.session_id,
            &[Message {
                role: MessageRole::User,
                content: MessageContent::Text(text),
            }],
        )?;

        // Clean up any in-flight mappings from a previous interrupted turn
        // before resetting, otherwise stale req2sess/call2sess entries leak.
        if state.phase != Phase::Idle {
            Self::cleanup_inflight_mappings(&state);
        }
        state.reset_conversation_turn();
        state.set_phase(Phase::AwaitingIdentity);
        register_active_session(&state.session_id);
        state.save()?;

        // Request system prompt from the identity capsule.
        // session_id is threaded through so the response echoes it back.
        ipc::publish_json(
            "spark.v1.request.build",
            &serde_json::json!({
                "workspace_root": env::var("workspace_root").unwrap_or_default(),
                "session_id": state.session_id,
            }),
        )?;

        Ok(())
    }

    /// Guard an orchestration-response handler against the `TurnState`
    /// read-after-write visibility race (astrid#816).
    ///
    /// A response event (`spark.v1.response.ready`,
    /// `prompt_builder.v1.response.assemble`, …) only fires *mid-turn* — react
    /// sent the matching request while the turn was in `expected`. So if the
    /// `TurnState` load comes back `Idle` (the missing/default state), that is
    /// the race — the previous handler's `save()` on another pooled Store
    /// isn't visible yet — not a finished turn. Bounce the response back
    /// through the bus (the round-trip is the backoff) so it retries once the
    /// write lands; bounded by `MAX_REDRIVE_RETRIES` so a genuinely orphaned
    /// event can't loop forever.
    ///
    /// Returns `true` if the caller should return early — either a re-drive
    /// was issued, the retry budget is spent, or the turn is in some *other*
    /// (already-advanced / cancelled) phase that must not be retried. Returns
    /// `false` only when the turn is in `expected` and the caller may proceed.
    fn redrive_if_unready(
        expected: Phase,
        actual: Phase,
        topic: &str,
        payload: &serde_json::Value,
    ) -> bool {
        if actual == expected {
            return false;
        }
        // Only `Idle` is the visibility-race signature (a fresh
        // `TurnState::default()`). Any other phase is a genuine
        // already-advanced or cancelled turn — return without retrying.
        if actual == Phase::Idle {
            let retry = payload.get("_retry").and_then(|v| v.as_u64()).unwrap_or(0);
            if retry < MAX_REDRIVE_RETRIES {
                let mut p = payload.clone();
                // Guard the mutation: `p["_retry"] = …` panics if the payload
                // isn't a JSON object. Re-publish under this invocation's
                // (preserved) principal so the retry lands in the same KV
                // scope, and surface a failed re-drive (e.g. `CapabilityDenied`)
                // rather than silently dropping the event.
                if let Some(obj) = p.as_object_mut() {
                    obj.insert("_retry".to_string(), serde_json::json!(retry + 1));
                    if let Err(e) = ipc::publish_json(topic, &p) {
                        log::warn(format!("react: failed to re-drive '{topic}': {e:?}"));
                    }
                } else {
                    log::warn(format!(
                        "react: cannot re-drive '{topic}' — payload is not a JSON object"
                    ));
                }
            } else {
                log::warn(format!(
                    "react: gave up re-driving '{topic}' after {retry} retries \
                     (turn state never became visible)"
                ));
            }
        }
        true
    }

    /// Handles `spark.v1.response.ready` events from the identity capsule.
    ///
    /// Receives the assembled system prompt and sends it to the prompt
    /// builder capsule for capsule hook interception before LLM generation.
    #[astrid::interceptor("handle_identity_response")]
    pub fn handle_identity_response(&self, payload: serde_json::Value) -> Result<(), SysError> {
        // Resolve session from the echoed session_id in the identity response.
        let session_id = payload
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_SESSION_ID);

        let mut state = TurnState::load(session_id);

        // Opportunistic timeout check on every interceptor invocation
        if Self::check_timeout_with_cleanup(&mut state)? {
            return Ok(());
        }

        if Self::redrive_if_unready(
            Phase::AwaitingIdentity,
            state.phase,
            "spark.v1.response.ready",
            &payload,
        ) {
            return Ok(());
        }

        // Extract the prompt from the identity capsule's BuildResponse
        let prompt = payload
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        state.system_prompt = prompt.clone();
        state.set_phase(Phase::AwaitingPromptBuild);

        // Fetch messages from session to send to prompt builder for plugin
        // hook interception. The prompt builder's response does not echo
        // messages back, so handle_prompt_response fetches again. This
        // costs an extra session round-trip but keeps the prompt builder
        // response lean.
        let messages = Self::fetch_messages(&state.session_id)?;

        state.save()?;

        let model = env::var("model").unwrap_or_else(|_| "claude-sonnet-4-20250514".into());

        // Derive the active provider from the registry's LLM topic.
        let llm_topic = Self::active_llm_topic();
        let provider = llm_topic
            .strip_prefix("llm.v1.request.generate.")
            .unwrap_or("unknown")
            .to_string();

        // Send to prompt builder for plugin hook interception.
        // session_id is threaded through so the response echoes it back.
        ipc::publish_json(
            "prompt_builder.v1.assemble",
            &serde_json::json!({
                "messages": messages,
                "system_prompt": prompt,
                "request_id": state.request_id.to_string(),
                "session_id": state.session_id,
                "model": model,
                "provider": provider,
            }),
        )
    }

    /// Handles `prompt_builder.response.assemble` events from the prompt builder.
    ///
    /// Receives the final assembled prompt (after capsule hooks) and publishes
    /// an LLM generation request to the provider capsule.
    #[astrid::interceptor("handle_prompt_response")]
    pub fn handle_prompt_response(&self, payload: serde_json::Value) -> Result<(), SysError> {
        // Resolve session from the echoed session_id in the prompt builder response.
        let session_id = payload
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_SESSION_ID);

        let mut state = TurnState::load(session_id);

        if Self::check_timeout_with_cleanup(&mut state)? {
            return Ok(());
        }

        if Self::redrive_if_unready(
            Phase::AwaitingPromptBuild,
            state.phase,
            "prompt_builder.v1.response.assemble",
            &payload,
        ) {
            return Ok(());
        }

        // Apply the assembled system prompt from the prompt builder.
        if let Some(prompt) = payload.get("system_prompt").and_then(|v| v.as_str()) {
            state.system_prompt = prompt.to_string();
        }

        // Parse tools and messages from the prompt builder response.
        let tools: Vec<LlmToolDefinition> =
            parse_json_array_field(&payload, "tools", "tool schema");
        let mut messages: Vec<Message> = parse_json_array_field(&payload, "messages", "message");

        // Apply user context prefix to the LOCAL COPY ONLY.
        // Session's copy stays clean - this is an ephemeral transform.
        if let Some(prefix) = payload.get("user_context_prefix").and_then(|v| v.as_str())
            && !prefix.is_empty()
            && let Some(last_user_msg) = messages
                .iter_mut()
                .rev()
                .find(|m| matches!(m.role, MessageRole::User))
            && let MessageContent::Text(ref mut text) = last_user_msg.content
        {
            *text = format!("{prefix}\n{text}");
        }

        // Store tools in turn state for reuse in tool result iterations.
        state.current_tools = tools;
        state.set_phase(Phase::Streaming);
        state.save()?;

        Self::publish_llm_request(&state, &messages)
    }

    /// Handles `llm.stream.*` events from the LLM provider capsule.
    ///
    /// Accumulates text deltas and tool call deltas. When `StreamEvent::Done`
    /// arrives, evaluates whether to dispatch tool calls or emit the final
    /// response.
    #[astrid::interceptor("handle_llm_stream")]
    pub fn handle_llm_stream(&self, payload: IpcPayload) -> Result<(), SysError> {
        let (request_id, event) = match payload {
            IpcPayload::LlmStreamEvent { request_id, event } => (request_id, event),
            _ => return Ok(()),
        };

        // Resolve session from the request_id -> session_id mapping
        // stored when the LLM request was published.
        let session_id = match lookup_session_by_request(&request_id) {
            Some(sid) => sid,
            None => return Ok(()), // Unknown request, ignore
        };

        let mut state = TurnState::load(&session_id);

        if Self::check_timeout_with_cleanup(&mut state)? {
            return Ok(());
        }

        if state.phase != Phase::Streaming {
            return Ok(());
        }

        // Verify this stream belongs to our current request
        if state.request_id != request_id {
            return Ok(());
        }

        // Reset the phase timer on each streaming event so the timeout
        // measures "time since last token" not "time since stream start".
        // Prevents slow models (local inference) from timing out while
        // actively streaming.
        state.phase_entered_at_ms = now_ms();

        match event {
            StreamEvent::TextDelta(text) => {
                state.response_text.push_str(&text);
            }
            StreamEvent::ToolCallStart { id, name } => {
                state.pending_stream_tools.push(PendingToolCall {
                    id,
                    name,
                    args_json: String::new(),
                    complete: false,
                });
            }
            StreamEvent::ToolCallDelta { id, args_delta } => {
                if let Some(tc) = state.pending_stream_tools.iter_mut().find(|t| t.id == id) {
                    tc.args_json.push_str(&args_delta);
                }
            }
            StreamEvent::ToolCallEnd { id } => {
                if let Some(tc) = state.pending_stream_tools.iter_mut().find(|t| t.id == id) {
                    tc.complete = true;
                }
            }
            StreamEvent::Done => {
                return Self::handle_stream_done(&mut state);
            }
            StreamEvent::Error(err) => {
                log::error(format!("LLM stream error: {err}"));
                let _ = ipc::publish_json(
                    "agent.v1.response",
                    &IpcPayload::AgentResponse {
                        text: format!("LLM error: {err}"),
                        is_final: true,
                        session_id: state.session_id.clone(),
                    },
                );
                // Clean up ALL in-flight mappings (req2sess + call2sess)
                // before resetting turn state to prevent orphaned KV entries.
                Self::cleanup_inflight_mappings(&state);
                state.reset_conversation_turn();
                state.set_phase(Phase::Idle);
                state.save()?;
                return Ok(());
            }
            // Usage and ReasoningDelta are informational, no state change needed
            _ => {}
        }

        state.save()?;
        Ok(())
    }

    /// Handles `tool.execute.result` events from the tool router.
    ///
    /// Records the result for the completed tool call. When all dispatched
    /// tool calls have results, appends them to session and publishes the
    /// next LLM generation request.
    #[astrid::interceptor("handle_tool_result")]
    pub fn handle_tool_result(&self, payload: IpcPayload) -> Result<(), SysError> {
        let (call_id, result) = match payload {
            IpcPayload::ToolExecuteResult { call_id, result } => (call_id, result),
            _ => return Ok(()),
        };

        // Resolve session from the call_id -> session_id mapping
        // stored when the tool call was dispatched.
        let session_id = match lookup_session_by_call(&call_id) {
            Some(sid) => sid,
            None => return Ok(()), // Unknown call, ignore
        };

        let mut state = TurnState::load(&session_id);

        if Self::check_timeout_with_cleanup(&mut state)? {
            return Ok(());
        }

        if state.phase != Phase::AwaitingTools {
            return Ok(());
        }

        // Record the result for this tool call.
        // Verify turn_request_id matches to reject stale results from a previous turn.
        if let Some(tc) = state.dispatched_tools.iter_mut().find(|t| t.id == call_id) {
            if !tc.turn_request_id.is_nil() && tc.turn_request_id != state.request_id {
                log::warn(format!(
                    "Dropping stale tool result for {}: turn_request_id mismatch",
                    call_id
                ));
                return Ok(());
            }
            tc.result = Some(result);
        }

        // Check if all dispatched tools have results.
        // Guard against vacuous truth: empty dispatched_tools means the
        // turn was reset (e.g. by a new user prompt) and this is a stale
        // tool result arriving late.
        let all_done = !state.dispatched_tools.is_empty()
            && state.dispatched_tools.iter().all(|t| t.result.is_some());
        if !all_done {
            state.save()?;
            return Ok(());
        }

        // Check iteration bound BEFORE appending tool results to session
        // history. If we exceed the limit, we don't want orphaned tool-call
        // messages in history with no subsequent assistant response.
        state.iteration_count += 1;
        let max_iterations = u32::try_from(get_config_u64(
            "max_iterations",
            u64::from(DEFAULT_MAX_ITERATIONS),
        ))
        .unwrap_or(DEFAULT_MAX_ITERATIONS);

        let call_ids: Vec<String> = state
            .dispatched_tools
            .iter()
            .map(|t| t.id.clone())
            .collect();

        if state.iteration_count >= max_iterations {
            log::error(format!(
                "ReAct loop exceeded {max_iterations} iterations, forcing stop"
            ));
            let _ = ipc::publish_json(
                "agent.v1.response",
                &IpcPayload::AgentResponse {
                    text: format!(
                        "Stopped: ReAct loop exceeded maximum of {max_iterations} iterations."
                    ),
                    is_final: true,
                    session_id: state.session_id.clone(),
                },
            );
            state.reset_conversation_turn();
            state.set_phase(Phase::Idle);
            state.save()?;
            delete_call_sessions(&call_ids);
            return Ok(());
        }

        // Build clean messages for session.
        let tool_calls: Vec<ToolCall> = state
            .dispatched_tools
            .iter()
            .map(|t| ToolCall {
                id: t.id.clone(),
                name: t.name.clone(),
                arguments: t.arguments.clone(),
            })
            .collect();

        let mut session_messages = vec![Message::assistant_with_tools(tool_calls)];
        for tc in &state.dispatched_tools {
            if let Some(ref result) = tc.result {
                session_messages.push(Message {
                    role: MessageRole::Tool,
                    content: MessageContent::ToolResult(result.clone()),
                });
            }
        }

        // Fetch fresh history with atomic append-before-read BEFORE
        // deleting call2sess mappings. If this fails, mappings survive
        // and a retry from the same tool result can re-enter this path.
        let messages = Self::fetch_messages_with_append(&state.session_id, &session_messages)?;

        // Commit new state before removing mappings so a save failure
        // doesn't orphan the session with deleted mappings.
        state.reset_turn();
        state.set_phase(Phase::Streaming);
        state.save()?;

        // Safe to clean up now - new state is committed.
        delete_call_sessions(&call_ids);

        Self::publish_llm_request(&state, &messages)
    }

    /// Handle active model change from the registry capsule.
    ///
    /// Stores the new provider topic in KV so subsequent LLM requests
    /// route to the correct provider. Validates that the topic follows
    /// the expected `llm.request.generate.*` pattern as defense-in-depth.
    /// Caches context window limits from the provider metadata.
    #[astrid::interceptor("handle_model_changed")]
    pub fn handle_model_changed(&self, payload: serde_json::Value) -> Result<(), SysError> {
        if let Some(topic) = payload.get("request_topic").and_then(|t| t.as_str()) {
            if !topic.starts_with("llm.v1.request.generate.") {
                log::warn(format!("Rejected model change with invalid topic: {topic}"));
                return Ok(());
            }
            kv::set_bytes("llm_provider_topic", topic.as_bytes())?;

            // Cache context window limits from the provider metadata.
            for (kv_key, payload_key, log_on_set) in [
                (KV_CONTEXT_WINDOW, "context_window", true),
                (KV_MAX_OUTPUT_TOKENS, "max_output_tokens", false),
            ] {
                let value = payload
                    .get(payload_key)
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                if let Err(e) = kv::set_bytes(kv_key, &value.to_le_bytes()) {
                    log::error(format!("Failed to cache {payload_key}: {e}"));
                } else if log_on_set && value > 0 {
                    log::info(format!("Cached provider {payload_key}: {value} tokens"));
                }
            }
        } else {
            log::warn("handle_model_changed: payload missing 'request_topic', ignoring");
        }
        Ok(())
    }
}

impl ReactLoop {
    /// Check phase timeout and clean up in-flight KV mappings if timed out.
    ///
    /// Returns `true` if the phase timed out and was reset to Idle.
    fn check_timeout_with_cleanup(state: &mut TurnState) -> Result<bool, SysError> {
        if state.phase == Phase::Idle {
            return Ok(false);
        }
        // Snapshot in-flight mapping data before potential reset
        let request_id = state.request_id;
        let call_ids: Vec<String> = state
            .dispatched_tools
            .iter()
            .map(|t| t.id.clone())
            .collect();

        if state.check_phase_timeout()? {
            // Clean up KV correlation mappings that would otherwise leak
            if !request_id.is_nil() {
                delete_request_session(&request_id);
            }
            if !call_ids.is_empty() {
                delete_call_sessions(&call_ids);
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// Called when the LLM stream finishes. Evaluates whether to dispatch
    /// tool calls or emit the final response.
    fn handle_stream_done(state: &mut TurnState) -> Result<(), SysError> {
        // Clean up the request_id -> session_id mapping now that the stream is done.
        delete_request_session(&state.request_id);

        let has_tool_calls = !state.pending_stream_tools.is_empty();

        if has_tool_calls {
            // Two-phase tool dispatch: parse all tool calls first, then publish.
            // This prevents partial dispatch if a publish fails mid-loop.
            let mut dispatched = Vec::new();

            for tc in &state.pending_stream_tools {
                let arguments: serde_json::Value = match serde_json::from_str(&tc.args_json) {
                    Ok(args) => args,
                    Err(e) => {
                        log::warn(format!(
                            "Failed to parse tool arguments for {}: {e}. Defaulting to empty object.",
                            tc.name
                        ));
                        serde_json::Value::Object(serde_json::Map::new())
                    }
                };

                dispatched.push(DispatchedToolCall {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    arguments,
                    result: None,
                    turn_request_id: state.request_id,
                });
            }

            // Store call_id -> session_id mappings BEFORE publishing so
            // results that arrive immediately are never orphaned.
            let call_ids: Vec<String> = dispatched.iter().map(|t| t.id.clone()).collect();
            store_call_sessions(&call_ids, &state.session_id)?;

            state.dispatched_tools = dispatched;
            state.pending_stream_tools.clear();
            state.set_phase(Phase::AwaitingTools);
            if let Err(e) = state.save() {
                delete_call_sessions(&call_ids);
                return Err(e);
            }

            // Phase 2: publish all tool requests. On failure, clean up
            // the mappings we wrote so they don't leak.
            for tc in &state.dispatched_tools {
                if let Err(e) = ipc::publish_json(
                    "tool.v1.request.execute",
                    &IpcPayload::ToolExecuteRequest {
                        call_id: tc.id.clone(),
                        tool_name: tc.name.clone(),
                        arguments: tc.arguments.clone(),
                    },
                ) {
                    log::error(format!("Failed to dispatch tool {}: {e}", tc.name));
                    delete_call_sessions(&call_ids);
                    let _ = ipc::publish_json(
                        "agent.v1.response",
                        &IpcPayload::AgentResponse {
                            text: format!("Failed to dispatch tool {}: {e}", tc.name),
                            is_final: true,
                            session_id: state.session_id.clone(),
                        },
                    );
                    state.set_phase(Phase::Idle);
                    state.save()?;
                    return Err(e);
                }
            }
        } else if !state.response_text.is_empty() {
            // Text response with no tool calls - conversation turn complete.
            // Use atomic append to confirm delivery to session.
            Self::fetch_messages_with_append(
                &state.session_id,
                &[Message::assistant(&state.response_text)],
            )?;

            // Publish final response to platforms
            ipc::publish_json(
                "agent.v1.response",
                &IpcPayload::AgentResponse {
                    text: state.response_text.clone(),
                    is_final: true,
                    session_id: state.session_id.clone(),
                },
            )?;

            state.set_phase(Phase::Idle);
            state.save()?;
        } else {
            // Empty response
            state.set_phase(Phase::Idle);
            state.save()?;
        }

        Ok(())
    }

    /// Clean up in-flight KV correlation mappings for the current turn.
    ///
    /// Must be called before `reset_turn()` when interrupting an active turn,
    /// otherwise stale `req2sess` and `call2sess` entries accumulate.
    fn cleanup_inflight_mappings(state: &TurnState) {
        // Always clean request mapping if one exists (Streaming, AwaitingPromptBuild, etc.)
        if !state.request_id.is_nil() {
            delete_request_session(&state.request_id);
        }
        // Clean call mappings if tools were dispatched
        if !state.dispatched_tools.is_empty() {
            let call_ids: Vec<String> = state
                .dispatched_tools
                .iter()
                .map(|t| t.id.clone())
                .collect();
            delete_call_sessions(&call_ids);
        }
    }

    /// Handle a cancel signal from the frontend.
    ///
    /// Publishes a `tool.v1.request.cancel` event so the host-level process
    /// tracker can SIGINT/SIGKILL any spawned child processes, then cleans up
    /// in-flight KV mappings and resets the turn to Idle.
    fn handle_cancel(session_id: &str) -> Result<(), SysError> {
        let mut state = TurnState::load(session_id);
        if state.phase == Phase::Idle {
            return Ok(());
        }
        log::info(format!("Cancelling turn for session {session_id}"));

        // Notify tool capsules (host-level process tracker) before cleanup.
        if state.phase == Phase::AwaitingTools && !state.dispatched_tools.is_empty() {
            let call_ids: Vec<String> = state
                .dispatched_tools
                .iter()
                .map(|t| t.id.clone())
                .collect();
            if let Err(e) = ipc::publish_json(
                "tool.v1.request.cancel",
                &IpcPayload::ToolCancelRequest { call_ids },
            ) {
                log::warn(format!("Failed to publish tool cancel event: {e}"));
            }
        }

        Self::cleanup_inflight_mappings(&state);
        state.reset_conversation_turn();
        state.set_phase(Phase::Idle);
        state.save()
    }

    /// Publish an LLM generation request to the provider capsule.
    ///
    /// Tools and messages are provided by the caller — either from the prompt
    /// builder response (initial request) or from turn state (tool iterations).
    /// Messages are compacted via the context engine if a context window budget
    /// is available.
    fn publish_llm_request(state: &TurnState, messages: &[Message]) -> Result<(), SysError> {
        let model = env::var("model").unwrap_or_else(|_| "claude-sonnet-4-20250514".into());

        let llm_topic = Self::active_llm_topic();

        // Compact messages to fit within the provider's context window.
        let messages = Self::compact_messages(&state.session_id, messages.to_vec());

        // Store request_id -> session_id mapping so handle_llm_stream
        // can resolve the owning session from the stream's request_id.
        store_request_session(&state.request_id, &state.session_id)?;

        if let Err(e) = ipc::publish_json(
            &llm_topic,
            &IpcPayload::LlmRequest {
                request_id: state.request_id,
                model,
                messages,
                tools: state.current_tools.clone(),
                system: state.system_prompt.clone(),
            },
        ) {
            delete_request_session(&state.request_id);
            return Err(e);
        }
        Ok(())
    }

    /// Fetch conversation history from the session capsule.
    fn fetch_messages(session_id: &str) -> Result<Vec<Message>, SysError> {
        Self::fetch_messages_inner(session_id, None)
    }

    /// Fetch conversation history with atomic append-before-read.
    ///
    /// The provided messages are appended to session storage and included
    /// in the returned history in a single atomic operation, eliminating
    /// the race between separate append + fetch calls.
    fn fetch_messages_with_append(
        session_id: &str,
        messages_to_append: &[Message],
    ) -> Result<Vec<Message>, SysError> {
        Self::fetch_messages_inner(session_id, Some(messages_to_append))
    }

    /// Core implementation for session message fetching.
    ///
    /// Uses the SDK's `ipc::request_response` helper, which generates a
    /// correlation id, subscribes to
    /// `session.v1.response.get_messages.<correlation_id>` BEFORE
    /// publishing (preventing delivery race), and drops the
    /// subscription on every return path.
    ///
    /// # IPC envelope format
    ///
    /// Session publishes `{correlation_id, messages}` at the root of the
    /// reply payload (see `astrid-capsule-session::handle_get_messages`).
    /// `ipc::request_response` deserialises the raw JSON without an envelope
    /// wrapper, so `messages` lives at the root of `response`.
    fn fetch_messages_inner(
        session_id: &str,
        append_before_read: Option<&[Message]>,
    ) -> Result<Vec<Message>, SysError> {
        let timeout = session_timeout_ms();

        let mut request = serde_json::json!({ "session_id": session_id });
        if let Some(msgs) = append_before_read
            && !msgs.is_empty()
        {
            request["append_before_read"] = serde_json::to_value(msgs).map_err(|e| {
                SysError::ApiError(format!("Failed to serialize append messages: {e}"))
            })?;
        }

        let response: serde_json::Value = ipc::request_response(
            "session.v1.request.get_messages",
            "session.v1.response.get_messages",
            &request,
            timeout,
        )?;

        // Session publishes `{correlation_id, messages}` at the top
        // level of the response payload (see
        // `astrid-capsule-session::handle_get_messages`). No envelope
        // wrapper — the SDK's `request_response` already deserialised
        // the raw JSON payload; `messages` lives at the root.
        let messages: Vec<Message> = response
            .get("messages")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| SysError::ApiError(format!("Failed to parse session messages: {e}")))?
            .unwrap_or_default();

        Ok(messages)
    }

    /// Resolve the active LLM provider topic from the registry.
    fn active_llm_topic() -> String {
        // 1. Per-principal cache (populated by handle_model_changed for
        //    the load-time principal, and by the fetch-on-miss path
        //    below for every other principal).
        //
        //    `get_bytes_opt` distinguishes "key absent" from "key with
        //    empty value" — important because the kernel collapsed
        //    the SDK 0.7 `get_bytes` return for missing keys to an
        //    empty `Vec` (see SDK kv.rs doc), so a downstream
        //    `filter(|s| !s.is_empty())` is the only honest way to
        //    decide cache miss vs cache hit-with-empty-value.
        if let Ok(Some(bytes)) = kv::get_bytes_opt("llm_provider_topic")
            && let Ok(topic) = String::from_utf8(bytes)
            && !topic.is_empty()
        {
            return topic;
        }
        // 2. Operator env override. `env::var` collapses missing keys
        //    to `Ok("")` (SDK 0.7 documented behaviour) — relying on
        //    `unwrap_or_else` here used to silently bypass the
        //    fallback default and return the empty string, which the
        //    host then rejected as `InvalidInput` on publish. Use
        //    `var_opt` so the missing case actually falls through.
        if let Ok(Some(topic)) = env::var_opt("llm_provider_topic")
            && !topic.is_empty()
        {
            return topic;
        }
        // 3. Lazy fetch from the registry capsule. Registry broadcasts
        //    `registry.v1.active_model_changed` once at startup; every
        //    receiver caches the topic in its load-time principal's
        //    KV. Per-principal invocations (any non-default principal —
        //    every gateway-minted bearer) start with an empty cache
        //    and have to ask the registry directly the first time they
        //    publish. Subscribe before publish to avoid a delivery
        //    race; the subscription is dropped at scope exit.
        if let Some(topic) = Self::fetch_active_llm_topic_from_registry() {
            return topic;
        }
        // 4. Sane default. Reachable when neither the cache, env
        //    override, nor the registry has a usable provider —
        //    surfaces upstream as a publish failure rather than a
        //    silent stamp on the wrong topic.
        "llm.v1.request.generate.anthropic".into()
    }

    /// Fetch the active LLM provider's `request_topic` from the
    /// registry capsule via a synchronous request/response round-trip.
    /// Caches the result (topic + context window + max output tokens)
    /// into the current principal's KV so subsequent prompts skip the
    /// IPC hop.
    ///
    /// Returns `None` if the registry doesn't reply within 5s, has no
    /// active model, or returns a payload missing `request_topic`.
    fn fetch_active_llm_topic_from_registry() -> Option<String> {
        const RESPONSE_TOPIC: &str = "registry.v1.response.get_active_model";
        const REQUEST_TOPIC: &str = "registry.v1.get_active_model";
        const TIMEOUT_MS: u64 = 5_000;

        // Subscribe BEFORE publishing so the registry's reply (which
        // may be inline-synchronous on its dispatcher task) can't slip
        // through before we're listening.
        let sub = ipc::subscribe(RESPONSE_TOPIC).ok()?;
        if ipc::publish_json(REQUEST_TOPIC, &serde_json::json!({})).is_err() {
            return None;
        }
        let result = sub.recv(TIMEOUT_MS).ok()?;
        let msg = result.messages.first()?;

        // Registry replies with `Option<ProviderEntry>` — JSON `null`
        // means "no active model"; anything else is the active entry
        // shape with a `request_topic` field.
        let payload: serde_json::Value = serde_json::from_str(&msg.payload).ok()?;
        // `null` is the valid "no active model" reply; anything that isn't an
        // object is a corrupt/unexpected response worth a warning rather than a
        // silent `None` (which is indistinguishable from "no model").
        if payload.is_null() {
            return None;
        }
        let provider = payload.as_object().or_else(|| {
            log::warn("react: registry active-model response is not a JSON object");
            None
        })?;
        let topic = provider.get("request_topic")?.as_str()?.to_string();
        if topic.is_empty() {
            return None;
        }

        // Best-effort cache so subsequent prompts under this principal
        // skip the round-trip. Errors are non-fatal — a transient KV
        // failure just means we'll re-fetch next time.
        let _ = kv::set_bytes("llm_provider_topic", topic.as_bytes());
        for (kv_key, payload_key) in [
            (KV_CONTEXT_WINDOW, "context_window"),
            (KV_MAX_OUTPUT_TOKENS, "max_output_tokens"),
        ] {
            if let Some(v) = provider
                .get(payload_key)
                .and_then(serde_json::Value::as_u64)
            {
                let _ = kv::set_bytes(kv_key, &v.to_le_bytes());
            }
        }

        Some(topic)
    }

    /// Read cached context window from KV. Returns `None` if not yet queried.
    fn cached_context_window() -> Option<u64> {
        kv::get_bytes(KV_CONTEXT_WINDOW)
            .ok()
            .and_then(|b| <[u8; 8]>::try_from(b.as_slice()).ok())
            .map(u64::from_le_bytes)
            .filter(|&v| v > 0)
    }

    /// Read cached max output tokens from KV.
    fn cached_max_output_tokens() -> u64 {
        kv::get_bytes(KV_MAX_OUTPUT_TOKENS)
            .ok()
            .and_then(|b| <[u8; 8]>::try_from(b.as_slice()).ok())
            .map(u64::from_le_bytes)
            .unwrap_or(0)
    }

    /// Compact messages via the context engine if a context window budget is known.
    ///
    /// If no limits are cached (provider hasn't been queried yet), returns
    /// messages unchanged — matching the previous no-compaction behavior.
    fn compact_messages(session_id: &str, messages: Vec<Message>) -> Vec<Message> {
        let context_window = match Self::cached_context_window() {
            Some(cw) => cw,
            None => return messages,
        };

        let max_output = Self::cached_max_output_tokens();

        // Budget: total context minus output reservation.
        // Use 90% of the remaining budget as the target to leave headroom
        // for system prompt and tool schemas (estimated separately by the
        // provider, not included in the message token count).
        let max_tokens = context_window.saturating_sub(max_output);
        let target_tokens = max_tokens * COMPACTION_TARGET_NUM / COMPACTION_TARGET_DENOM;

        if max_tokens == 0 {
            return messages;
        }

        // The context engine publishes to a static response topic (no
        // correlation id), so we can't use `ipc::request_response` here.
        // Hold the `Subscription` in scope so Drop tears it down on
        // every return path; no manual `unsubscribe` needed.
        let sub = match ipc::subscribe(COMPACT_RESPONSE_TOPIC) {
            Ok(h) => h,
            Err(_) => return messages,
        };

        let result = (|| -> Option<Vec<Message>> {
            let msg_values: Vec<serde_json::Value> = messages
                .iter()
                .filter_map(|m| serde_json::to_value(m).ok())
                .collect();

            let request = serde_json::json!({
                "session_id": session_id,
                "messages": msg_values,
                "max_tokens": max_tokens,
                "target_tokens": target_tokens,
            });

            if ipc::publish_json(COMPACT_REQUEST_TOPIC, &request).is_err() {
                return None;
            }

            let poll_result = sub.recv(DEFAULT_COMPACT_TIMEOUT_MS).ok()?;

            // Navigate PollResult: first message's payload -> data
            let first_msg = poll_result.messages.first()?;
            let payload: serde_json::Value = serde_json::from_str(&first_msg.payload).ok()?;
            let data = payload.get("data")?;

            let compacted_msgs: Vec<serde_json::Value> =
                serde_json::from_value(data.get("messages")?.clone()).ok()?;

            let result: Vec<Message> = compacted_msgs
                .into_iter()
                .filter_map(|v| serde_json::from_value(v).ok())
                .collect();

            if let Some(true) = data.get("compacted").and_then(|v| v.as_bool()) {
                let removed = data
                    .get("messages_removed")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                log::info(format!(
                    "Context compaction: removed {removed} messages \
                         (budget: {max_tokens} tokens, target: {target_tokens})"
                ));
            }

            Some(result)
        })();

        result.unwrap_or(messages)
    }
}

/// Parse a JSON array field from a payload, deserializing each element.
///
/// Logs a warning for each element that fails to deserialize and skips it.
/// Returns an empty vec if the field is missing or not an array.
fn parse_json_array_field<T: serde::de::DeserializeOwned>(
    payload: &serde_json::Value,
    key: &str,
    label: &str,
) -> Vec<T> {
    payload
        .get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| {
                    serde_json::from_value::<T>(v.clone())
                        .map_err(|e| {
                            log::warn(format!("Failed to parse {label} from prompt builder: {e}"));
                            e
                        })
                        .ok()
                })
                .collect()
        })
        .unwrap_or_default()
}
