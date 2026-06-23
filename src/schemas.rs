//! OpenAI-compatible Chat Completions streaming types.
//!
//! These types map to the OpenAI Chat Completions API streaming format,
//! which is also implemented by Groq, Together, Mistral, DeepSeek,
//! Fireworks, and many other providers.

use serde::Deserialize;

/// A streaming chunk from the Chat Completions API.
#[derive(Deserialize, Debug)]
pub(crate) struct ChatCompletionChunk {
    /// Choices in this chunk (empty in the final usage-only chunk).
    #[serde(default)]
    pub(crate) choices: Vec<ChunkChoice>,
    /// Usage statistics (present only in the final chunk when
    /// `stream_options.include_usage` is set).
    pub(crate) usage: Option<OpenAIUsage>,
}

/// A single choice within a streaming chunk.
#[derive(Deserialize, Debug)]
pub(crate) struct ChunkChoice {
    /// The delta content for this choice.
    pub(crate) delta: ChunkDelta,
    /// Finish reason: `null` while streaming, then `"stop"`, `"tool_calls"`,
    /// or `"length"` at the end.
    pub(crate) finish_reason: Option<String>,
}

/// Incremental delta within a streaming choice.
#[derive(Deserialize, Debug)]
pub(crate) struct ChunkDelta {
    /// Text content delta.
    pub(crate) content: Option<String>,
    /// Tool call deltas (for parallel tool calls, indexed).
    pub(crate) tool_calls: Option<Vec<ChunkToolCall>>,
}

/// A tool call delta in the streaming response.
#[derive(Deserialize, Debug)]
pub(crate) struct ChunkToolCall {
    /// Index of the tool call (supports parallel tool calls).
    pub(crate) index: usize,
    /// Tool call ID (present only in the first chunk for this call).
    pub(crate) id: Option<String>,
    /// Function details.
    pub(crate) function: Option<ChunkFunction>,
}

/// Function details within a tool call delta.
#[derive(Deserialize, Debug)]
pub(crate) struct ChunkFunction {
    /// Function name (present only in the first chunk for this call).
    pub(crate) name: Option<String>,
    /// Partial JSON arguments to append.
    pub(crate) arguments: Option<String>,
}

/// Token usage statistics.
#[derive(Deserialize, Debug)]
pub(crate) struct OpenAIUsage {
    /// Input/prompt tokens consumed.
    pub(crate) prompt_tokens: usize,
    /// Output/completion tokens generated.
    pub(crate) completion_tokens: usize,
}

/// Response shape of `GET {base_url}/v1/models` (OpenAI list-models format,
/// implemented by OpenAI, Groq, Together, LM Studio, vLLM, llama.cpp, ...).
#[derive(Deserialize, Debug)]
pub(crate) struct ModelList {
    /// The catalogue of servable models. Unknown extra fields are ignored.
    #[serde(default)]
    pub(crate) data: Vec<ModelEntry>,
}

/// A single entry in the `/v1/models` `data` array. Only `id` is consumed;
/// every other field (`object`, `created`, `owned_by`, ...) is ignored.
#[derive(Deserialize, Debug)]
pub(crate) struct ModelEntry {
    /// The model id to advertise as a provider-entry id.
    pub(crate) id: String,
}

/// HTTP response payload from the SDK http airlock (buffered fallback).
#[derive(Deserialize)]
#[expect(dead_code)]
pub(crate) struct HttpResponse {
    /// HTTP status code.
    pub(crate) status: u16,
    /// Response headers.
    pub(crate) headers: std::collections::HashMap<String, String>,
    /// Response body.
    pub(crate) body: String,
}
