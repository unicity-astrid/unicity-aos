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
//! (Groq, Together, Mistral, etc.), use `astrid-capsule-openai-compat` instead.

mod models;
mod schemas;

use astrid_sdk::prelude::*;
use astrid_sdk::types::{IpcPayload, Message, MessageContent, MessageRole, StreamEvent};
use models::lookup;
use schemas::{
    FunctionCallArgsDelta, FunctionCallArgsDone, OutputItemAdded, ResponseCompleted, TextDelta,
};
use serde_json::Value;
use uuid::Uuid;

const STREAM_TOPIC: &str = "llm.v1.stream.openai";
const BASE_URL: &str = "https://api.openai.com";
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
        let model_id = env::var("model").unwrap_or_else(|_| "gpt-5.4".into());
        let info = lookup(&model_id);

        let context_window = env::var("context_window")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(info.context_window);
        let max_output = env::var("max_output_tokens")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(info.max_output_tokens);

        let mut capabilities = vec!["text", "tools"];
        if info.supports_vision {
            capabilities.push("vision");
        }
        if info.supports_structured_output {
            capabilities.push("structured_output");
        }
        if info.is_reasoning {
            capabilities.push("reasoning");
        }

        let response = serde_json::json!({
            "providers": [{
                "id": "openai",
                "description": format!("OpenAI {} ({})", info.name, model_id),
                "capabilities": capabilities,
                "request_topic": "llm.v1.request.generate.openai",
                "stream_topic": STREAM_TOPIC,
                "context_window": context_window,
                "max_output_tokens": max_output,
                "models": models::list_model_ids(),
            }]
        });
        ipc::publish_json("llm.v1.response.describe", &response)?;
        Ok(response)
    }
}

impl OpenAIProvider {
    /// Build and send the Responses API request, then parse the SSE stream.
    fn execute_request(
        request_id: Uuid,
        model: &str,
        messages: &[Message],
        tools: &[astrid_sdk::types::LlmToolDefinition],
        system: &str,
    ) -> Result<(), SysError> {
        let url = format!("{BASE_URL}/v1/responses");

        let resolved_model = if model.is_empty() {
            env::var("model").unwrap_or_else(|_| "gpt-5.4".into())
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

        let api_key = env::var("api_key").unwrap_or_default();
        if api_key.is_empty() {
            return Err(SysError::ApiError("OpenAI api_key not configured".into()));
        }

        let req = http::Request::post(&url)
            .header("authorization", format!("Bearer {api_key}"))
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
