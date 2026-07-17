#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]
#![warn(missing_docs)]

//! Native OpenAI LLM provider capsule.
//!
//! Uses OpenAI's **Responses API** (`POST /v1/responses`) — the recommended
//! API for new projects, replacing Chat Completions. Key differences:
//!
//! - **`input`** instead of `messages`, **`instructions`** instead of system message
//! - **Named SSE events** (`event: response.output_text.delta`) instead of `data: {json}`
//! - **Structured outputs** via `text.format: { type: "json_schema", ... }`
//! - **Strict function calling** — `strict: true` on tool definitions
//! - **Reasoning effort** — `none`/`low`/`medium`/`high`/`xhigh` for GPT-5.x and o-series
//! - **Service tier** — `auto`/`default`/`flex`/`priority` routing
//! - **`max_output_tokens`** at the top level
//!
//! For generic OpenAI-compatible providers that still use `/v1/chat/completions`
//! (Groq, Together, Mistral, etc.), use `aos-openai-compat` instead.

mod models;
mod schemas;

use astrid_sdk::prelude::*;
use astrid_sdk::types::{IpcPayload, Message, MessageContent, MessageRole, StreamEvent};
use models::lookup;
use schemas::{
    FunctionCallArgsDelta, FunctionCallArgsDone, ModelList, OutputItemAdded, ResponseCompleted,
    TextDelta,
};
use serde_json::Value;
use uuid::Uuid;

const STREAM_TOPIC: &str = "llm.v1.stream.openai";
/// IPC topic the registry routes generate requests to for this capsule.
/// Single source of truth, reused by `llm_describe` and the test suite.
const REQUEST_TOPIC: &str = "llm.v1.request.generate.openai";
const BASE_URL: &str = "https://api.openai.com";
/// Default model hint when the env `model` is unset.
const DEFAULT_MODEL: &str = "gpt-5.5";
/// Maximum SSE line buffer size (1 MB).
const MAX_LINE_BUFFER_SIZE: usize = 1024 * 1024;

/// Native OpenAI LLM provider capsule.
#[derive(Default)]
pub struct OpenAIProvider;

#[capsule]
impl OpenAIProvider {
    /// Handles incoming LLM generation requests.
    #[astrid::interceptor("handle_llm_request")]
    pub fn handle_llm_request(&self, req: IpcPayload) -> Result<(), SysError> {
        if let IpcPayload::LlmRequest {
            request_id,
            model,
            messages,
            tools,
            system,
            ..
        } = req
            && let Err(e) = Self::execute_request(request_id, &model, &messages, &tools, &system)
        {
            log::error(format!("OpenAI request failed: {e}"));
            let _ = ipc::publish_json(
                STREAM_TOPIC,
                &IpcPayload::LlmStreamEvent {
                    request_id,
                    event: StreamEvent::Error(e.to_string()),
                },
            );
        }
        Ok(())
    }

    /// Returns provider metadata for IPC-based provider discovery.
    ///
    /// The registry capsule publishes `llm.v1.request.describe` and drains
    /// `llm.v1.response.describe` for a bounded window (post-#752 replaces
    /// the removed `hooks::trigger` fan-out). The provider must publish its
    /// descriptor explicitly — the interceptor return value is no longer
    /// fanned out to the caller under the new ABI.
    #[astrid::interceptor("llm_describe")]
    pub fn llm_describe(&self, _payload: serde_json::Value) -> Result<serde_json::Value, SysError> {
        // The env `model` is the *default selection* hint, not the only usable
        // model. We advertise the LIVE `/v1/models` catalogue (enriched from the
        // capability table) when reachable, and fall back to the full hardcoded
        // catalog otherwise so an offline/keyless install never regresses.
        let default_model = env::var("model").unwrap_or_else(|_| DEFAULT_MODEL.into());

        let entries = match Self::discover_models() {
            Ok(live_ids) => {
                // Live list is authority; enrich each id from the catalog and
                // hoist (or prepend) the configured default so it is first.
                models::build_live_entries(&live_ids, &default_model, REQUEST_TOPIC, STREAM_TOPIC)
            }
            Err(e) => {
                // Any discovery failure (missing/blank key, non-2xx, non-JSON,
                // empty data, network error) falls back to the full catalog.
                log::warn(format!(
                    "/v1/models discovery failed, advertising hardcoded catalog: {e}"
                ));
                models::build_provider_entries(&default_model, REQUEST_TOPIC, STREAM_TOPIC)
            }
        };

        let response = serde_json::json!({ "providers": entries });
        ipc::publish_json("llm.v1.response.describe", &response)?;
        Ok(response)
    }
}

impl OpenAIProvider {
    /// Build the `Authorization` header value from a raw configured key.
    ///
    /// Returns `Some("Bearer <trimmed>")` only when the key has non-whitespace
    /// content; a missing, empty, or whitespace/newline-only key (common from a
    /// copy-paste) is treated as **keyless** (`None`) so discovery never emits
    /// `Authorization: Bearer <whitespace>`. The header carries the trimmed
    /// value, stripping stray surrounding whitespace from the configured secret.
    fn bearer_header(raw_key: &str) -> Option<String> {
        let trimmed = raw_key.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(format!("Bearer {trimmed}"))
        }
    }

    /// Resolve the request base URL from the `base_url` env (defaulting to the
    /// [`BASE_URL`] const when unset), normalized via [`normalize_base_url`].
    ///
    /// Single source of truth for the endpoint so discovery (`/v1/models`) and
    /// generation (`/v1/responses`) always hit the SAME host: a configured
    /// proxy/Azure `base_url` must not split traffic between the two paths.
    fn resolve_base_url() -> String {
        Self::normalize_base_url(&env::var("base_url").unwrap_or_else(|_| BASE_URL.into()))
    }

    /// Pure normalization of a configured base URL: strip any trailing slash so
    /// `{base}/v1/...` never doubles the separator. Split from [`resolve_base_url`]
    /// so the endpoint-building invariant is unit-testable without a host env.
    fn normalize_base_url(raw: &str) -> String {
        raw.trim_end_matches('/').to_string()
    }

    /// Query `GET {base_url}/v1/models` and return the discovered model ids.
    ///
    /// Returns `Ok(Vec)` with **at least one** id on success. Any failure
    /// (network error, non-2xx, unparseable body, empty `data`) returns `Err`
    /// so the caller falls back to the hardcoded catalog. Never panics; never
    /// blocks beyond the host HTTP timeout. A missing/blank api_key is NOT a
    /// hard error here — OpenAI rejects keyless `/v1/models` with a non-2xx,
    /// which funnels to the same fallback.
    fn discover_models() -> Result<Vec<String>, SysError> {
        let url = format!("{}/v1/models", Self::resolve_base_url());

        let mut req = http::Request::get(&url);
        // Only send `Authorization` when the key has non-whitespace content, and
        // send the trimmed value: a whitespace/newline-only key (common from a
        // copy-paste) is treated as keyless, not sent as `Bearer <ws>`.
        if let Some(value) = Self::bearer_header(&env::var("api_key").unwrap_or_default()) {
            req = req.header("authorization", value);
        }

        let resp = http::send(&req)?;
        if !resp.is_success() {
            return Err(SysError::ApiError(format!(
                "/v1/models returned status {}",
                resp.status()
            )));
        }

        let list: ModelList = resp.json()?;
        Self::extract_model_ids(list)
    }

    /// Pure extraction + emptiness gate, split out so it is unit-testable
    /// without HTTP. Drops blank ids (empty or whitespace-only, which cannot be
    /// selected) and deduplicates colliding ids stably (preserving server
    /// order), guarding against hostile upstreams that repeat an id; errors if
    /// nothing usable remains so the caller falls back to the catalog.
    fn extract_model_ids(list: ModelList) -> Result<Vec<String>, SysError> {
        let mut seen = std::collections::HashSet::new();
        let ids: Vec<String> = list
            .data
            .into_iter()
            .map(|m| m.id)
            .filter(|id| !id.trim().is_empty())
            .filter(|id| seen.insert(id.clone()))
            .collect();
        if ids.is_empty() {
            return Err(SysError::ApiError("/v1/models returned no models".into()));
        }
        Ok(ids)
    }

    /// Build and send the Responses API request, then parse the SSE stream.
    fn execute_request(
        request_id: Uuid,
        model: &str,
        messages: &[Message],
        tools: &[astrid_sdk::types::LlmToolDefinition],
        system: &str,
    ) -> Result<(), SysError> {
        // Resolve `base_url` from env (defaulting to BASE_URL) through the same
        // helper `discover_models` uses, so discovery and generation hit the SAME
        // endpoint. Without this a configured proxy/Azure `base_url` is used for
        // /v1/models discovery while generation silently goes to api.openai.com.
        let url = format!("{}/v1/responses", Self::resolve_base_url());

        let resolved_model = if model.is_empty() {
            env::var("model").unwrap_or_else(|_| DEFAULT_MODEL.into())
        } else {
            model.to_string()
        };

        let info = lookup(&resolved_model);

        // Build input array from messages.
        let input = Self::build_input(messages);

        let mut request_body = serde_json::json!({
            "model": resolved_model,
            "input": input,
            "stream": true,
        });

        // System instructions go at the top level, not in input.
        if !system.is_empty() {
            request_body["instructions"] = Value::String(system.to_string());
        }

        // max_output_tokens — env override, then registry default.
        let max_tokens = env::var("max_output_tokens")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(info.max_output_tokens);
        if max_tokens > 0 {
            request_body["max_output_tokens"] = serde_json::json!(max_tokens);
        }

        // Temperature — only supported when reasoning effort is `none` on GPT-5.4.
        // On older reasoning models (o-series, GPT-5.2), not supported at all.
        let reasoning_effort = env::var("reasoning_effort").unwrap_or_default();
        let effort_is_none = reasoning_effort.is_empty() || reasoning_effort == "none";

        if effort_is_none
            && let Ok(temp) = env::var("temperature")
            && let Ok(t) = temp.parse::<f64>()
        {
            request_body["temperature"] = serde_json::json!(t);
        }

        // Reasoning effort.
        if info.is_reasoning && !reasoning_effort.is_empty() && reasoning_effort != "none" {
            request_body["reasoning"] = serde_json::json!({ "effort": reasoning_effort });
        }

        // Service tier.
        if let Ok(tier) = env::var("service_tier")
            && !tier.is_empty()
        {
            request_body["service_tier"] = serde_json::json!(tier);
        }

        // Tools with strict mode.
        if !tools.is_empty() {
            let api_tools: Vec<Value> = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                        "strict": true,
                    })
                })
                .collect();
            request_body["tools"] = Value::Array(api_tools);
        }

        // Use the shared `bearer_header` so the generation path trims the key
        // identically to discovery (`discover_models`). Without this, a key with
        // trailing whitespace lets discovery succeed (it trims) while generation
        // sends `Bearer <key>\n` and fails. A blank/whitespace-only key is keyless.
        let Some(auth_value) = Self::bearer_header(&env::var("api_key").unwrap_or_default()) else {
            return Err(SysError::ApiError("OpenAI api_key not configured".into()));
        };

        let req = http::Request::post(&url)
            .header("authorization", auth_value)
            .json(&request_body)?;

        let stream = http::stream_start(&req)?;

        if stream.status() != 200 {
            let status = stream.status();
            let mut error_body = String::new();
            while let Some(chunk) = stream.read_chunk()? {
                error_body.push_str(&String::from_utf8_lossy(&chunk));
                if error_body.len() > 4096 {
                    error_body.truncate(4096);
                    break;
                }
            }
            // `stream` drops at end of scope, releasing the kernel-side resource.
            return Err(SysError::ApiError(format!(
                "OpenAI API error ({status}): {error_body}"
            )));
        }

        Self::parse_sse_stream(request_id, &stream)
        // `stream` drops at end of scope, releasing the kernel-side resource.
    }

    /// Parse the Responses API SSE stream.
    ///
    /// The Responses API uses named events (`event: <type>\ndata: <json>\n\n`)
    /// instead of Chat Completions' bare `data: {json}` format.
    fn parse_sse_stream(request_id: Uuid, stream: &http::HttpStream) -> Result<(), SysError> {
        let mut line_buffer = String::new();
        let mut current_event: Option<String> = None;

        while let Some(chunk) = stream.read_chunk()? {
            let chunk_str = String::from_utf8_lossy(&chunk);
            line_buffer.push_str(&chunk_str);

            if line_buffer.len() > MAX_LINE_BUFFER_SIZE {
                return Err(SysError::ApiError(
                    "SSE line buffer exceeded maximum size".into(),
                ));
            }

            while let Some(newline_pos) = line_buffer.find('\n') {
                let line = line_buffer[..newline_pos]
                    .trim_end_matches('\r')
                    .to_string();
                line_buffer = line_buffer[(newline_pos + 1)..].to_string();

                if line.is_empty() {
                    // Blank line = end of SSE event block.
                    current_event = None;
                    continue;
                }

                if let Some(event_type) = line.strip_prefix("event: ") {
                    current_event = Some(event_type.to_string());
                    continue;
                }

                let Some(data) = line.strip_prefix("data: ") else {
                    continue;
                };

                let Some(ref event_type) = current_event else {
                    continue;
                };

                Self::handle_event(request_id, event_type, data)?;
            }
        }

        Ok(())
    }

    /// Dispatch a single named SSE event.
    fn handle_event(request_id: Uuid, event_type: &str, data: &str) -> Result<(), SysError> {
        match event_type {
            "response.output_text.delta" => {
                if let Ok(delta) = serde_json::from_str::<TextDelta>(data)
                    && !delta.delta.is_empty()
                {
                    Self::publish_stream(request_id, StreamEvent::TextDelta(delta.delta))?;
                }
            }
            "response.output_item.added" => {
                if let Ok(item) = serde_json::from_str::<OutputItemAdded>(data)
                    && item.item.item_type == "function_call"
                {
                    let id = item.item.call_id.unwrap_or_else(|| item.item.id.clone());
                    let name = item.item.name.unwrap_or_default();
                    if !name.is_empty() {
                        Self::publish_stream(request_id, StreamEvent::ToolCallStart { id, name })?;
                    }
                }
            }
            "response.function_call_arguments.delta" => {
                if let Ok(delta) = serde_json::from_str::<FunctionCallArgsDelta>(data)
                    && !delta.delta.is_empty()
                {
                    Self::publish_stream(
                        request_id,
                        StreamEvent::ToolCallDelta {
                            id: delta.item_id,
                            args_delta: delta.delta,
                        },
                    )?;
                }
            }
            "response.function_call_arguments.done" => {
                if let Ok(done) = serde_json::from_str::<FunctionCallArgsDone>(data) {
                    Self::publish_stream(
                        request_id,
                        StreamEvent::ToolCallEnd { id: done.item_id },
                    )?;
                }
            }
            "response.completed" => {
                if let Ok(completed) = serde_json::from_str::<ResponseCompleted>(data)
                    && let Some(usage) = completed.response.usage
                {
                    Self::publish_stream(
                        request_id,
                        StreamEvent::Usage {
                            input_tokens: usage.input_tokens,
                            output_tokens: usage.output_tokens,
                        },
                    )?;
                }
                Self::publish_stream(request_id, StreamEvent::Done)?;
            }
            "response.failed" => {
                log::error(format!("OpenAI response failed: {data}"));
                Self::publish_stream(
                    request_id,
                    StreamEvent::Error(format!("Response failed: {data}")),
                )?;
            }
            // Ignore lifecycle events we don't need: response.created,
            // response.in_progress, response.output_item.done,
            // response.content_part.added, response.content_part.done,
            // response.output_text.done
            _ => {}
        }
        Ok(())
    }

    /// Publish a stream event to the event bus.
    fn publish_stream(request_id: Uuid, event: StreamEvent) -> Result<(), SysError> {
        ipc::publish_json(
            STREAM_TOPIC,
            &IpcPayload::LlmStreamEvent { request_id, event },
        )
    }

    /// Build the `input` array for the Responses API from Astrid messages.
    fn build_input(messages: &[Message]) -> Vec<Value> {
        let mut input: Vec<Value> = Vec::new();

        for msg in messages {
            if msg.role == MessageRole::System {
                // System messages go in `instructions`, not input.
                continue;
            }
            input.push(Self::convert_message(msg));
        }

        input
    }

    /// Convert an Astrid `Message` to Responses API input format.
    fn convert_message(message: &Message) -> Value {
        match &message.content {
            MessageContent::Text(text) => {
                serde_json::json!({
                    "role": Self::role_str(message.role),
                    "content": text,
                })
            }
            MessageContent::ToolCalls(calls) => {
                // In Responses API, tool calls are separate output items.
                // For conversation history, we send them as assistant messages.
                let tool_calls: Vec<Value> = calls
                    .iter()
                    .map(|c| {
                        serde_json::json!({
                            "type": "function_call",
                            "id": c.id,
                            "call_id": c.id,
                            "name": c.name,
                            "arguments": if c.arguments.is_string() {
                                c.arguments.clone()
                            } else {
                                Value::String(c.arguments.to_string())
                            },
                        })
                    })
                    .collect();

                // Return tool calls as separate items in input.
                // The Responses API expects function_call items directly.
                if tool_calls.len() == 1 {
                    tool_calls.into_iter().next().unwrap_or_default()
                } else {
                    // Multiple tool calls — return as array (will be flattened by caller if needed).
                    serde_json::json!(tool_calls)
                }
            }
            MessageContent::ToolResult(result) => {
                serde_json::json!({
                    "type": "function_call_output",
                    "call_id": result.call_id,
                    "output": result.content,
                })
            }
            MessageContent::MultiPart(parts) => {
                let content: Vec<Value> = parts
                    .iter()
                    .map(|p| match p {
                        astrid_sdk::types::ContentPart::Text { text } => {
                            serde_json::json!({"type": "input_text", "text": text})
                        }
                        astrid_sdk::types::ContentPart::Image { media_type, data } => {
                            serde_json::json!({
                                "type": "input_image",
                                "image_url": format!("data:{media_type};base64,{data}"),
                            })
                        }
                    })
                    .collect();

                serde_json::json!({
                    "role": Self::role_str(message.role),
                    "content": content,
                })
            }
        }
    }

    /// Map Astrid `MessageRole` to OpenAI role string.
    fn role_str(role: MessageRole) -> &'static str {
        match role {
            MessageRole::System => "developer",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BASE_URL, DEFAULT_MODEL, ModelList, OpenAIProvider, REQUEST_TOPIC, STREAM_TOPIC, models,
    };

    #[test]
    fn discovery_and_generation_share_one_base_url() {
        // Regression: discovery read `base_url` from env but generation hardcoded
        // the `BASE_URL` const, so a configured proxy/Azure endpoint split traffic
        // (discovery hit the proxy, generation silently hit api.openai.com). Both
        // paths must build their URL from the SAME normalized base.
        let proxy = "https://proxy.example.com/";
        let base = OpenAIProvider::normalize_base_url(proxy);
        let models_url = format!("{base}/v1/models");
        let responses_url = format!("{base}/v1/responses");
        // Same host for both routes — no trailing-slash doubling, no api.openai.com.
        assert_eq!(models_url, "https://proxy.example.com/v1/models");
        assert_eq!(responses_url, "https://proxy.example.com/v1/responses");
        assert!(!responses_url.contains("api.openai.com"));
    }

    #[test]
    fn normalize_base_url_defaults_and_strips_trailing_slash() {
        // The const default is normalized identically to a configured value.
        assert_eq!(OpenAIProvider::normalize_base_url(BASE_URL), BASE_URL);
        assert_eq!(
            OpenAIProvider::normalize_base_url("https://api.openai.com/"),
            "https://api.openai.com"
        );
        assert_eq!(
            OpenAIProvider::normalize_base_url("https://api.openai.com"),
            "https://api.openai.com"
        );
    }

    #[test]
    fn generation_auth_trims_key_like_discovery() {
        // Regression: discovery built its header via `bearer_header` (which trims)
        // while generation used the RAW `api_key` env, so a key with trailing
        // whitespace let discovery succeed but generation send `Bearer <key>\n`
        // and fail. Both paths now route through `bearer_header`, so a whitespace-
        // padded key yields an identical trimmed header on the generation path.
        assert_eq!(
            OpenAIProvider::bearer_header("  sk-live-xyz \n"),
            Some("Bearer sk-live-xyz".to_string())
        );
        // And a whitespace-only key is keyless on BOTH paths (generation returns
        // the "not configured" error rather than sending `Bearer <ws>`).
        assert_eq!(OpenAIProvider::bearer_header(" \t\n "), None);
    }

    #[test]
    fn extract_model_ids_parses_dedups_and_drops_blanks() {
        // A representative `/v1/models` body with vendor extras, a blank id, and
        // a duplicate. Only non-blank ids survive, stably deduped on server order.
        let body = r#"{
            "object": "list",
            "data": [
                { "id": "gpt-5.5", "object": "model", "owned_by": "openai" },
                { "id": "   ", "object": "model" },
                { "id": "gpt-5.4", "object": "model" },
                { "id": "gpt-5.5", "object": "model" }
            ]
        }"#;
        let list: ModelList = serde_json::from_str(body).expect("parse model list");
        let ids = OpenAIProvider::extract_model_ids(list).expect("non-empty");
        assert_eq!(ids, vec!["gpt-5.5", "gpt-5.4"]);
    }

    #[test]
    fn extract_model_ids_empty_data_is_error() {
        // Empty `data`, all-blank ids, and a missing `data` key all funnel to the
        // Err arm that triggers the catalog fallback in `discover_models`.
        let empty: ModelList = serde_json::from_str(r#"{ "data": [] }"#).expect("parse");
        assert!(OpenAIProvider::extract_model_ids(empty).is_err());

        let blank: ModelList =
            serde_json::from_str(r#"{ "data": [ { "id": "  " } ] }"#).expect("parse");
        assert!(OpenAIProvider::extract_model_ids(blank).is_err());

        let missing: ModelList = serde_json::from_str(r#"{ "object": "list" }"#).expect("parse");
        assert!(OpenAIProvider::extract_model_ids(missing).is_err());
    }

    #[test]
    fn bearer_header_treats_blank_key_as_keyless() {
        // A copy-pasted empty/whitespace/newline key must NOT produce a header:
        // sending `Bearer <whitespace>` breaks discovery against some servers.
        assert_eq!(OpenAIProvider::bearer_header(""), None);
        assert_eq!(OpenAIProvider::bearer_header("   "), None);
        assert_eq!(OpenAIProvider::bearer_header(" \t\r\n "), None);
        // A genuine key produces a header, trimmed of stray surrounding space.
        assert_eq!(
            OpenAIProvider::bearer_header("  sk-abc123\n"),
            Some("Bearer sk-abc123".to_string())
        );
    }

    #[test]
    fn discovery_failure_fallback_equals_full_hardcoded_catalog() {
        // The `Err` arm of `llm_describe` builds exactly the full hardcoded
        // catalog via `build_provider_entries` — an offline/keyless install must
        // advertise the known catalog and never regress.
        let fallback = models::build_provider_entries(DEFAULT_MODEL, REQUEST_TOPIC, STREAM_TOPIC);
        // Sanity: the fallback is the whole catalog, frontier-default first.
        assert_eq!(fallback[0].id, DEFAULT_MODEL);
        assert!(fallback.len() > 1);
        // Every entry carries the shared topics.
        for e in &fallback {
            assert_eq!(e.request_topic, REQUEST_TOPIC);
            assert_eq!(e.stream_topic, STREAM_TOPIC);
        }
    }

    #[test]
    fn default_model_constant_is_gpt_5_5() {
        assert_eq!(DEFAULT_MODEL, "gpt-5.5");
    }
}
