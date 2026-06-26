#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]
#![warn(missing_docs)]

//! OpenAI-compatible LLM provider capsule.
//!
//! Subscribes to `llm.v1.request.generate.openai-compat` IPC events, calls any
//! OpenAI-compatible Chat Completions API via the HTTP airlock, parses the SSE
//! streaming response, and publishes standardized `llm.v1.stream.openai-compat`
//! events back to the event bus.
//!
//! Configure `base_url` to point at any compatible provider:
//! - OpenAI: `https://api.openai.com`
//! - Groq: `https://api.groq.com/openai`
//! - Together: `https://api.together.ai`
//! - Mistral: `https://api.mistral.ai`
//! - DeepSeek: `https://api.deepseek.com`
//! - Fireworks: `https://api.fireworks.ai/inference`

mod schemas;

use astrid_sdk::prelude::*;
use astrid_sdk::types::{IpcPayload, Message, MessageContent, MessageRole, StreamEvent};
use schemas::{ChatCompletionChunk, ModelList};
use serde_json::Value;
use uuid::Uuid;

const STREAM_TOPIC: &str = "llm.v1.stream.openai-compat";
/// IPC topic this provider advertises and the registry routes generation
/// requests to. This is the stable provider route alias, not the package name.
const REQUEST_TOPIC: &str = "llm.v1.request.generate.openai-compat";
const PROVIDER_ALIAS: &str = "openai-compat";
/// Maximum SSE line buffer size (1 MB). If the server sends data without
/// a newline that exceeds this, the stream is aborted.
const MAX_LINE_BUFFER_SIZE: usize = 1024 * 1024;

#[derive(Debug)]
enum SseData {
    Done,
    Chunk(ChatCompletionChunk),
}

/// OpenAI-compatible LLM provider capsule.
#[derive(Default)]
pub struct OpenAICompatProvider;

#[capsule]
impl OpenAICompatProvider {
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
            log::error(format!("LLM request failed: {e}"));
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
    /// The registry capsule publishes a `llm.v1.request.describe` envelope
    /// and drains responses on `llm.v1.response.describe` for a bounded
    /// window. Each provider capsule subscribes to the request topic and
    /// publishes its capability descriptor on the response topic. This
    /// replaces the pre-#752 `hooks::trigger` fan-out path that returned
    /// interceptor results through kernel-mediated dispatch — under the
    /// new ABI the interceptor return value is no longer fanned out, so
    /// the provider must publish explicitly.
    ///
    /// The return value is kept (same shape) so other interceptor callers
    /// continue to see the descriptor; the explicit `ipc::publish_json`
    /// is what registry's new fan-out actually consumes.
    #[astrid::interceptor("llm_describe")]
    pub fn llm_describe(&self, _payload: serde_json::Value) -> Result<serde_json::Value, SysError> {
        // A blank or whitespace-only env `model` is treated as unset: we never
        // advertise a bogus `unknown` id the registry could bind and then send
        // upstream verbatim. The configured default is `None` in that case.
        let default_model = Self::parse_env_model(env::var("model").ok());
        let context_window = env::var("context_window")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(128_000);
        let max_output = env::var("max_output_tokens")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(8_192);

        // Discover the upstream catalogue, folding the discovery result and the
        // configured default into the final id list (see `resolve_model_ids`).
        let discovery = Self::discover_models();
        if let Err(e) = &discovery {
            match &default_model {
                Some(_) => log::warn(format!(
                    "/v1/models discovery failed, using env default: {e}"
                )),
                None => log::warn(format!(
                    "/v1/models discovery failed and no env model configured; \
                     advertising no provider entries: {e}"
                )),
            }
        }
        let model_ids = Self::resolve_model_ids(discovery, default_model.as_deref());

        let default_ref = default_model.as_deref().unwrap_or("");
        let providers =
            Self::describe_providers(&model_ids, default_ref, context_window, max_output);
        let response = serde_json::json!({ "providers": providers });
        ipc::publish_json("llm.v1.response.describe", &response)?;
        Ok(response)
    }
}

impl OpenAICompatProvider {
    /// Normalize the raw env `model` value into a configured default.
    ///
    /// A missing value, or one that is empty or whitespace-only after trimming,
    /// is treated as **unset** (`None`). This is deliberate: a blank env model
    /// must never become a selectable provider-entry id, otherwise the registry
    /// could bind it and we would send a meaningless `model` upstream.
    fn parse_env_model(raw: Option<String>) -> Option<String> {
        raw.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
    }

    /// Fold the `/v1/models` discovery result and the configured default into
    /// the final list of model ids to advertise.
    ///
    /// - Discovery succeeded: use the discovered ids verbatim.
    /// - Discovery failed but a default is configured: fall back to that single
    ///   id so existing pinned installs never regress.
    /// - Discovery failed AND no default is configured: return an empty list, so
    ///   the provider advertises NO entries rather than a bogus `unknown` one
    ///   that the registry could bind and then send upstream.
    fn resolve_model_ids(
        discovery: Result<Vec<String>, SysError>,
        default_model: Option<&str>,
    ) -> Vec<String> {
        match discovery {
            Ok(ids) => ids,
            Err(_) => match default_model {
                Some(model) => vec![model.to_string()],
                None => Vec::new(),
            },
        }
    }

    /// Build the `Authorization` header value from a raw configured key.
    ///
    /// Returns `Some("Bearer <trimmed>")` only when the key has non-whitespace
    /// content; a missing, empty, or whitespace/newline-only key (common from a
    /// copy-paste) is treated as **keyless** (`None`) so we never emit
    /// `Authorization: Bearer <whitespace>`, which permissive/keyless
    /// OpenAI-compatible servers reject. The header carries the trimmed value,
    /// stripping stray surrounding whitespace from the configured secret.
    fn bearer_header(raw_key: &str) -> Option<String> {
        let trimmed = raw_key.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(format!("Bearer {trimmed}"))
        }
    }

    /// Query `GET {base_url}/v1/models` and return the discovered model ids.
    ///
    /// Returns `Ok(Vec)` with **at least one** id on success. Any failure
    /// (network error, non-2xx, unparseable body, empty `data`, server that
    /// does not implement `/v1/models`) returns `Err` so the caller falls back
    /// to the env default. Never panics; never blocks beyond the host HTTP
    /// timeout.
    fn discover_models() -> Result<Vec<String>, SysError> {
        let base_url = env::var("base_url").unwrap_or_else(|_| "https://api.openai.com".into());
        let url = format!("{}/v1/models", base_url.trim_end_matches('/'));

        let mut req = http::Request::get(&url);
        // Only send `Authorization` when the key has non-whitespace content, and
        // send the trimmed value: a whitespace/newline-only key (common from a
        // copy-paste) must be treated as keyless, not sent as `Bearer <ws>`,
        // which breaks discovery against keyless/permissive servers.
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
    /// nothing usable remains so the caller falls back to the env default.
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

    /// Build one provider-entry per model id, with the env-default model emitted
    /// FIRST (`entry[0]`) so the registry can pre-select it positionally. Every
    /// entry shares the same `request_topic`/`stream_topic`. There is NO
    /// `"default"` field — the default is signalled by ORDER. All entries are
    /// plain `serde_json::Value`.
    ///
    /// Ordering rules:
    /// - If `default_model` appears in `model_ids`, its entry is `entry[0]` and
    ///   the remaining ids keep their discovered order after it.
    /// - If `default_model` is NOT in `model_ids` (the upstream catalogue does
    ///   not advertise the configured default), the discovered order is
    ///   preserved unchanged; `entry[0]` is simply the first discovered model.
    ///   The registry still auto-selects `entry[0]`; the operator's configured
    ///   `model` was not offered by the upstream, so the first servable model is
    ///   the best default.
    fn describe_providers(
        model_ids: &[String],
        default_model: &str,
        context_window: u64,
        max_output: u64,
    ) -> Vec<serde_json::Value> {
        // Stable partition: default model first (if present), then the rest in
        // their discovered order.
        let mut ordered: Vec<&String> = Vec::with_capacity(model_ids.len());
        ordered.extend(model_ids.iter().filter(|id| id.as_str() == default_model));
        ordered.extend(model_ids.iter().filter(|id| id.as_str() != default_model));

        ordered
            .iter()
            .map(|id| {
                serde_json::json!({
                    "id": id,
                    "description": format!("OpenAI-compatible model: {id}"),
                    "capabilities": ["text", "vision", "tools"],
                    "request_topic": REQUEST_TOPIC,
                    "stream_topic": STREAM_TOPIC,
                    "context_window": context_window,
                    "max_output_tokens": max_output,
                })
            })
            .collect()
    }

    /// Build and send the HTTP request, then parse the SSE response.
    fn execute_request(
        request_id: Uuid,
        model: &str,
        messages: &[Message],
        tools: &[astrid_sdk::types::LlmToolDefinition],
        system: &str,
    ) -> Result<(), SysError> {
        let base_url = env::var("base_url").unwrap_or_else(|_| "https://api.openai.com".into());
        let url = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));

        let resolved_model = Self::resolve_request_model(model, env::var("model").ok());

        let mut api_messages: Vec<Value> = Vec::new();

        // System message goes first in the messages array.
        if !system.is_empty() {
            api_messages.push(serde_json::json!({
                "role": "system",
                "content": system,
            }));
        }

        for msg in messages {
            if msg.role != MessageRole::System {
                api_messages.push(Self::convert_message(msg));
            }
        }

        let mut request_body = serde_json::json!({
            "model": resolved_model,
            "messages": api_messages,
            "stream": true,
            "stream_options": { "include_usage": true },
        });

        // Apply default generation parameters from env (only if not already
        // specified in the request — env vars are defaults, not overrides).
        let has_max_tokens = request_body.get("max_tokens").is_some_and(|v| !v.is_null());
        if !has_max_tokens
            && let Ok(max_tokens) = env::var("max_output_tokens")
            && let Ok(n) = max_tokens.parse::<u64>()
            && n > 0
        {
            request_body["max_tokens"] = serde_json::json!(n);
        }
        let has_temp = request_body
            .get("temperature")
            .is_some_and(|v| !v.is_null());
        if !has_temp
            && let Ok(temp) = env::var("temperature")
            && let Ok(t) = temp.parse::<f64>()
        {
            request_body["temperature"] = serde_json::json!(t);
        }

        if !tools.is_empty() {
            let api_tools: Vec<Value> = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.input_schema,
                        }
                    })
                })
                .collect();
            request_body["tools"] = Value::Array(api_tools);
        }

        let api_key = env::var("api_key").unwrap_or_default();
        if api_key.is_empty() {
            return Err(SysError::ApiError("api_key not configured".into()));
        }

        let req = http::Request::post(&url)
            .header("authorization", format!("Bearer {api_key}"))
            .json(&request_body)?;

        let stream = http::stream_start(&req)?;

        if stream.status() != 200 {
            // Drain the error body for the error message. Stream drops at
            // scope exit; no manual close required.
            let mut error_body = String::new();
            while let Some(chunk) = stream.read_chunk()? {
                error_body.push_str(&String::from_utf8_lossy(&chunk));
                if error_body.len() > 4096 {
                    error_body.truncate(4096);
                    break;
                }
            }
            return Err(SysError::ApiError(format!(
                "API error ({}): {error_body}",
                stream.status()
            )));
        }

        Self::parse_sse_stream_live(request_id, &stream)
        // `stream` drops here, releasing the kernel-side HTTP stream.
    }

    /// Stream SSE chunks in real-time, publishing IPC events as they arrive.
    fn parse_sse_stream_live(request_id: Uuid, stream: &http::HttpStream) -> Result<(), SysError> {
        let mut active_tools: Vec<(String, String)> = Vec::new();
        let mut line_buffer = String::new();

        while let Some(chunk) = stream.read_chunk()? {
            let chunk_str = String::from_utf8_lossy(&chunk);
            line_buffer.push_str(&chunk_str);

            if line_buffer.len() > MAX_LINE_BUFFER_SIZE {
                return Err(SysError::ApiError(
                    "SSE line buffer exceeded maximum size".into(),
                ));
            }

            // Process all complete lines in the buffer.
            while let Some(newline_pos) = line_buffer.find('\n') {
                let line = line_buffer[..newline_pos]
                    .trim_end_matches('\r')
                    .to_string();
                line_buffer = line_buffer[(newline_pos + 1)..].to_string();

                if line.is_empty() {
                    continue;
                }

                let Some(data) = Self::sse_data_payload(&line) else {
                    continue;
                };

                match Self::parse_sse_data(data)? {
                    SseData::Done => {
                        Self::publish_stream(request_id, StreamEvent::Done)?;
                        return Ok(());
                    }
                    SseData::Chunk(chunk) => {
                        Self::process_chunk(request_id, &chunk, &mut active_tools)?;
                    }
                }
            }
        }

        if line_buffer.trim().is_empty() {
            Err(SysError::ApiError("SSE stream ended before [DONE]".into()))
        } else {
            Err(SysError::ApiError(
                "SSE stream ended with a partial line".into(),
            ))
        }
    }

    fn parse_sse_data(data: &str) -> Result<SseData, SysError> {
        let data = data.trim();
        if data == "[DONE]" {
            return Ok(SseData::Done);
        }

        serde_json::from_str::<ChatCompletionChunk>(data)
            .map(SseData::Chunk)
            .map_err(|e| SysError::ApiError(format!("invalid SSE JSON: {e}")))
    }

    fn sse_data_payload(line: &str) -> Option<&str> {
        let data = line.strip_prefix("data:")?.trim_start();
        if data.trim().is_empty() {
            return None;
        }
        Some(data)
    }

    /// Process a single parsed SSE chunk, emitting the appropriate stream events.
    fn process_chunk(
        request_id: Uuid,
        chunk: &ChatCompletionChunk,
        active_tools: &mut Vec<(String, String)>,
    ) -> Result<(), SysError> {
        // Handle usage (final chunk with empty choices).
        if let Some(usage) = &chunk.usage {
            Self::publish_stream(
                request_id,
                StreamEvent::Usage {
                    input_tokens: usage.prompt_tokens,
                    output_tokens: usage.completion_tokens,
                },
            )?;
        }

        let Some(choice) = chunk.choices.first() else {
            return Ok(());
        };

        // Handle text deltas.
        if let Some(ref text) = choice.delta.content
            && !text.is_empty()
        {
            Self::publish_stream(request_id, StreamEvent::TextDelta(text.clone()))?;
        }

        // Handle tool call deltas.
        if let Some(ref tool_calls) = choice.delta.tool_calls {
            for tc in tool_calls {
                // Grow the tracking vec if needed.
                while active_tools.len() <= tc.index {
                    active_tools.push((String::new(), String::new()));
                }

                if let Some(ref id) = tc.id {
                    active_tools[tc.index].0 = id.clone();
                }

                if let Some(ref func) = tc.function {
                    if let Some(ref name) = func.name {
                        active_tools[tc.index].1 = name.clone();
                        Self::publish_stream(
                            request_id,
                            StreamEvent::ToolCallStart {
                                id: active_tools[tc.index].0.clone(),
                                name: name.clone(),
                            },
                        )?;
                    }

                    if let Some(ref args) = func.arguments
                        && !args.is_empty()
                    {
                        Self::publish_stream(
                            request_id,
                            StreamEvent::ToolCallDelta {
                                id: active_tools[tc.index].0.clone(),
                                args_delta: args.clone(),
                            },
                        )?;
                    }
                }
            }
        }

        // Handle finish reason: emit ToolCallEnd for all active tool calls.
        if let Some(ref reason) = choice.finish_reason
            && reason == "tool_calls"
        {
            for (id, _name) in active_tools.iter() {
                if !id.is_empty() {
                    Self::publish_stream(request_id, StreamEvent::ToolCallEnd { id: id.clone() })?;
                }
            }
            active_tools.clear();
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

    /// Convert an Astrid `Message` to the OpenAI Chat Completions JSON format.
    fn convert_message(message: &Message) -> Value {
        match &message.content {
            MessageContent::Text(text) => {
                serde_json::json!({
                    "role": Self::role_str(message.role),
                    "content": text,
                })
            }
            MessageContent::ToolCalls(calls) => {
                let tool_calls: Vec<Value> = calls
                    .iter()
                    .map(|c| {
                        serde_json::json!({
                            "id": c.id,
                            "type": "function",
                            "function": {
                                "name": c.name,
                                "arguments": if c.arguments.is_string() {
                                    c.arguments.clone()
                                } else {
                                    Value::String(c.arguments.to_string())
                                },
                            }
                        })
                    })
                    .collect();

                serde_json::json!({
                    "role": "assistant",
                    "content": null,
                    "tool_calls": tool_calls,
                })
            }
            MessageContent::ToolResult(result) => {
                serde_json::json!({
                    "role": "tool",
                    "tool_call_id": result.call_id,
                    "content": result.content,
                })
            }
            MessageContent::MultiPart(parts) => {
                let content: Vec<Value> = parts
                    .iter()
                    .map(|p| match p {
                        astrid_sdk::types::ContentPart::Text { text } => {
                            serde_json::json!({"type": "text", "text": text})
                        }
                        astrid_sdk::types::ContentPart::Image { media_type, data } => {
                            serde_json::json!({
                                "type": "image_url",
                                "image_url": {
                                    "url": format!("data:{media_type};base64,{data}"),
                                }
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
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        }
    }

    fn provider_model_id(model: &str) -> &str {
        model
            .strip_prefix(PROVIDER_ALIAS)
            .and_then(|rest| rest.strip_prefix(':'))
            .unwrap_or(model)
    }

    fn resolve_request_model(model: &str, env_model: Option<String>) -> String {
        let raw = if model.is_empty() {
            env_model.unwrap_or_else(|| "gpt-5.4".into())
        } else {
            model.to_string()
        };
        Self::provider_model_id(&raw).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{ModelList, OpenAICompatProvider, REQUEST_TOPIC, STREAM_TOPIC, SseData};

    /// Helper: collect the `id` field of every provider entry, in order.
    fn ids(entries: &[serde_json::Value]) -> Vec<String> {
        entries
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn parse_models_body_yields_one_entry_per_id() {
        // Representative `/v1/models` body with vendor-specific extra fields and
        // an ollama-style colon id alongside a real OpenAI-style id.
        let body = r#"{
            "object": "list",
            "data": [
                { "id": "gpt-5.4", "object": "model", "owned_by": "openai" },
                { "id": "llama3.3:70b", "object": "model", "owned_by": "library" },
                { "id": "mixtral-8x7b", "object": "model", "created": 1700000000 }
            ]
        }"#;
        let list: ModelList = serde_json::from_str(body).expect("parse model list");
        assert_eq!(list.data.len(), 3);
        // The colon in the ollama-style id survives deserialization verbatim.
        assert_eq!(list.data[1].id, "llama3.3:70b");

        let model_ids = OpenAICompatProvider::extract_model_ids(list).expect("non-empty");
        assert_eq!(model_ids, vec!["gpt-5.4", "llama3.3:70b", "mixtral-8x7b"]);

        let entries =
            OpenAICompatProvider::describe_providers(&model_ids, "gpt-5.4", 128_000, 8_192);
        assert_eq!(entries.len(), 3);
        for entry in &entries {
            assert_eq!(entry["request_topic"].as_str().unwrap(), REQUEST_TOPIC);
        }
        // `id` equals the source model id end-to-end (colon preserved).
        assert_eq!(
            ids(&entries),
            vec!["gpt-5.4", "llama3.3:70b", "mixtral-8x7b"]
        );
    }

    #[test]
    fn advertised_topics_use_stable_provider_alias() {
        assert_eq!(REQUEST_TOPIC, "llm.v1.request.generate.openai-compat");
        assert_eq!(STREAM_TOPIC, "llm.v1.stream.openai-compat");
        assert!(REQUEST_TOPIC.ends_with(super::PROVIDER_ALIAS));
        assert!(STREAM_TOPIC.ends_with(super::PROVIDER_ALIAS));
    }

    #[test]
    fn provider_request_model_strips_registry_prefix_only() {
        assert_eq!(
            OpenAICompatProvider::provider_model_id("openai-compat:gpt-5.4"),
            "gpt-5.4"
        );
        assert_eq!(
            OpenAICompatProvider::provider_model_id("openai-compat:llama3.3:70b"),
            "llama3.3:70b"
        );
        assert_eq!(
            OpenAICompatProvider::provider_model_id("llama3.3:70b"),
            "llama3.3:70b"
        );
        assert_eq!(
            OpenAICompatProvider::provider_model_id("other:gpt-5.4"),
            "other:gpt-5.4"
        );
    }

    #[test]
    fn request_model_normalization_also_applies_to_env_default() {
        assert_eq!(
            OpenAICompatProvider::resolve_request_model(
                "",
                Some("openai-compat:gpt-5.4".to_string())
            ),
            "gpt-5.4"
        );
        assert_eq!(
            OpenAICompatProvider::resolve_request_model(
                "",
                Some("openai-compat:llama3.3:70b".to_string())
            ),
            "llama3.3:70b"
        );
        assert_eq!(
            OpenAICompatProvider::resolve_request_model(
                "openai-compat:request-model",
                Some("openai-compat:env-model".to_string())
            ),
            "request-model"
        );
        assert_eq!(
            OpenAICompatProvider::resolve_request_model("", None),
            "gpt-5.4"
        );
    }

    #[test]
    fn sse_data_payload_accepts_common_wire_variants() {
        assert_eq!(
            OpenAICompatProvider::sse_data_payload("data: {\"choices\":[]}"),
            Some("{\"choices\":[]}")
        );
        assert_eq!(
            OpenAICompatProvider::sse_data_payload("data:{\"choices\":[]}"),
            Some("{\"choices\":[]}")
        );
        assert_eq!(
            OpenAICompatProvider::sse_data_payload("data:   [DONE]"),
            Some("[DONE]")
        );
        assert_eq!(OpenAICompatProvider::sse_data_payload("data:"), None);
        assert_eq!(OpenAICompatProvider::sse_data_payload("data:   "), None);
        assert_eq!(OpenAICompatProvider::sse_data_payload(": ping"), None);
    }

    #[test]
    fn sse_data_done_is_terminal() {
        assert!(matches!(
            OpenAICompatProvider::parse_sse_data("[DONE]").expect("done parses"),
            SseData::Done
        ));
        assert!(matches!(
            OpenAICompatProvider::parse_sse_data("  [DONE]  ").expect("done parses"),
            SseData::Done
        ));
    }

    #[test]
    fn sse_data_chunk_decodes_text_delta() {
        let data = r#"{
            "choices": [
                {
                    "delta": { "content": "hello" },
                    "finish_reason": null
                }
            ]
        }"#;
        let parsed = OpenAICompatProvider::parse_sse_data(data).expect("chunk parses");
        let SseData::Chunk(chunk) = parsed else {
            panic!("expected chunk");
        };
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("hello"));
    }

    #[test]
    fn malformed_sse_data_is_error() {
        let err = OpenAICompatProvider::parse_sse_data("{not-json}")
            .expect_err("malformed data must fail");
        assert!(
            err.to_string().contains("invalid SSE JSON"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn describe_emits_env_model_first() {
        // Default model is present but NOT first in discovered order.
        let model_ids = vec![
            "first-discovered".to_string(),
            "the-default".to_string(),
            "last-discovered".to_string(),
        ];
        let entries =
            OpenAICompatProvider::describe_providers(&model_ids, "the-default", 128_000, 8_192);

        // entry[0] is the env default; the rest keep their discovered order.
        assert_eq!(entries[0]["id"].as_str().unwrap(), "the-default");
        assert_eq!(
            ids(&entries),
            vec!["the-default", "first-discovered", "last-discovered"]
        );

        // No entry carries a "default" key — ordering is the only signal.
        for entry in &entries {
            assert!(entry.get("default").is_none());
        }
    }

    #[test]
    fn describe_preserves_order_when_default_absent() {
        // Default model is NOT in the discovered list.
        let model_ids = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
        let entries =
            OpenAICompatProvider::describe_providers(&model_ids, "not-present", 128_000, 8_192);

        // Discovered order preserved unchanged; nothing dropped.
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0]["id"].as_str().unwrap(), "alpha");
        assert_eq!(ids(&entries), vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn empty_data_is_discovery_error() {
        // Empty `data` array funnels to the fallback Err arm.
        let empty: ModelList = serde_json::from_str(r#"{ "data": [] }"#).expect("parse");
        assert!(OpenAICompatProvider::extract_model_ids(empty).is_err());

        // A sole entry with an empty `id` is dropped, leaving nothing usable.
        let blank: ModelList =
            serde_json::from_str(r#"{ "data": [ { "id": "" } ] }"#).expect("parse");
        assert!(OpenAICompatProvider::extract_model_ids(blank).is_err());

        // A sole entry with a whitespace-only `id` is likewise dropped: it
        // cannot be selected, so it funnels to the same fallback Err arm.
        let whitespace: ModelList =
            serde_json::from_str(r#"{ "data": [ { "id": "   " } ] }"#).expect("parse");
        assert!(OpenAICompatProvider::extract_model_ids(whitespace).is_err());
    }

    #[test]
    fn duplicate_ids_are_deduplicated_preserving_order() {
        // A buggy/hostile upstream that repeats an id must not yield two
        // provider entries with the same id. Dedup is stable on server order.
        let dup: ModelList =
            serde_json::from_str(r#"{ "data": [ { "id": "gpt-5.4" }, { "id": "gpt-5.4" } ] }"#)
                .expect("parse");
        let ids = OpenAICompatProvider::extract_model_ids(dup).expect("one survivor");
        assert_eq!(ids, vec!["gpt-5.4"]);
        assert_eq!(ids.len(), 1);

        // Non-adjacent collisions collapse too, and the first occurrence wins
        // (server order preserved for the survivors).
        let scattered: ModelList = serde_json::from_str(
            r#"{ "data": [ { "id": "a" }, { "id": "b" }, { "id": "a" }, { "id": "c" } ] }"#,
        )
        .expect("parse");
        let ids = OpenAICompatProvider::extract_model_ids(scattered).expect("survivors");
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn unparseable_body_is_discovery_error() {
        // A non-JSON / HTML body (e.g. a 404 page) fails to deserialize, which
        // is the Err that triggers the env fallback in `discover_models`.
        let result: Result<ModelList, _> = serde_json::from_str("<html>404</html>");
        assert!(result.is_err());
    }

    #[test]
    fn blank_env_model_is_treated_as_unset() {
        // Missing, empty, and whitespace-only env models all normalize to None,
        // so none of them can leak into the advertised id set.
        assert_eq!(OpenAICompatProvider::parse_env_model(None), None);
        assert_eq!(
            OpenAICompatProvider::parse_env_model(Some(String::new())),
            None
        );
        assert_eq!(
            OpenAICompatProvider::parse_env_model(Some("   \t".into())),
            None
        );
        // A real value survives, trimmed.
        assert_eq!(
            OpenAICompatProvider::parse_env_model(Some("  gpt-5.4 ".into())),
            Some("gpt-5.4".to_string())
        );
    }

    #[test]
    fn no_env_model_and_failed_discovery_yields_no_entries() {
        use super::SysError;

        // The exact `llm_describe` state when env `model` is unset/blank AND
        // `/v1/models` discovery fails: there is no configured default to fall
        // back to, so the resolved id list MUST be empty — NOT a bogus `unknown`
        // entry the registry could bind and then send upstream verbatim.
        let failed: Result<Vec<String>, SysError> =
            Err(SysError::ApiError("discovery failed".into()));
        let model_ids = OpenAICompatProvider::resolve_model_ids(failed, None);
        assert!(
            model_ids.is_empty(),
            "no env model + failed discovery must advertise nothing, got: {model_ids:?}"
        );

        // And the providers built from it carry no entries at all.
        let providers = OpenAICompatProvider::describe_providers(&model_ids, "", 128_000, 8_192);
        assert!(providers.is_empty());
    }

    #[test]
    fn configured_default_survives_failed_discovery() {
        use super::SysError;

        // With a configured default, a discovery failure still falls back to the
        // single pinned id so existing installs never regress.
        let failed: Result<Vec<String>, SysError> = Err(SysError::ApiError("boom".into()));
        let model_ids = OpenAICompatProvider::resolve_model_ids(failed, Some("gpt-5.4"));
        assert_eq!(model_ids, vec!["gpt-5.4"]);
    }

    #[test]
    fn successful_discovery_ignores_default_fallback() {
        use super::SysError;

        // On success the discovered ids are used verbatim regardless of default.
        let ok: Result<Vec<String>, SysError> = Ok(vec!["a".into(), "b".into()]);
        assert_eq!(
            OpenAICompatProvider::resolve_model_ids(ok, None),
            vec!["a", "b"]
        );
    }

    #[test]
    fn fallback_advertisement_matches_single_model_shape() {
        // The discovery-failure path advertises a single env-model entry.
        let entries = OpenAICompatProvider::describe_providers(
            &["gpt-5.4".to_string()],
            "gpt-5.4",
            128_000,
            8_192,
        );

        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry["id"].as_str().unwrap(), "gpt-5.4");
        assert_eq!(entry["request_topic"].as_str().unwrap(), REQUEST_TOPIC);
        // Shape-stable, positionally-default, no "default" key.
        assert!(entry.get("default").is_none());
    }

    #[test]
    fn whitespace_only_api_key_is_treated_as_keyless() {
        // A copy-pasted key that is empty or only whitespace/newlines must NOT
        // produce an `Authorization` header: sending `Bearer <whitespace>`
        // breaks discovery against keyless/permissive servers. Treat it as
        // keyless (no header) instead.
        assert_eq!(OpenAICompatProvider::bearer_header(""), None);
        assert_eq!(OpenAICompatProvider::bearer_header("   "), None);
        assert_eq!(OpenAICompatProvider::bearer_header("\n"), None);
        assert_eq!(OpenAICompatProvider::bearer_header(" \t\r\n "), None);
    }

    #[test]
    fn real_api_key_is_trimmed_and_sent() {
        // A genuine key still produces a header, with stray surrounding
        // whitespace (copy-paste newline) stripped from the sent value.
        assert_eq!(
            OpenAICompatProvider::bearer_header("sk-abc123"),
            Some("Bearer sk-abc123".to_string())
        );
        assert_eq!(
            OpenAICompatProvider::bearer_header("  sk-abc123\n"),
            Some("Bearer sk-abc123".to_string())
        );
    }
}
