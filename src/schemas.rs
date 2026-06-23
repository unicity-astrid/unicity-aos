//! OpenAI Responses API streaming event types.

use serde::Deserialize;

/// Text delta event — `response.output_text.delta`
#[derive(Deserialize)]
#[expect(dead_code)]
pub(crate) struct TextDelta {
    /// The item ID this delta belongs to.
    pub item_id: String,
    /// Index in the output array.
    pub output_index: usize,
    /// Index within the content parts.
    pub content_index: usize,
    /// The text chunk.
    pub delta: String,
}

/// Function call arguments delta — `response.function_call_arguments.delta`
#[derive(Deserialize)]
#[expect(dead_code)]
pub(crate) struct FunctionCallArgsDelta {
    /// The item ID (tool call ID).
    pub item_id: String,
    /// Index in the output array.
    pub output_index: usize,
    /// Partial JSON arguments.
    pub delta: String,
}

/// Function call arguments done — `response.function_call_arguments.done`
#[derive(Deserialize)]
#[expect(dead_code)]
pub(crate) struct FunctionCallArgsDone {
    /// The item ID (tool call ID).
    pub item_id: String,
    /// Index in the output array.
    pub output_index: usize,
    /// The function name.
    pub name: String,
    /// Complete JSON arguments.
    pub arguments: String,
}

/// Output item added — `response.output_item.added`
#[derive(Deserialize)]
#[expect(dead_code)]
pub(crate) struct OutputItemAdded {
    /// The output item.
    pub item: OutputItem,
    /// Index in the output array.
    pub output_index: usize,
}

/// An item in the response output array.
#[derive(Deserialize)]
pub(crate) struct OutputItem {
    /// Item ID.
    pub id: String,
    /// Item type: "message", "function_call", etc.
    #[serde(rename = "type")]
    pub item_type: String,
    /// Function name (only for function_call items).
    #[serde(default)]
    pub name: Option<String>,
    /// Call ID for function calls.
    #[serde(default)]
    pub call_id: Option<String>,
}

/// Response completed — `response.completed`
#[derive(Deserialize)]
pub(crate) struct ResponseCompleted {
    /// The full response object.
    pub response: ResponseObject,
}

/// The response object within a completed event.
#[derive(Deserialize)]
#[expect(dead_code)]
pub(crate) struct ResponseObject {
    /// Response status.
    pub status: String,
    /// Token usage.
    #[serde(default)]
    pub usage: Option<Usage>,
}

/// Token usage statistics.
#[derive(Deserialize)]
pub(crate) struct Usage {
    /// Input tokens consumed.
    pub input_tokens: usize,
    /// Output tokens generated.
    pub output_tokens: usize,
}

/// Response shape of `GET {base_url}/v1/models` (OpenAI list-models format).
/// Unknown extra fields are ignored; only `data[].id` is consumed.
#[derive(Deserialize)]
pub(crate) struct ModelList {
    /// The catalogue of servable models.
    #[serde(default)]
    pub data: Vec<ModelEntry>,
}

/// A single entry in the `/v1/models` `data` array. Only `id` is consumed;
/// every other field (`object`, `created`, `owned_by`, ...) is ignored.
#[derive(Deserialize)]
pub(crate) struct ModelEntry {
    /// The model id to advertise as a provider-entry id.
    pub id: String,
}
