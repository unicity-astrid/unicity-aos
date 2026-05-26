# astrid-capsule-openai-compat

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![MSRV: 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](https://www.rust-lang.org)

**The OpenAI-compatible LLM provider for [Astrid OS](https://github.com/unicity-astrid/astrid).**

In the OS model, this capsule is a device driver. It translates between Astrid's standardized LLM event protocol and any OpenAI-compatible Chat Completions API — the same way a device driver translates between an OS and hardware.

Configure `base_url` to point at any compatible provider:

| Provider | `base_url` |
|---|---|
| OpenAI | `https://api.openai.com/v1` |
| Groq | `https://api.groq.com/openai/v1` |
| Together | `https://api.together.xyz/v1` |
| Mistral | `https://api.mistral.ai/v1` |
| DeepSeek | `https://api.deepseek.com/v1` |
| Fireworks | `https://api.fireworks.ai/inference/v1` |

## How it works

1. Subscribes to `llm.v1.request.generate.openai-compat` IPC events
2. Converts Astrid's `Message` format to the OpenAI Chat Completions JSON format (text, tool calls, tool results, multipart)
3. Opens a streaming HTTP connection to `{base_url}/chat/completions` via the HTTP streaming airlock
4. Parses the SSE response in real-time and publishes standardized `llm.v1.stream.openai-compat` events back to the IPC bus as chunks arrive

Stream events cover the full response lifecycle: text deltas, parallel tool call start/delta/end, usage reporting (prompt + completion tokens), and completion.

## Configuration

The capsule reads these environment variables during `astrid init`:

| Variable | Type | Description |
|---|---|---|
| `api_key` | secret | API key for the provider |
| `base_url` | string | API base URL (default: `https://api.openai.com/v1`) |
| `model` | string | Default model ID (default: `gpt-4o`) |

## IPC protocol

| Direction | Topic | Payload |
|---|---|---|
| Subscribe | `llm.v1.request.generate.openai-compat` | `IpcPayload::LlmRequest` |
| Publish | `llm.v1.stream.openai-compat` | `IpcPayload::LlmStreamEvent` |

## Development

```bash
rustup target add wasm32-unknown-unknown
cargo build --target wasm32-unknown-unknown --release
```

## License

Dual-licensed under [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE).

Copyright (c) 2025-2026 Joshua J. Bouw and Unicity Labs.
