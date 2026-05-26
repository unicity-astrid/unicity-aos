# astrid-capsule-openai

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![MSRV: 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](https://www.rust-lang.org)

**The native OpenAI LLM provider for [Astrid OS](https://github.com/unicity-astrid/astrid).**

In the OS model, this capsule is a device driver. It translates between Astrid's standardized LLM event protocol and OpenAI's Chat Completions API — with full support for OpenAI-specific features that generic compatible providers don't offer.

For generic OpenAI-compatible providers (Groq, Together, Mistral, DeepSeek, Fireworks, etc.), use [`astrid-capsule-openai-compat`](https://github.com/unicity-astrid/capsule-openai-compat) instead.

## OpenAI-specific features

| Feature | Description |
|---|---|
| **Strict function calling** | `strict: true` on tool definitions — guaranteed schema adherence |
| **Reasoning effort** | `reasoning_effort` for o-series models (`low`/`medium`/`high`) |
| **Service tier** | `auto`/`default`/`flex`/`priority` routing |
| **`max_completion_tokens`** | OpenAI's preferred field (distinct from generic `max_tokens`) |
| **Parallel tool calls** | Explicit `parallel_tool_calls: true` |
| **Structured outputs** | `response_format` with `json_schema` support (planned) |

## How it works

1. Subscribes to `llm.v1.request.generate.openai` IPC events
2. Converts Astrid's `Message` format to the OpenAI Chat Completions JSON format (text, tool calls, tool results, multipart/vision)
3. Opens a streaming HTTP connection to `https://api.openai.com/v1/chat/completions` via the HTTP streaming airlock
4. Parses the SSE response in real-time and publishes standardized `llm.v1.stream.openai` events back to the IPC bus as chunks arrive

Stream events cover the full response lifecycle: text deltas, parallel tool call start/delta/end, usage reporting (prompt + completion tokens), and completion.

## Configuration

The capsule reads these environment variables during `astrid init`:

| Variable | Type | Description |
|---|---|---|
| `api_key` | secret | OpenAI API key |
| `model` | string | Default model ID (default: `gpt-4.1`) |
| `context_window` | integer | Model context window in tokens (default: `128000`) |
| `max_output_tokens` | integer | Max output tokens (default: `8192`) |
| `temperature` | string | Default temperature 0.0-2.0 (blank = provider default) |
| `reasoning_effort` | string | For o-series models: `low`/`medium`/`high` (blank = default) |
| `service_tier` | string | `auto`/`default`/`flex`/`priority` (default: `auto`) |

## IPC protocol

| Direction | Topic | Payload |
|---|---|---|
| Subscribe | `llm.v1.request.generate.openai` | `IpcPayload::LlmRequest` |
| Publish | `llm.v1.stream.openai` | `IpcPayload::LlmStreamEvent` |
| Subscribe | `llm.v1.request.describe` | Provider discovery |

## Development

```bash
rustup target add wasm32-unknown-unknown
cargo build --target wasm32-unknown-unknown --release
```

## License

Dual-licensed under [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE).

Copyright (c) 2026 Joshua J. Bouw and Unicity Labs.
