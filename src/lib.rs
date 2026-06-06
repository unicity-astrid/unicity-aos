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
//! - `registry.v1.set_active_model` — payload: `{"model_id": "..."}`, sets active model
//!
//! **Events** (published by registry):
//! - `registry.v1.active_model_changed` — payload: `ProviderEntry`, emitted on model change
//!
//! **Provider discovery** (capsule-to-capsule IPC, replaces the
//! removed `hooks::trigger` fan-out):
//! - publishes `llm.v1.request.describe`
//! - collects responses on `llm.v1.response.describe` for a short
//!   bounded window
//! - each provider responds with `{"providers": [...]}`

use std::sync::atomic::{AtomicU64, Ordering};

use astrid_sdk::prelude::*;

// Use the shared contract type — generated from WIT, serde-enabled.
use astrid_sdk::contracts::registry::ProviderEntry;

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The kernel's system session UUID, used to validate IPC messages from the kernel.
const KERNEL_UUID: &str = "00000000-0000-0000-0000-000000000000";

/// IPC topics for capsule-to-capsule LLM provider discovery.
const LLM_DESCRIBE_REQUEST_TOPIC: &str = "llm.v1.request.describe";
const LLM_DESCRIBE_RESPONSE_TOPIC: &str = "llm.v1.response.describe";

/// Time the discovery routine waits for provider capsules to respond
/// after the describe request is published. Provider capsules typically
/// reply synchronously inside their interceptor, but we still give the
/// bus a generous window to settle.
const DISCOVERY_TIMEOUT_MS: u64 = 500;

/// The persisted registry state.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct RegistryState {
    providers: Vec<ProviderEntry>,
    active_model_id: Option<String>,
}

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
    // Drain whatever arrives in the window. `recv` blocks up to the
    // remaining timeout; once we have nothing to read we bail. A single
    // bounded recv would only see the first responder — we keep polling
    // until the window closes (signalled by a Timeout error).
    let mut remaining = DISCOVERY_TIMEOUT_MS;
    loop {
        let step = remaining.min(100);
        match response_sub.recv(step) {
            Ok(result) => {
                for msg in &result.messages {
                    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&msg.payload)
                    else {
                        continue;
                    };
                    let Some(arr) = payload.get("providers").and_then(|p| p.as_array()) else {
                        continue;
                    };
                    for entry in arr {
                        if let Ok(p) = serde_json::from_value::<ProviderEntry>(entry.clone()) {
                            providers.push(p);
                        }
                    }
                }
            }
            Err(_) => {
                // Timeout (or other host error) — assume no more arrivals.
                break;
            }
        }
        remaining = remaining.saturating_sub(step);
        if remaining == 0 {
            break;
        }
    }

    providers
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

    // Prune a stale active id and auto-select when exactly one provider exists.
    // Both take `&mut state` and mutate it in place (persisting only when they
    // actually change something — `auto_select_if_single` is a no-op when a
    // model is already selected or multiple providers exist), so the local copy
    // is already current and no reload is needed.
    clear_stale_active_model(&mut state);
    auto_select_if_single(&mut state);

    let active = state
        .active_model_id
        .as_ref()
        .and_then(|id| state.providers.iter().find(|p| &p.id == id));

    let _ = ipc::publish_json("registry.v1.response.get_active_model", &active);
}

/// Handle a `registry.v1.set_active_model` request.
fn handle_set_active_model(payload: &serde_json::Value) {
    let model_id = match payload
        .get("data")
        .and_then(|d| d.get("model_id"))
        .and_then(|v| v.as_str())
        .or_else(|| payload.get("model_id").and_then(|v| v.as_str()))
    {
        Some(id) => id.to_string(),
        None => {
            let _ = ipc::publish_json(
                "registry.v1.response.set_active_model",
                &serde_json::json!({"error": "missing model_id"}),
            );
            return;
        }
    };

    set_active_model_by_id(&model_id);
}

/// Set the active model by ID (extracted helper for reuse).
fn set_active_model_by_id(model_id: &str) {
    let mut state = load_state();

    if state.providers.is_empty() {
        state.providers = discover_providers();
    }

    if let Some(provider) = state.providers.iter().find(|p| p.id == model_id).cloned() {
        state.active_model_id = Some(model_id.to_string());
        save_state(&state);
        publish_model_changed(&provider);
        let _ = ipc::publish_json(
            "registry.v1.response.set_active_model",
            &serde_json::json!({"status": "ok", "active_model": provider}),
        );
    } else {
        let _ = ipc::publish_json(
            "registry.v1.response.set_active_model",
            &serde_json::json!({"error": format!("unknown model: {model_id}")}),
        );
    }
}

/// Clear `active_model_id` if it no longer resolves to a known provider.
fn clear_stale_active_model(state: &mut RegistryState) {
    if let Some(ref id) = state.active_model_id
        && !state.providers.iter().any(|p| &p.id == id)
    {
        log::info(format!(
            "Active model '{id}' no longer available after reload, clearing"
        ));
        state.active_model_id = None;
        save_state(state);
    }
}

/// Auto-select the sole provider if exactly one is available.
fn auto_select_if_single(state: &mut RegistryState) {
    if state.providers.len() == 1 && state.active_model_id.is_none() {
        let provider = state.providers[0].clone();
        state.active_model_id = Some(provider.id.clone());
        save_state(state);
        publish_model_changed(&provider);
        log::info(format!("Auto-selected sole LLM provider: {}", provider.id));
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
                set_active_model_by_id(parts[1]);
            } else {
                emit_model_selection();
            }
        }
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
            set_active_model_by_id(model_id);
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
        clear_stale_active_model(&mut state);
        auto_select_if_single(&mut state);

        // Event loop — blocks on the primary subscription, then drains auxiliary.
        // All four subscriptions are RAII (`Subscription`); their `Drop`
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
                    clear_stale_active_model(&mut state);
                    auto_select_if_single(&mut state);
                }
            }
        }

        Ok(())
    }
}
