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
//! - Together: `https://api.together.xyz`
//! - Mistral: `https://api.mistral.ai`
//! - DeepSeek: `https://api.deepseek.com`
//! - Fireworks: `https://api.fireworks.ai/inference`

mod schemas;

use astrid_sdk::prelude::*;
use astrid_sdk::types::{IpcPayload, Message, MessageContent, MessageRole, StreamEvent};
use schemas::ChatCompletionChunk;
use serde_json::Value;
use uuid::Uuid;

const STREAM_TOPIC: &str = "llm.v1.stream.openai-compat";
/// Maximum SSE line buffer size (1 MB). If the server sends data without
/// a newline that exceeds this, the stream is aborted.
const MAX_LINE_BUFFER_SIZE: usize = 1024 * 1024;

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
            let _ = log::error(format!("LLM request failed: {e}"));
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
    /// Called by the registry capsule via `hooks::trigger("llm.v1.request.describe")`.
    /// Returns the provider's model ID, capabilities, and IPC routing topics.
    #[astrid::interceptor("llm_describe")]
    pub fn llm_describe(&self, _payload: serde_json::Value) -> Result<serde_json::Value, SysError> {
        let model = env::var("model").unwrap_or_else(|_| "unknown".into());
        let context_window = env::var("context_window")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(128_000);
        let max_output = env::var("max_output_tokens")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(8_192);
        Ok(serde_json::json!({
            "providers": [{
                "id": "openai-compat",
                "description": format!("OpenAI-compatible provider (default model: {model})"),
                "capabilities": ["text", "vision", "tools"],
                "request_topic": "llm.v1.request.generate.openai-compat",
                "stream_topic": "llm.v1.stream.openai-compat",
                "context_window": context_window,
                "max_output_tokens": max_output,
            }]
        }))
    }
}

impl OpenAICompatProvider {
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

        let resolved_model = if model.is_empty() {
            env::var("model").unwrap_or_else(|_| "gpt-5.4".into())
        } else {
            model.to_string()
        };

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

        let resp = http::stream_start(&req)?;

        if resp.status != 200 {
            // Drain the error body for the error message.
            let mut error_body = String::new();
            while let Some(chunk) = http::stream_read(&resp.handle)? {
                error_body.push_str(&String::from_utf8_lossy(&chunk));
                if error_body.len() > 4096 {
                    error_body.truncate(4096);
                    break;
                }
            }
            let _ = http::stream_close(&resp.handle);
            return Err(SysError::ApiError(format!(
                "API error ({}): {error_body}",
                resp.status
            )));
        }

        let result = Self::parse_sse_stream_live(request_id, &resp.handle);
        let _ = http::stream_close(&resp.handle);
        result
    }

    /// Stream SSE chunks in real-time, publishing IPC events as they arrive.
    fn parse_sse_stream_live(
        request_id: Uuid,
        stream: &http::HttpStreamHandle,
    ) -> Result<(), SysError> {
        let mut active_tools: Vec<(String, String)> = Vec::new();
        let mut line_buffer = String::new();

        while let Some(chunk) = http::stream_read(stream)? {
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

                let Some(data) = line.strip_prefix("data: ") else {
                    continue;
                };

                if data == "[DONE]" {
                    Self::publish_stream(request_id, StreamEvent::Done)?;
                    return Ok(());
                }

                let Ok(chunk) = serde_json::from_str::<ChatCompletionChunk>(data) else {
                    continue;
                };

                Self::process_chunk(request_id, &chunk, &mut active_tools)?;
            }
        }

        Ok(())
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
}
