#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]

//! LLM Provider Registry capsule.
//!
//! Discovers available LLM providers via IPC hook fan-out and manages
//! model selection. Provider capsules respond to `llm.v1.request.describe`
//! with their capabilities and routing topics, following the same pattern
//! as tool discovery (`tool.v1.request.describe`).
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

use std::sync::atomic::{AtomicU64, Ordering};

use astrid_sdk::prelude::*;

// Use the shared contract type — generated from WIT, serde-enabled.
use astrid_sdk::contracts::registry::ProviderEntry;

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The kernel's system session UUID, used to validate IPC messages from the kernel.
const KERNEL_UUID: &str = "00000000-0000-0000-0000-000000000000";

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

fn save_state(state: &RegistryState) {
    let _ = kv::set_json(STATE_KEY, state);
}

/// Discover LLM providers via IPC hook fan-out.
///
/// Uses `hooks::trigger` with `llm.v1.request.describe` — the kernel fans
/// out to all capsules with matching interceptors and returns a JSON array
/// of responses. Each provider capsule returns `{"providers": [...]}`.
fn discover_providers() -> Vec<ProviderEntry> {
    let request_json = serde_json::json!({
        "hook": "llm.v1.request.describe",
        "payload": {},
    });
    let request_str = match serde_json::to_string(&request_json) {
        Ok(s) => s,
        Err(e) => {
            log::warn(format!(
                "Failed to serialize provider discovery request: {e}"
            ));
            return Vec::new();
        }
    };
    let response_str = match hooks::trigger(&request_str) {
        Ok(s) => s,
        Err(e) => {
            log::warn(format!("Provider discovery hook trigger failed: {e}"));
            return Vec::new();
        }
    };
    let responses: Vec<serde_json::Value> = match serde_json::from_str(&response_str) {
        Ok(r) => r,
        Err(e) => {
            log::warn(format!("Failed to parse provider discovery response: {e}"));
            return Vec::new();
        }
    };

    responses
        .iter()
        .filter_map(|resp| resp.get("providers").and_then(|p| p.as_array()))
        .flatten()
        .filter_map(|p| serde_json::from_value::<ProviderEntry>(p.clone()).ok())
        .collect()
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
fn handle_get_active_model() {
    let state = load_state();
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

    let request_id = format!(
        "models-{}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
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

        let sub = ipc::subscribe("registry.v1.*").map_err(|e| SysError::ApiError(e.to_string()))?;
        let cmd_sub = ipc::subscribe("cli.v1.command.execute")
            .map_err(|e| SysError::ApiError(e.to_string()))?;
        let selection_sub = ipc::subscribe("registry.v1.selection.callback")
            .map_err(|e| SysError::ApiError(e.to_string()))?;
        let capsules_loaded_sub = ipc::subscribe("astrid.v1.capsules_loaded")
            .map_err(|e| SysError::ApiError(e.to_string()))?;

        let _ = runtime::signal_ready();

        // Wait for the kernel to signal that all capsules have been loaded.
        let mut capsules_ready = false;
        if let Ok(result) = ipc::recv(&capsules_loaded_sub, 5000)
            && has_kernel_message(&result)
        {
            capsules_ready = true;
        }

        if !capsules_ready {
            log::warn(
                "Timed out waiting for astrid.v1.capsules_loaded - proceeding with discovery anyway",
            );
        }

        // Now that all capsules are loaded, discover providers via IPC hook.
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
        loop {
            match ipc::recv(&sub, 5000) {
                Ok(result) => dispatch_registry_messages(&result),
                Err(_) => break,
            }

            // Drain CLI command messages (non-blocking).
            if let Ok(result) = ipc::poll(&cmd_sub) {
                dispatch_command_messages(&result);
            }

            // Drain model selection callbacks from the TUI picker.
            if let Ok(result) = ipc::poll(&selection_sub) {
                dispatch_selection_messages(&result);
            }

            // Re-discover providers on capsule reload.
            if let Ok(result) = ipc::poll(&capsules_loaded_sub)
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

        let _ = ipc::unsubscribe(&sub);
        let _ = ipc::unsubscribe(&cmd_sub);
        let _ = ipc::unsubscribe(&selection_sub);
        let _ = ipc::unsubscribe(&capsules_loaded_sub);

        Ok(())
    }
}
