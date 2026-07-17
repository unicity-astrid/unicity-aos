# aos-openai

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![MSRV: 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](https://www.rust-lang.org)

**The native OpenAI LLM provider for [Unicity AOS](https://github.com/unicity-aos/aos-ce).**

In the OS model, this capsule is a device driver. It translates between the runtime's standardized LLM
event protocol and OpenAI's Responses API -- with full support for OpenAI-specific features that
generic compatible providers do not offer.

For generic OpenAI-compatible providers (Groq, Together, Mistral, DeepSeek, Fireworks, LM Studio,
vLLM, llama.cpp, etc.), use
[`aos-openai-compat`](https://github.com/unicity-aos/aos-ce/tree/main/capsules/capsule-openai-compat) instead.

## OpenAI-specific features

| Feature | Description |
|---|---|
| **Responses API** | Uses `POST /v1/responses` (OpenAI's recommended API for new projects) |
| **Strict function calling** | `strict: true` on tool definitions -- guaranteed schema adherence |
| **Reasoning effort** | `none`/`low`/`medium`/`high`/`xhigh` for GPT-5.x and o-series models |
| **Service tier** | `auto`/`default`/`flex`/`priority` routing |
| **`max_output_tokens`** | OpenAI's top-level field (distinct from generic `max_tokens`) |
| **Parallel tool calls** | Concurrent function calls in a single response turn |
| **Structured outputs** | `text.format` with `json_schema` support |

## How it works

1. Subscribes to `llm.v1.request.generate.openai` IPC events
2. Converts the runtime's `Message` format to the Responses API `input` format (text, tool calls, tool
   results, multipart/vision)
3. Opens a streaming HTTP connection to `https://api.openai.com/v1/responses` via the HTTP
   streaming airlock
4. Parses the named SSE response events in real-time and publishes standardized
   `llm.v1.stream.openai` events back to the IPC bus as chunks arrive

Stream events cover the full response lifecycle: text deltas, parallel tool call start/delta/end,
usage reporting (prompt + completion tokens), and completion.

## Model discovery

When the registry asks this capsule what it can serve, the capsule queries
`GET {base_url}/v1/models` (authenticated with the configured `api_key`) and returns one provider
entry per discovered model id. Every entry shares the same request and stream topics; the entry id
IS the model id.

Each discovered id is enriched from a built-in capability catalog: context window, max output
tokens, and capability tags (`text`, `tools`, `vision`, `structured_output`, `reasoning`) are
resolved from the catalog for known ids, and unknown ids get conservative defaults
(128,000 token context, 16,384 max output, `text` + `tools` only).

The env `model` value controls entry ordering: the configured default is hoisted to `entry[0]`
(present in the live list) or prepended as its own enriched entry (absent from the live list), so
the registry always has a usable default-hint model.

If the live query fails for any reason (network error, non-2xx, empty response, missing API key),
the capsule falls back to the full built-in catalog. This keeps an offline or keyless install from
advertising nothing -- the known frontier and common model families are always available as
selectable entries.

Discovery runs at describe-time (when the registry fans out `llm.v1.request.describe`), not at
startup.

### Built-in catalog

The catalog covers the frontier and common model families. Dated snapshots (e.g.
`gpt-5.4-2026-03-05`) resolve to their canonical catalog row via longest-prefix matching and
inherit the same capability flags.

| Model | Context window | Max output | Capabilities |
|---|---|---|---|
| `gpt-5.5` | 1,050,000 | 128,000 | text, tools, vision, structured output, reasoning |
| `gpt-5.5-codex` | 1,050,000 | 128,000 | text, tools, vision, structured output, reasoning |
| `gpt-5.4` | 1,050,000 | 128,000 | text, tools, vision, structured output, reasoning |
| `gpt-5.4-mini` | 400,000 | 128,000 | text, tools, vision, structured output, reasoning |
| `gpt-5.4-nano` | 400,000 | 128,000 | text, tools, vision, structured output, reasoning |
| `gpt-5.3` | 400,000 | 128,000 | text, tools, vision, structured output |
| `gpt-5.3-codex` | 1,000,000 | 128,000 | text, tools, vision, structured output, reasoning |
| `gpt-5.3-codex-spark` | 128,000 | 128,000 | text, tools, structured output |
| `gpt-5.2` | 400,000 | 128,000 | text, tools, vision, structured output, reasoning |
| `gpt-5.2-codex` | 400,000 | 128,000 | text, tools, vision, structured output, reasoning |
| `gpt-4.1` | 1,048,576 | 32,768 | text, tools, vision, structured output |
| `gpt-4.1-mini` | 1,048,576 | 32,768 | text, tools, vision, structured output |
| `gpt-4.1-nano` | 1,048,576 | 32,768 | text, tools, vision, structured output |
| `o3` | 200,000 | 100,000 | text, tools, vision, structured output, reasoning |
| `o3-mini` | 200,000 | 100,000 | text, tools, structured output, reasoning |
| `o4-mini` | 200,000 | 100,000 | text, tools, vision, structured output, reasoning |
| `gpt-4o` | 128,000 | 16,384 | text, tools, vision, structured output |
| `gpt-4o-mini` | 128,000 | 16,384 | text, tools, vision, structured output |

Unknown live ids not covered by the catalog fall back to conservative defaults (128,000 context,
16,384 max output, `text` + `tools` only) and are named after their id rather than the generic
"Unknown Model".

## Configuration

These fields are prompted during `aos init`. In a standalone Astrid Runtime installation,
the equivalent distribution-managed flow is `astrid distro apply <source>`. The `model` field is populated live from OpenAI's `/v1/models` once
`base_url` and `api_key` are entered.

| Variable | Type | Default | Description |
|---|---|---|---|
| `base_url` | string | `https://api.openai.com` | OpenAI API base URL (without `/v1`) |
| `api_key` | secret | -- | OpenAI API key, sent as `Authorization: Bearer ...` |
| `model` | select | `gpt-5.5` | Default model; populated live from `{base_url}/v1/models` during onboarding |
| `temperature` | string | _(unset)_ | Sampling temperature (`0.0`--`2.0`); blank uses the model default. Not applied when `reasoning_effort` is set. |
| `reasoning_effort` | string | _(unset)_ | Reasoning effort for GPT-5.x and o-series: `none`/`low`/`medium`/`high`/`xhigh` |
| `service_tier` | string | `auto` | Service tier: `auto`/`default`/`flex`/`priority` |

`context_window` and `max_output_tokens` are resolved from the built-in catalog for each model
and do not need to be set manually. Set them as env vars only to override catalog defaults for
specific models.

### The `model` field is a live select

During `aos init`, the installer fetches `{base_url}/v1/models` (using the entered `api_key`)
and presents a numbered menu of available models. The configured default (`gpt-5.5`) is
pre-selected. If the endpoint cannot be reached the installer falls back to free-text entry.

The manifest declaration looks like this:

```toml
[env]
model = { type = "select", request = "...", default = "gpt-5.5",
          options_from = { http = "{base_url}/v1/models", bearer = "{api_key}",
                           select = "data[].id", after = ["base_url", "api_key"] } }
```

The `after` constraint means the model select only runs once `base_url` and `api_key` are known.
The installer attaches the bearer only to requests to the configured `base_url` host, and caps
the response at 5 MB.

## Selecting a model at runtime

Model selection is per-principal and stored in the registry capsule's KV store. You can change the
active model at any time without touching the capsule configuration.

**CLI:**

```sh
# List all models available across all configured providers
aos models list

# List with machine-readable output
aos models list --json

# Show the currently active model for your principal
aos models current
aos models current --json

# Select a model by bare id (when unambiguous across providers)
aos models set gpt-5.5
aos models set o3

# Disambiguate when two providers serve the same model name
aos models set openai:gpt-5.5

# Clear the active selection (falls back to the auto-selected default)
aos models unset
```

`aos models` is a shorthand for `aos capsule models` -- both reach the same registry
capsule verb.

**HTTP API (gateway):**

```sh
# List models available to the authenticated principal
GET /api/models

# Get the active model
GET /api/models/active

# Set the active model
PUT /api/models/active
Content-Type: application/json

{ "id": "gpt-5.5" }
```

All three endpoints are scoped to the authenticated bearer principal.

### What happens when nothing is selected

If no model is selected and no provider is configured, prompts fail with a clear error:

```
No LLM model is selected. Run `aos models` to choose one, or install/configure an LLM provider.
```

The react capsule never fabricates a default model -- a missing selection always surfaces as an
error, not a silent fallback to an arbitrary model.

## Onboarding during `aos init`

When running `aos init` or installing a distro that includes this capsule, the installer
presents all `group = "llm"` capsules as a multi-select ("which provider(s) do you want to set
up?"). For each chosen provider it then runs the onboarding sequence in order:

1. Enter `base_url` (default: `https://api.openai.com`)
2. Enter `api_key`
3. Pick a model from the live numbered menu (populated from `{base_url}/v1/models`)

The default model (`gpt-5.5`) is pre-selected in the numbered menu. If the endpoint cannot be
reached, the installer falls back to free-text entry.

## IPC protocol

| Direction | Topic | Payload |
|---|---|---|
| Subscribe | `llm.v1.request.generate.openai` | `IpcPayload::LlmRequest` |
| Subscribe | `llm.v1.request.describe` | describe request (registry fan-out) |
| Publish | `llm.v1.stream.openai` | `IpcPayload::LlmStreamEvent` |
| Publish | `llm.v1.response.describe` | provider descriptor array |

## Development

```bash
rustup target add wasm32-unknown-unknown
cargo build
```

## License

Dual-licensed under [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE).

Copyright (c) 2026 Joshua J. Bouw and Unicity Labs.
