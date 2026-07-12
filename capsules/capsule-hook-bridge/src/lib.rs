#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]
#![warn(missing_docs)]

//! Hook Bridge capsule — maps lifecycle events to semantic hooks.
//!
//! The kernel dispatches lifecycle events (e.g. `tool_call_started`,
//! `session_created`) to this capsule via interceptors. The Hook Bridge
//! maps each event to a semantic hook name, fans out to subscriber
//! capsules over IPC, and applies merge strategies to the collected
//! responses.
//!
//! # Architecture (post per-domain WIT split)
//!
//! ```text
//! Kernel EventBus → EventDispatcher → Hook Bridge (this capsule)
//!                                        ↓ ipc::publish("hook.v1.event.<hook>", req)
//!                                     Subscriber capsules A, B, C...
//!                                        ↓ publish "hook.v1.response.<hook>.<corr_id>"
//!                                     Hook Bridge collects responses, applies merge
//!                                        ↓ returns merged result via interceptor reply
//! ```
//!
//! Before the per-domain WIT split, fan-out was driven by the
//! `sys::trigger-hook` host fn (removed in `sdk-rust` 0.7). The kernel
//! iterated `CapsuleRegistry` itself and returned a concatenated list
//! of responses. Post-split, fan-out is a capsule-to-capsule IPC
//! convention: the Hook Bridge publishes a request, listens on a
//! correlation-keyed reply topic, and applies the merge.
//!
//! This is a **policy** capsule: it defines which lifecycle events map
//! to which hook names and how responses are merged.

use astrid_sdk::contracts::hook::HookEventRequest;
use astrid_sdk::prelude::*;
use serde::Serialize;

/// Hard deadline for collecting hook responses, per dispatch.
///
/// The bus capped `request_response` at 60 s; we use a much shorter
/// window because interceptors block the lifecycle event chain and any
/// hook handler that takes >1 s is misbehaving. If no responses arrive
/// in this window, we return the merge of whatever did arrive (which
/// may be the `MergeSemantics::None` result).
const HOOK_COLLECT_DEADLINE_MS: u64 = 5_000;

/// Merged result from hook fan-out.
///
/// Uses `serde_json::Value` for `data` (not `String`) to preserve the
/// wire format: consumers expect `data` as a nested JSON object, not a
/// JSON-encoded string. The WIT contract describes this as
/// `option<string>` for schema purposes, but the Rust type must match
/// what goes on the wire.
#[derive(Serialize)]
pub struct HookResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    skip: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
}

// ── Merge Semantics ────────────────────────────────────────────────────────────────

/// How interceptor responses are merged for a hook.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MergeSemantics {
    /// Fire-and-forget: responses are discarded.
    None,
    /// `before_tool_call` specific: any `skip: true` → skip,
    /// last non-null `modified_params` wins.
    ToolCallBefore,
    /// Last non-null value for the named field wins.
    LastNonNull { field: &'static str },
}

/// A mapping from a lifecycle event to a hook name and merge strategy.
struct HookMapping {
    hook_name: &'static str,
    merge: MergeSemantics,
}

// ── Hook Trigger Protocol ──────────────────────────────────────────────────────────

// The event published on `hook.v1.event.<hook_name>` is the canonical
// `astrid:hook/hook-event-request` shape — `astrid_sdk::contracts::hook::HookEventRequest`,
// shared with sage (the other producer) and the SDK consumer side. Per the
// WIT, `payload` is the lifecycle event serialized as a JSON STRING (not an
// inline value), so a subscriber using the canonical type / the SDK's
// `HookEvent::payload::<T>()` can deserialize it. Subscribers reply on
// `hook.v1.response.<hook_name>.<correlation_id>` when a correlation id is
// present; absent => fire-and-forget.

// ── Event-to-Hook Mapping Table ────────────────────────────────────────────────────

/// Resolve the hook mapping for a given event type string.
///
/// Returns `None` for events that have no corresponding hook.
fn mapping_for_event(event_type: &str) -> Option<HookMapping> {
    match event_type {
        // Session lifecycle
        "astrid.v1.lifecycle.session_created" => Some(HookMapping {
            hook_name: "session_start",
            merge: MergeSemantics::None,
        }),
        "astrid.v1.lifecycle.session_ended" => Some(HookMapping {
            hook_name: "session_end",
            merge: MergeSemantics::None,
        }),

        // Tool hooks
        "astrid.v1.lifecycle.tool_call_started" => Some(HookMapping {
            hook_name: "before_tool_call",
            merge: MergeSemantics::ToolCallBefore,
        }),
        "astrid.v1.lifecycle.tool_call_completed" => Some(HookMapping {
            hook_name: "after_tool_call",
            merge: MergeSemantics::LastNonNull {
                field: "modified_result",
            },
        }),
        "astrid.v1.lifecycle.tool_result_persisting" => Some(HookMapping {
            hook_name: "tool_result_persist",
            merge: MergeSemantics::LastNonNull {
                field: "transformed_result",
            },
        }),

        // Message hooks
        "astrid.v1.lifecycle.message_received" => Some(HookMapping {
            hook_name: "message_received",
            merge: MergeSemantics::None,
        }),
        "astrid.v1.lifecycle.message_sending" => Some(HookMapping {
            hook_name: "message_sending",
            merge: MergeSemantics::LastNonNull {
                field: "modified_content",
            },
        }),
        "astrid.v1.lifecycle.message_sent" => Some(HookMapping {
            hook_name: "message_sent",
            merge: MergeSemantics::None,
        }),

        // Sub-agent hooks
        "astrid.v1.lifecycle.sub_agent_spawned" => Some(HookMapping {
            hook_name: "subagent_start",
            merge: MergeSemantics::None,
        }),
        "astrid.v1.lifecycle.sub_agent_completed"
        | "astrid.v1.lifecycle.sub_agent_failed"
        | "astrid.v1.lifecycle.sub_agent_cancelled" => Some(HookMapping {
            hook_name: "subagent_stop",
            merge: MergeSemantics::None,
        }),

        // Context compaction (broadcast-only observation hooks)
        "astrid.v1.lifecycle.context_compaction_started" => Some(HookMapping {
            hook_name: "on_compaction_started",
            merge: MergeSemantics::None,
        }),
        "astrid.v1.lifecycle.context_compaction_completed" => Some(HookMapping {
            hook_name: "on_compaction_completed",
            merge: MergeSemantics::None,
        }),

        // Kernel lifecycle
        "astrid.v1.lifecycle.kernel_started" => Some(HookMapping {
            hook_name: "kernel_start",
            merge: MergeSemantics::None,
        }),
        "astrid.v1.lifecycle.kernel_shutdown" => Some(HookMapping {
            hook_name: "kernel_stop",
            merge: MergeSemantics::None,
        }),

        _ => Option::None,
    }
}

// ── Merge Logic ────────────────────────────────────────────────────────────────────

/// Apply merge semantics to a list of subscriber responses.
fn apply_merge(merge: &MergeSemantics, responses: &[serde_json::Value]) -> HookResult {
    match merge {
        MergeSemantics::None => HookResult {
            skip: Option::None,
            data: Option::None,
        },

        MergeSemantics::ToolCallBefore => {
            let mut skip = false;
            let mut last_params: Option<serde_json::Value> = Option::None;

            for resp in responses {
                // Any response with skip: true wins
                if resp.get("skip").and_then(|v| v.as_bool()).unwrap_or(false) {
                    skip = true;
                }
                // Last non-null modified_params wins
                if let Some(params) = resp.get("modified_params")
                    && !params.is_null()
                {
                    last_params = Some(params.clone());
                }
            }

            HookResult {
                skip: if skip { Some(true) } else { Option::None },
                data: last_params,
            }
        }

        MergeSemantics::LastNonNull { field } => {
            let mut last_value: Option<serde_json::Value> = Option::None;

            for resp in responses {
                if let Some(val) = resp.get(*field)
                    && !val.is_null()
                {
                    last_value = Some(val.clone());
                }
            }

            HookResult {
                skip: Option::None,
                data: last_value,
            }
        }
    }
}

// ── Correlation IDs ────────────────────────────────────────────────────────────────

/// Generate a 16-byte hex correlation id from the host CSPRNG.
///
/// We can't depend on `uuid` directly (not re-exported from
/// `astrid-sdk`), and using a monotonic timestamp would collide if two
/// dispatches landed in the same nanosecond. `runtime::random_bytes` is
/// the documented CSPRNG path.
fn correlation_id() -> Result<String, SysError> {
    let bytes = runtime::random_bytes(16)?;
    let mut s = String::with_capacity(32);
    for b in bytes {
        s.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('0'));
        s.push(char::from_digit(u32::from(b & 0x0F), 16).unwrap_or('0'));
    }
    Ok(s)
}

// ── Core Dispatch ──────────────────────────────────────────────────────────────────

/// Dispatch a lifecycle event through the hook system.
///
/// 1. Look up the event-to-hook mapping.
/// 2. For `MergeSemantics::None`: publish on
///    `hook.v1.event.<hook_name>` fire-and-forget.
/// 3. For merge cases: subscribe to a correlation-keyed reply topic,
///    publish the event with the correlation id, collect responses
///    until quiescence or deadline, apply the merge.
fn dispatch_hook(
    event_type: &str,
    payload: &serde_json::Value,
) -> Result<Option<HookResult>, SysError> {
    let Some(mapping) = mapping_for_event(event_type) else {
        // No hook mapping for this event — nothing to do.
        return Ok(Option::None);
    };

    let event_topic = format!("hook.v1.event.{}", mapping.hook_name);

    // Fire-and-forget for None-merge events. No responder is expected,
    // so we don't allocate a subscription handle.
    if matches!(mapping.merge, MergeSemantics::None) {
        let request = HookEventRequest {
            hook: mapping.hook_name.to_string(),
            payload: serde_json::to_string(payload)?,
            correlation_id: Option::None,
        };
        ipc::publish_json(&event_topic, &request)?;
        return Ok(Option::None);
    }

    // Fan-out + collect. Subscribe BEFORE publishing so a fast responder
    // can't beat us to the reply topic.
    let corr_id = correlation_id()?;
    let reply_topic = format!("hook.v1.response.{}.{corr_id}", mapping.hook_name);

    let sub = ipc::subscribe(&reply_topic)?;

    let request = HookEventRequest {
        hook: mapping.hook_name.to_string(),
        payload: serde_json::to_string(payload)?,
        correlation_id: Some(corr_id),
    };
    ipc::publish_json(&event_topic, &request)?;

    // Drain replies until the collection window closes, or `recv`
    // returns an empty batch (quiescence). The Drop on `sub` releases
    // the kernel-side subscription on every return path.
    let mut responses: Vec<serde_json::Value> = Vec::new();
    let start = time::monotonic();
    loop {
        let elapsed_ms = u64::try_from((time::monotonic().saturating_sub(start)).as_millis())
            .unwrap_or(HOOK_COLLECT_DEADLINE_MS);
        if elapsed_ms >= HOOK_COLLECT_DEADLINE_MS {
            break;
        }
        let remaining = HOOK_COLLECT_DEADLINE_MS - elapsed_ms;

        match sub.recv(remaining) {
            Ok(poll) => {
                if poll.messages.is_empty() {
                    // Either the host returned early-empty or we hit the
                    // deadline without new messages. Either way, stop.
                    break;
                }
                for msg in poll.messages {
                    match serde_json::from_str::<serde_json::Value>(&msg.payload) {
                        Ok(v) => responses.push(v),
                        Err(e) => {
                            log::warn(format!(
                                "hook-bridge: dropping malformed reply on {reply_topic}: {e}"
                            ));
                        }
                    }
                }
            }
            Err(SysError::HostError(msg)) if msg.contains("Timeout") => {
                // No more replies inside the window. Done.
                break;
            }
            Err(e) => return Err(e),
        }
    }

    Ok(Some(apply_merge(&mapping.merge, &responses)))
}

// ── Capsule Implementation ─────────────────────────────────────────────────────────

/// Hook Bridge capsule.
///
/// Maps lifecycle events to semantic hooks, fans out to subscribers via
/// IPC, and applies merge strategies to the responses.
#[derive(Default)]
pub struct HookBridge;

/// Extract event type and dispatch the hook. Used by all interceptor handlers.
fn handle_lifecycle(
    event_type: &str,
    payload: serde_json::Value,
) -> Result<Option<HookResult>, SysError> {
    dispatch_hook(event_type, &payload)
}

#[capsule]
impl HookBridge {
    // ── Session lifecycle ──

    /// Handle `session_created` lifecycle event.
    #[astrid::interceptor("on_session_created")]
    pub fn on_session_created(&self, payload: serde_json::Value) -> Result<(), SysError> {
        let _ = handle_lifecycle("astrid.v1.lifecycle.session_created", payload)?;
        Ok(())
    }

    /// Handle `session_ended` lifecycle event.
    #[astrid::interceptor("on_session_ended")]
    pub fn on_session_ended(&self, payload: serde_json::Value) -> Result<(), SysError> {
        let _ = handle_lifecycle("astrid.v1.lifecycle.session_ended", payload)?;
        Ok(())
    }

    // ── Tool hooks ──

    /// Handle `tool_call_started` — maps to `before_tool_call` hook.
    ///
    /// Returns merged result with potential skip/modified_params.
    #[astrid::interceptor("on_tool_call_started")]
    pub fn on_tool_call_started(
        &self,
        payload: serde_json::Value,
    ) -> Result<Option<HookResult>, SysError> {
        handle_lifecycle("astrid.v1.lifecycle.tool_call_started", payload)
    }

    /// Handle `tool_call_completed` — maps to `after_tool_call` hook.
    #[astrid::interceptor("on_tool_call_completed")]
    pub fn on_tool_call_completed(
        &self,
        payload: serde_json::Value,
    ) -> Result<Option<HookResult>, SysError> {
        handle_lifecycle("astrid.v1.lifecycle.tool_call_completed", payload)
    }

    /// Handle `tool_result_persisting` — maps to `tool_result_persist` hook.
    #[astrid::interceptor("on_tool_result_persisting")]
    pub fn on_tool_result_persisting(
        &self,
        payload: serde_json::Value,
    ) -> Result<Option<HookResult>, SysError> {
        handle_lifecycle("astrid.v1.lifecycle.tool_result_persisting", payload)
    }

    // ── Message hooks ──

    /// Handle `message_received` lifecycle event.
    #[astrid::interceptor("on_message_received")]
    pub fn on_message_received(&self, payload: serde_json::Value) -> Result<(), SysError> {
        let _ = handle_lifecycle("astrid.v1.lifecycle.message_received", payload)?;
        Ok(())
    }

    /// Handle `message_sending` — maps to `message_sending` hook.
    #[astrid::interceptor("on_message_sending")]
    pub fn on_message_sending(
        &self,
        payload: serde_json::Value,
    ) -> Result<Option<HookResult>, SysError> {
        handle_lifecycle("astrid.v1.lifecycle.message_sending", payload)
    }

    /// Handle `message_sent` lifecycle event.
    #[astrid::interceptor("on_message_sent")]
    pub fn on_message_sent(&self, payload: serde_json::Value) -> Result<(), SysError> {
        let _ = handle_lifecycle("astrid.v1.lifecycle.message_sent", payload)?;
        Ok(())
    }

    // ── Sub-agent hooks ──

    /// Handle `sub_agent_spawned` lifecycle event.
    #[astrid::interceptor("on_subagent_spawned")]
    pub fn on_subagent_spawned(&self, payload: serde_json::Value) -> Result<(), SysError> {
        let _ = handle_lifecycle("astrid.v1.lifecycle.sub_agent_spawned", payload)?;
        Ok(())
    }

    /// Handle `sub_agent_completed` lifecycle event.
    #[astrid::interceptor("on_subagent_completed")]
    pub fn on_subagent_completed(&self, payload: serde_json::Value) -> Result<(), SysError> {
        let _ = handle_lifecycle("astrid.v1.lifecycle.sub_agent_completed", payload)?;
        Ok(())
    }

    /// Handle `sub_agent_failed` lifecycle event.
    #[astrid::interceptor("on_subagent_failed")]
    pub fn on_subagent_failed(&self, payload: serde_json::Value) -> Result<(), SysError> {
        let _ = handle_lifecycle("astrid.v1.lifecycle.sub_agent_failed", payload)?;
        Ok(())
    }

    /// Handle `sub_agent_cancelled` lifecycle event.
    #[astrid::interceptor("on_subagent_cancelled")]
    pub fn on_subagent_cancelled(&self, payload: serde_json::Value) -> Result<(), SysError> {
        let _ = handle_lifecycle("astrid.v1.lifecycle.sub_agent_cancelled", payload)?;
        Ok(())
    }

    // ── Context compaction ──

    /// Handle `context_compaction_started` lifecycle event.
    #[astrid::interceptor("on_compaction_started")]
    pub fn on_compaction_started(&self, payload: serde_json::Value) -> Result<(), SysError> {
        let _ = handle_lifecycle("astrid.v1.lifecycle.context_compaction_started", payload)?;
        Ok(())
    }

    /// Handle `context_compaction_completed` lifecycle event.
    #[astrid::interceptor("on_compaction_completed")]
    pub fn on_compaction_completed(&self, payload: serde_json::Value) -> Result<(), SysError> {
        let _ = handle_lifecycle("astrid.v1.lifecycle.context_compaction_completed", payload)?;
        Ok(())
    }

    // ── Kernel lifecycle ──

    /// Handle `kernel_started` lifecycle event.
    #[astrid::interceptor("on_kernel_started")]
    pub fn on_kernel_started(&self, payload: serde_json::Value) -> Result<(), SysError> {
        let _ = handle_lifecycle("astrid.v1.lifecycle.kernel_started", payload)?;
        Ok(())
    }

    /// Handle `kernel_shutdown` lifecycle event.
    #[astrid::interceptor("on_kernel_shutdown")]
    pub fn on_kernel_shutdown(&self, payload: serde_json::Value) -> Result<(), SysError> {
        let _ = handle_lifecycle("astrid.v1.lifecycle.kernel_shutdown", payload)?;
        Ok(())
    }
}
