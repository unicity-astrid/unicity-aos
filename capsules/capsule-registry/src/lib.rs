#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]

//! LLM Provider Registry capsule.
//!
//! Discovers available LLM providers via IPC fan-out and manages model
//! selection. Provider capsules respond to `llm.v1.request.describe` on
//! `llm.v1.response.describe` with their capabilities and routing topics,
//! following the same pattern as tool discovery
//! (`tool.v1.request.describe`).
//!
//! # IPC Protocol
//!
//! **Queries** (publish to these topics, registry responds on `registry.v1.response.*`):
//! - `registry.v1.get_providers` — returns list of available LLM providers
//! - `registry.v1.get_active_model` — returns the currently active model
//! - `registry.v1.set_active_model` — payload: `{"model_id": "...", "corr_id"?: "..."}`,
//!   sets active model. The optional `corr_id` is echoed verbatim into the
//!   `registry.v1.response.set_active_model` reply (ok and error) so a gateway
//!   can disambiguate concurrent same-principal SET replies; omitted when absent.
//!
//! **Events** (published by registry):
//! - `registry.v1.active_model_changed` — payload: `ProviderEntry` on a model
//!   change, or JSON `null` when the active model is cleared
//!
//! **Provider discovery** (capsule-to-capsule IPC, replaces the
//! removed `hooks::trigger` fan-out):
//! - publishes `llm.v1.request.describe`
//! - collects responses on `llm.v1.response.describe` for a short
//!   bounded window
//! - each provider responds with `{"providers": [...]}`

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

use astrid_sdk::prelude::*;

// Use the shared contract type — generated from WIT, serde-enabled.
use astrid_sdk::contracts::registry::ProviderEntry;

mod selection;
use selection::{
    ReconcileOutcome, RegistryState, auto_select_defaults_in_place, is_known_subcommand,
    models_result, reconcile_active_model_in_place, request_topic_qualifier, resolve_selection,
    subcommand_needs_discovery,
};

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The kernel's system session UUID, used to validate IPC messages from the kernel.
const KERNEL_UUID: &str = "00000000-0000-0000-0000-000000000000";

/// IPC topics for capsule-to-capsule LLM provider discovery.
const LLM_DESCRIBE_REQUEST_TOPIC: &str = "llm.v1.request.describe";
const LLM_DESCRIBE_RESPONSE_TOPIC: &str = "llm.v1.response.describe";

/// CLI run/result protocol topics (the scriptable `models` verb). The run
/// topic is the providing capsule id; results are keyed by request id. See
/// `astrid` `crates/astrid-cli/src/commands/capsule_verb.rs`.
const CLI_RUN_TOPIC: &str = "cli.v1.command.run.astrid-capsule-registry";
const CLI_RESULT_TOPIC_PREFIX: &str = "cli.v1.command.result.";

/// Maximum accepted length of an incoming `req_id`. The CLI sends a 32-char
/// hex simple UUID; allow a little slack (e.g. the hyphenated 36-char form)
/// but bound it so a hostile value can't bloat the derived topic.
const MAX_REQ_ID_LEN: usize = 64;

/// Validate an incoming `req_id` before it is interpolated into the result
/// topic `cli.v1.command.result.<req_id>`.
///
/// `req_id` is UNTRUSTED — it arrives in the run payload. The CLI subscribes
/// to `cli.v1.command.result.*`, a single trailing wildcard *segment*. A
/// `req_id` containing a topic separator (`.`) or a wildcard (`*`) would
/// publish to a topic that does not match that subscription (it would split
/// into extra segments), so the reply would silently never reach the caller —
/// or could be steered onto an unrelated topic.
///
/// We therefore accept ONLY the exact shape the CLI emits: a `Uuid`, either in
/// simple form (`Uuid::new_v4().simple()` = 32 lowercase-hex chars) or in
/// hyphenated form (`8-4-4-4-12` lowercase hex). That is lowercase hex digits
/// plus `-`, within a length bound. Everything else — uppercase, other ASCII
/// alphanumerics, `.`, `*`, whitespace, empty, or oversized — is rejected.
pub(crate) fn is_valid_req_id(req_id: &str) -> bool {
    fn is_lower_hex_or_hyphen(b: u8) -> bool {
        b.is_ascii_digit() || matches!(b, b'a'..=b'f') || b == b'-'
    }
    !req_id.is_empty()
        && req_id.len() <= MAX_REQ_ID_LEN
        && req_id.bytes().all(is_lower_hex_or_hyphen)
}

/// The only `command` this capsule's CLI run topic implements. The run topic
/// suffix is the package id returned as `provider_capsule` by GetCommands. It
/// is per-capsule, not per-verb, so the payload's `command` field must be
/// validated against this before its args are treated as a `models` subcommand.
const CLI_RUN_COMMAND: &str = "models";

/// Whether a CLI run payload's `command` field names the verb this capsule
/// implements. Extracted as a pure predicate so the gating decision is
/// unit-testable without the IO-buried dispatch loop.
pub(crate) fn is_models_command(payload: &serde_json::Value) -> bool {
    payload.get("command").and_then(|v| v.as_str()) == Some(CLI_RUN_COMMAND)
}

/// Time the discovery routine waits for provider capsules to respond
/// after the describe request is published. Provider capsules typically
/// reply synchronously inside their interceptor, but we still give the
/// bus a generous window to settle.
const DISCOVERY_TIMEOUT_MS: u64 = 500;

const STATE_KEY: &str = "registry_state";

fn load_state() -> RegistryState {
    kv::get_json::<RegistryState>(STATE_KEY).unwrap_or_default()
}

/// Persist registry state with an atomic compare-and-swap so concurrent
/// writers (e.g. a `/models` command landing while a discovery refresh
/// is in flight) don't silently clobber each other. Falls back to a
/// plain write if CAS isn't available for this key (first write) or if
/// the host rejects the call.
fn save_state(state: &RegistryState) {
    let new_bytes = match serde_json::to_vec(state) {
        Ok(b) => b,
        Err(e) => {
            log::warn(format!("Failed to serialize registry state: {e}"));
            return;
        }
    };
    let expected = kv::get_bytes_opt(STATE_KEY).ok().flatten();
    match kv::cas(STATE_KEY, expected.as_deref(), &new_bytes) {
        Ok(true) => {}
        Ok(false) => {
            // Lost the race; the next read+write cycle will reconcile.
            log::debug("Registry state CAS lost race; deferring to next write");
        }
        Err(e) => {
            log::warn(format!(
                "Registry state CAS failed ({e}); falling back to set_bytes"
            ));
            let _ = kv::set_bytes(STATE_KEY, &new_bytes);
        }
    }
}

/// Discover LLM providers via IPC fan-out.
///
/// Replaces the removed `hooks::trigger` host fn with a publish + drain
/// pattern:
/// 1. Subscribe to `llm.v1.response.describe` *before* publishing so the
///    response cannot race the subscription.
/// 2. Publish a describe request.
/// 3. Drain responses for [`DISCOVERY_TIMEOUT_MS`].
///
/// Each provider capsule responds with `{"providers": [...]}`. The
/// subscription handle drops at function exit, releasing the kernel-side
/// resource.
fn discover_providers() -> Vec<ProviderEntry> {
    let response_sub = match ipc::subscribe(LLM_DESCRIBE_RESPONSE_TOPIC) {
        Ok(s) => s,
        Err(e) => {
            log::warn(format!(
                "Failed to subscribe to {LLM_DESCRIBE_RESPONSE_TOPIC}: {e}"
            ));
            return Vec::new();
        }
    };

    let request_payload = serde_json::json!({});
    if let Err(e) = ipc::publish_json(LLM_DESCRIBE_REQUEST_TOPIC, &request_payload) {
        log::warn(format!(
            "Failed to publish {LLM_DESCRIBE_REQUEST_TOPIC}: {e}"
        ));
        return Vec::new();
    }

    let mut providers: Vec<ProviderEntry> = Vec::new();
    // Drain whatever arrives in the window. `recv` returns as soon as a message
    // is available (it does NOT block the full step when there's traffic), so we
    // must bound the loop by REAL elapsed time, not by subtracting the nominal
    // step per iteration. A burst of provider responses would otherwise return
    // many early `recv`s, each charging the full `step` against the budget, and
    // close the window after only a handful of iterations — dropping providers
    // that respond slightly later. The monotonic clock measures actual elapsed
    // time (only differences are meaningful; see SDK `time::monotonic`).
    let start = astrid_sdk::time::monotonic();
    let budget = DISCOVERY_TIMEOUT_MS;
    loop {
        let elapsed_ms = u64::try_from(
            astrid_sdk::time::monotonic()
                .saturating_sub(start)
                .as_millis(),
        )
        .unwrap_or(u64::MAX);
        let remaining = budget.saturating_sub(elapsed_ms);
        if remaining == 0 {
            break;
        }
        // Cap each blocking wait so we re-check the deadline periodically even
        // when no traffic arrives.
        let step = remaining.min(100);
        match response_sub.recv(step) {
            Ok(result) => {
                // One message = one kernel-stamped source. Process each
                // message's `providers` array AS A GROUP so the source's
                // entry[0]-first emit order is preserved. The route qualifier
                // is validated as a concrete LLM generate topic, but it is not
                // forced to equal the capsule package name.
                for msg in &result.messages {
                    providers.extend(stamp_message_providers(&msg.payload, &msg.source_id));
                }
            }
            Err(_) => {
                // Host error (not a plain timeout — the host signals an idle
                // window with an empty `Ok` envelope, see SDK ipc::recv). Stop
                // draining; the deadline loop would otherwise spin on the error.
                break;
            }
        }
    }

    dedupe_providers_by_id(providers)
}

fn dedupe_providers_by_id(providers: Vec<ProviderEntry>) -> Vec<ProviderEntry> {
    let mut seen = HashSet::with_capacity(providers.len());
    providers
        .into_iter()
        .filter(|provider| seen.insert(provider.id.clone()))
        .collect()
}

/// Parse one describe message's `{"providers": [...]}` body and return its
/// entries with canonical `"<provider>:<model>"` ids in the provider's emit
/// order (entry[0] = that provider's default hint). Each entry must name a
/// concrete `llm.v1.request.generate.<provider>` route; the trailing provider
/// qualifier is a routing alias, not a package identity assertion.
///
/// A malformed payload, or one with no `providers` array, returns empty
/// SILENTLY (no warning) — it is treated as "this message carried no
/// providers". Only a per-entry invalid route is dropped WITH a warning. An
/// entry whose JSON does not deserialize into a `ProviderEntry` is skipped
/// silently.
///
/// The `<model>` half of the canonical id is the provider-reported entry
/// `id`. Against today's single-entry providers that is the bare provider
/// name, yielding e.g. `"openai-compat:openai-compat"` — graceful
/// degradation, still resolvable by the bare name.
fn stamp_message_providers(payload: &str, source_id: &str) -> Vec<ProviderEntry> {
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(payload) else {
        return Vec::new();
    };
    let Some(arr) = payload.get("providers").and_then(|p| p.as_array()) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for entry in arr {
        let Ok(mut p) = serde_json::from_value::<ProviderEntry>(entry.clone()) else {
            continue;
        };
        // Each entry carries its own route topic. The suffix is the provider
        // qualifier used for model selection, but it is deliberately not
        // required to equal the package name encoded by `source_id`.
        let Some(capsule) = request_topic_qualifier(&p.request_topic) else {
            log::warn(format!(
                "Dropping provider entry '{}' (request_topic '{}'): source '{}' did not provide a concrete LLM generate route",
                p.id, p.request_topic, source_id
            ));
            continue;
        };
        // `<model>` is the provider-reported id; stamp the canonical form.
        p.id = format!("{capsule}:{}", p.id);
        out.push(p);
    }
    out
}

/// Check whether a poll result contains at least one message from the kernel.
fn has_kernel_message(result: &ipc::PollResult) -> bool {
    result
        .messages
        .iter()
        .any(|msg| msg.source_id == KERNEL_UUID)
}

/// Publish the active model changed event so the react loop (and frontends) can respond.
fn publish_model_changed(provider: &ProviderEntry) {
    let _ = ipc::publish_json("registry.v1.active_model_changed", provider);
}

/// The payload broadcast when the active model is *cleared*. A normal change
/// carries the new `ProviderEntry`; a clear carries JSON `null`, signalling
/// "no active model" so warm-cache subscribers (react) drop the stale binding
/// instead of routing to it until their next lazy fetch.
fn cleared_payload() -> serde_json::Value {
    serde_json::Value::Null
}

/// Publish the active-model-cleared event on the same `active_model_changed`
/// topic with a `null` payload (see [`cleared_payload`]). Without this,
/// clearing the selection persists silently and downstream keeps using the
/// last-known model until it happens to re-fetch.
fn publish_model_cleared() {
    let _ = ipc::publish_json("registry.v1.active_model_changed", &cleared_payload());
}

/// Handle a `registry.v1.get_providers` request.
fn handle_get_providers() {
    let providers = discover_providers();
    let mut state = load_state();

    if !providers.is_empty() {
        state.providers = providers;
        save_state(&state);
    } else if state.providers.is_empty() {
        log::warn("Provider discovery returned empty and no cached providers exist");
    }

    let _ = ipc::publish_json("registry.v1.response.get_providers", &state.providers);
}

/// Handle a `registry.v1.get_active_model` request.
///
/// Registry state (provider list + active model id) is stored in
/// the capsule's KV, which the kernel scopes by invocation principal.
/// The `active_model_changed` broadcast at boot only primes the
/// load-time principal's KV — every other principal arrives with an
/// empty store and would otherwise see `None` here on first ask,
/// then the gateway-side caller (react.active_llm_topic) would fall
/// back to its hardcoded `llm.v1.request.generate.anthropic` topic,
/// which has no subscriber on a typical LM Studio install.
///
/// Detect that case (empty providers OR missing active id) and
/// re-run the discover + auto-select dance under the invoking
/// principal. The persisted state ends up keyed to that principal,
/// so subsequent calls skip the round-trip. Discovery only adds one
/// 500ms describe-fanout window on first ask per principal.
fn handle_get_active_model() {
    let mut state = load_state();

    // Discover providers only when we have NONE cached. A populated provider
    // set with no active model selected (multiple providers, none chosen) is a
    // valid steady state — re-running the ~500ms describe fan-out on every call
    // just because `active_model_id` is `None` would stall the loop under load
    // and contradicts the "discover once per principal" contract above.
    if state.providers.is_empty() {
        let providers = discover_providers();
        if !providers.is_empty() {
            state.providers = providers;
            save_state(&state);
        }
    }

    // Reconcile a stale active id (remap an old bare provider-id selection to
    // that capsule's default after a single->multi upgrade, or clear a
    // genuinely-gone one) and auto-select a default when nothing is selected.
    // Both take `&mut state` and mutate it in place (persisting only when they
    // actually change something — `auto_select_defaults` is a no-op when a
    // model is already selected), so the local copy is already current and no
    // reload is needed.
    reconcile_active_model(&mut state);
    auto_select_defaults(&mut state);

    let active = state
        .active_model_id
        .as_ref()
        .and_then(|id| state.providers.iter().find(|p| &p.id == id));

    let _ = ipc::publish_json("registry.v1.response.get_active_model", &active);
}

/// Extract an optional field from a request payload, mirroring the `model_id`
/// lookup: nested under `data` first, then at the top level. Returns an owned
/// `String` so the value survives the borrow of `payload`.
fn extract_request_field(payload: &serde_json::Value, key: &str) -> Option<String> {
    payload
        .get("data")
        .and_then(|d| d.get(key))
        .and_then(|v| v.as_str())
        .or_else(|| payload.get(key).and_then(|v| v.as_str()))
        .map(str::to_string)
}

/// Build the success response object for a `set_active_model` request,
/// echoing the request's `corr_id` verbatim when present and omitting the
/// field entirely when absent (back-compat: callers that send no `corr_id`
/// must see an unchanged response body).
fn set_active_model_ok_response(
    provider: &ProviderEntry,
    corr_id: Option<&str>,
) -> serde_json::Value {
    let mut obj = serde_json::json!({"status": "ok", "active_model": provider});
    if let Some(corr_id) = corr_id {
        obj["corr_id"] = serde_json::Value::String(corr_id.to_string());
    }
    obj
}

/// Build the error response object for a `set_active_model` request, echoing
/// the request's `corr_id` verbatim when present and omitting it otherwise.
/// Mirrors [`set_active_model_ok_response`] so a correlated caller can match
/// either outcome by `corr_id`.
fn set_active_model_error_response(error: &str, corr_id: Option<&str>) -> serde_json::Value {
    let mut obj = serde_json::json!({"error": error});
    if let Some(corr_id) = corr_id {
        obj["corr_id"] = serde_json::Value::String(corr_id.to_string());
    }
    obj
}

/// Handle a `registry.v1.set_active_model` request.
///
/// Reads the OPTIONAL `corr_id` alongside `model_id` (same extraction shape)
/// and echoes it verbatim into every reply on
/// `registry.v1.response.set_active_model`. This lets the gateway, when two
/// concurrent same-principal SET requests race on the routed reply stream,
/// keep its own reply and skip the other — closing the wrong-response-body
/// race (state itself was already correct via the CAS in `save_state`).
fn handle_set_active_model(payload: &serde_json::Value) {
    let corr_id = extract_request_field(payload, "corr_id");
    let model_id = match extract_request_field(payload, "model_id") {
        Some(id) => id,
        None => {
            let _ = ipc::publish_json(
                "registry.v1.response.set_active_model",
                &set_active_model_error_response("missing model_id", corr_id.as_deref()),
            );
            return;
        }
    };

    set_active_model_by_id(&model_id, corr_id.as_deref());
}

/// Set the active model by operator input (extracted helper for reuse).
///
/// Resolves the input against the discovered/cached canonical entries via
/// [`resolve_selection`] (accepting a bare model name when unambiguous), then
/// persists `active_model_id` in CANONICAL `"<capsule>:<model>"` form — never
/// the raw bare input, so a later install cannot retroactively make the
/// stored selection ambiguous.
///
/// `corr_id` is the optional correlation id of the originating
/// `set_active_model` request; it is echoed verbatim into the reply when
/// `Some`. Non-request callers (CLI `/models`, CLI `run`, TUI selection
/// callback) pass `None`, preserving the existing uncorrelated reply body.
fn set_active_model_by_id(model_id: &str, corr_id: Option<&str>) {
    let mut state = load_state();

    if state.providers.is_empty() {
        state.providers = discover_providers();
    }

    match resolve_selection(model_id, &state.providers) {
        Ok(provider) => {
            let provider = provider.clone();
            state.active_model_id = Some(provider.id.clone());
            save_state(&state);
            publish_model_changed(&provider);
            let _ = ipc::publish_json(
                "registry.v1.response.set_active_model",
                &set_active_model_ok_response(&provider, corr_id),
            );
        }
        Err(e) => {
            let _ = ipc::publish_json(
                "registry.v1.response.set_active_model",
                &set_active_model_error_response(&e.message(), corr_id),
            );
        }
    }
}

/// Reconcile a stored `active_model_id` against the current entries, persisting
/// and emitting the change event when one occurs.
///
/// Persists only when the value actually changes (mirrors the CAS discipline —
/// no write when nothing moved). Runs lazily, so a stored selection heals on
/// the next `get_active_model` or reload — no upgrade hook required.
fn reconcile_active_model(state: &mut RegistryState) {
    match reconcile_active_model_in_place(state) {
        ReconcileOutcome::Unchanged => {}
        ReconcileOutcome::Remapped { from, to } => {
            log::info(format!(
                "Migrated active model binding '{from}' -> '{}'",
                to.id
            ));
            save_state(state);
            publish_model_changed(&to);
        }
        ReconcileOutcome::Cleared { from } => {
            log::info(format!(
                "Active model '{from}' no longer available after reload, clearing"
            ));
            save_state(state);
            publish_model_cleared();
        }
    }
}

/// Auto-select a default model when none is selected (multi-model aware),
/// persisting and emitting the change event when one occurs.
fn auto_select_defaults(state: &mut RegistryState) {
    if let Some(provider) = auto_select_defaults_in_place(state) {
        save_state(state);
        publish_model_changed(&provider);
        log::info(format!("Auto-selected default LLM model: {}", provider.id));
    }
}

/// Dispatch registry messages from a poll result.
fn dispatch_registry_messages(result: &ipc::PollResult) {
    if result.dropped > 0 {
        log::warn(format!(
            "Event bus dropped {} messages in registry poll",
            result.dropped
        ));
    }

    for msg in &result.messages {
        // Skip our own response messages.
        if msg.topic.starts_with("registry.v1.response.")
            || msg.topic == "registry.v1.active_model_changed"
        {
            continue;
        }

        match msg.topic.as_str() {
            "registry.v1.get_providers" => handle_get_providers(),
            "registry.v1.get_active_model" => handle_get_active_model(),
            "registry.v1.set_active_model" => {
                if let Ok(payload) = serde_json::from_str::<serde_json::Value>(&msg.payload) {
                    handle_set_active_model(&payload);
                }
            }
            _ => {}
        }
    }
}

/// Dispatch CLI command execution messages.
fn dispatch_command_messages(result: &ipc::PollResult) {
    for msg in &result.messages {
        let payload: serde_json::Value = match serde_json::from_str(&msg.payload) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let text = payload.get("text").and_then(|t| t.as_str()).unwrap_or("");

        let parts: Vec<&str> = text.split_whitespace().collect();
        let cmd = parts.first().copied().unwrap_or("");

        if cmd == "/models" {
            if parts.len() >= 2 {
                set_active_model_by_id(parts[1], None);
            } else {
                emit_model_selection();
            }
        }
    }
}

/// Dispatch scriptable `models` verb runs over the CLI run/result protocol.
///
/// Run body: `{ req_id, command: "models", args: [...] }` (see `astrid`
/// `crates/astrid-cli/src/commands/capsule_verb.rs`). The reply is published
/// on `cli.v1.command.result.<req_id>` as `{ exit_code, output, error? }`.
///
/// `set` and `unset` mutate persisted state here (the side effect IPC/KV can't
/// be folded into the pure builder); everything else just shapes and replies.
fn dispatch_cli_run_messages(result: &ipc::PollResult) {
    for msg in &result.messages {
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(&msg.payload) else {
            continue;
        };
        let Some(req_id) = payload.get("req_id").and_then(|v| v.as_str()) else {
            continue;
        };
        // `req_id` is untrusted and is interpolated into the result topic.
        // Reject any value that isn't the safe single-segment shape the CLI
        // sends BEFORE doing any work or deriving a topic — never publish to a
        // topic built from a malformed/hostile req_id.
        if !is_valid_req_id(req_id) {
            log::warn(format!(
                "registry: dropping cli run with invalid req_id (len {})",
                req_id.len()
            ));
            continue;
        }
        // The run topic `cli.v1.command.run.<provider_capsule>` is per-CAPSULE, not
        // per-verb: every `astrid capsule <verb>` the registry declares lands
        // here. We only implement `models`, so reject any other `command` rather
        // than treating its args as a models subcommand.
        if !is_models_command(&payload) {
            let command = payload
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            log::warn(format!(
                "registry: dropping cli run for unsupported command '{command}'"
            ));
            continue;
        }
        let args: Vec<String> = payload
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|a| a.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        // Reject an unknown/empty subcommand (typo or `--help` query) BEFORE
        // provider discovery, so it doesn't wait out the ~500ms discovery
        // window. The error body is independent of discovered entries.
        if !is_known_subcommand(args.first().map(String::as_str).unwrap_or("")) {
            let body = models_result(&args, &[], None);
            let topic = format!("{CLI_RESULT_TOPIC_PREFIX}{req_id}");
            let _ = ipc::publish_json(&topic, &body);
            continue;
        }

        // Discover/cache providers so `list`/`current --json`/`set <id>` see
        // entries even on a fresh principal (mirrors the other handlers). Skip
        // the ~500ms fan-out for subcommands whose reply can't depend on
        // discovered entries (`unset`, `set` with no id, `current` without
        // `--json`) so they return promptly.
        let mut state = load_state();
        if subcommand_needs_discovery(&args) && state.providers.is_empty() {
            let providers = discover_providers();
            if !providers.is_empty() {
                state.providers = providers;
                save_state(&state);
            }
        }
        // Heal a stale binding before reporting/using `active`. With empty
        // providers (discovery skipped) this is a no-op, so the stored binding
        // is preserved.
        reconcile_active_model(&mut state);

        let body = models_result(&args, &state.providers, state.active_model_id.as_deref());

        // Apply the side effects for the mutating subcommands, but only when
        // the pure builder reported success (resolution passed).
        let succeeded = body
            .get("exit_code")
            .and_then(serde_json::Value::as_i64)
            .is_some_and(|c| c == 0);
        match args.first().map(String::as_str) {
            Some("set") if succeeded => {
                if let Some(input) = args.get(1) {
                    set_active_model_by_id(input, None);
                }
            }
            Some("unset") => {
                if state.active_model_id.is_some() {
                    state.active_model_id = None;
                    save_state(&state);
                    publish_model_cleared();
                }
            }
            _ => {}
        }

        let topic = format!("{CLI_RESULT_TOPIC_PREFIX}{req_id}");
        let _ = ipc::publish_json(&topic, &body);
    }
}

/// Dispatch selection callback messages from the TUI picker.
fn dispatch_selection_messages(result: &ipc::PollResult) {
    for msg in &result.messages {
        let payload: serde_json::Value = match serde_json::from_str(&msg.payload) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let selected_id = payload
            .get("data")
            .and_then(|d| d.get("selected_id"))
            .and_then(|v| v.as_str())
            .or_else(|| payload.get("selected_id").and_then(|v| v.as_str()));

        if let Some(model_id) = selected_id {
            set_active_model_by_id(model_id, None);
        }
    }
}

/// Discover providers and emit a `SelectionRequired` IPC payload for the TUI.
fn emit_model_selection() {
    let providers = discover_providers();
    let mut state = load_state();

    if !providers.is_empty() {
        state.providers = providers;
        save_state(&state);
    }

    if state.providers.is_empty() {
        log::warn("No LLM providers found for /models selection");
        return;
    }

    let options: Vec<serde_json::Value> = state
        .providers
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id,
                "label": p.id,
                "description": p.description,
            })
        })
        .collect();

    // `SystemTime::now()` panics on `wasm32-unknown-unknown`. The
    // monotonic host clock + a per-capsule atomic counter is enough
    // for a unique request_id at the resolution we need.
    let request_id = format!(
        "models-{}-{}",
        astrid_sdk::time::monotonic().as_nanos(),
        REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed)
    );

    let selection = serde_json::json!({
        "type": "selection_required",
        "request_id": request_id,
        "title": "Select LLM Model",
        "options": options,
        "callback_topic": "registry.v1.selection.callback",
    });

    let _ = ipc::publish_json("registry.v1.response.models", &selection);
}

#[derive(Default)]
struct Registry;

#[capsule]
impl Registry {
    #[astrid::run]
    fn run(&self) -> Result<(), SysError> {
        log::info("Registry capsule starting");

        let sub = ipc::subscribe("registry.v1.*")?;
        let cmd_sub = ipc::subscribe("cli.v1.command.execute")?;
        let run_sub = ipc::subscribe(CLI_RUN_TOPIC)?;
        let selection_sub = ipc::subscribe("registry.v1.selection.callback")?;
        let capsules_loaded_sub = ipc::subscribe("astrid.v1.capsules_loaded")?;

        let _ = runtime::signal_ready();

        // Wait for the kernel to signal that all capsules have been loaded.
        let mut capsules_ready = false;
        if let Ok(result) = capsules_loaded_sub.recv(5000)
            && has_kernel_message(&result)
        {
            capsules_ready = true;
        }

        if !capsules_ready {
            log::warn(
                "Timed out waiting for astrid.v1.capsules_loaded - proceeding with discovery anyway",
            );
        }

        // Now that all capsules are loaded, discover providers via IPC.
        let providers = discover_providers();
        let mut state = load_state();
        if !providers.is_empty() {
            state.providers = providers;
            save_state(&state);
        } else if state.providers.is_empty() {
            log::warn("Initial provider discovery returned empty and no cached providers exist");
        }
        reconcile_active_model(&mut state);
        auto_select_defaults(&mut state);

        // Event loop — blocks on the primary subscription, then drains auxiliary.
        // All five subscriptions are RAII (`Subscription`); their `Drop`
        // releases the kernel-side resource at scope exit, so no manual
        // `unsubscribe` is required.
        loop {
            match sub.recv(5000) {
                Ok(result) => dispatch_registry_messages(&result),
                Err(_) => break,
            }

            // Drain CLI command messages (non-blocking).
            if let Ok(result) = cmd_sub.poll() {
                dispatch_command_messages(&result);
            }

            // Drain scriptable `models` verb runs (non-blocking).
            if let Ok(result) = run_sub.poll() {
                dispatch_cli_run_messages(&result);
            }

            // Drain model selection callbacks from the TUI picker.
            if let Ok(result) = selection_sub.poll() {
                dispatch_selection_messages(&result);
            }

            // Re-discover providers on capsule reload.
            if let Ok(result) = capsules_loaded_sub.poll()
                && has_kernel_message(&result)
            {
                log::info("Capsules reloaded - re-discovering providers");
                let providers = discover_providers();
                let mut state = load_state();
                if !providers.is_empty() {
                    state.providers = providers;
                    save_state(&state);
                    reconcile_active_model(&mut state);
                    auto_select_defaults(&mut state);
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
