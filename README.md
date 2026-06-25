# astrid-capsule-openai-compat

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![MSRV: 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](https://www.rust-lang.org)

**The OpenAI-compatible LLM provider for [Astrid OS](https://github.com/unicity-astrid/astrid).**

In the OS model, this capsule is a device driver. It translates between Astrid's standardized LLM
event protocol and any OpenAI-compatible Chat Completions API -- the same way a device driver
translates between an OS and hardware.

Configure `base_url` to point at any compatible provider:

| Provider | `base_url` | Notes |
|---|---|---|
| OpenAI | `https://api.openai.com` | |
| Groq | `https://api.groq.com/openai` | |
| Together | `https://api.together.ai` | |
| Mistral | `https://api.mistral.ai` | |
| DeepSeek | `https://api.deepseek.com` | |
| Fireworks | `https://api.fireworks.ai/inference` | |
| LM Studio | `http://localhost:1234` | Requires operator local-egress exemption -- see below |
| vLLM | `http://localhost:8000` | Requires operator local-egress exemption -- see below |
| llama.cpp | `http://localhost:8080` | Requires operator local-egress exemption -- see below |

Set `base_url` to the provider **origin only** -- the capsule appends `/v1/chat/completions` for
generation and `/v1/models` for discovery. Do not include a `/v1` suffix.

## How it works

1. Subscribes to `llm.v1.request.generate.astrid-capsule-openai-compat` IPC events
2. Converts Astrid's `Message` format to the OpenAI Chat Completions JSON format (text, tool calls,
   tool results, multipart)
3. Opens a streaming HTTP connection to `{base_url}/v1/chat/completions` via the HTTP streaming
   airlock
4. Parses the SSE response in real-time and publishes standardized `llm.v1.stream.astrid-capsule-openai-compat`
   events back to the IPC bus as chunks arrive

Stream events cover the full response lifecycle: text deltas, parallel tool call
start/delta/end, usage reporting (prompt + completion tokens), and completion.

## Model discovery

When the registry asks this capsule what it can serve, the capsule queries
`GET {base_url}/v1/models` and returns one provider entry per discovered model id. Every entry
shares the same request and stream topics; the entry id IS the model id.

Discovery runs at describe-time (when the registry fans out `llm.v1.request.describe`), not at
startup. The env `model` value controls two things during discovery:

- **Ordering.** If the env model appears in the discovered list, its entry is emitted first
  (`entry[0]`), so the registry can pre-select it. All other models keep their upstream order.
- **Offline fallback.** If `{base_url}/v1/models` is unreachable or returns an error, the capsule
  falls back to advertising a single entry for the configured env model. This keeps existing pinned
  installs working when the endpoint is temporarily down.

If discovery fails AND no env model is configured, the capsule advertises nothing rather than
invent a bogus id the registry could send upstream verbatim.

Ollama-style ids that contain a colon (e.g. `llama3.3:70b`) pass through verbatim -- the colon is
preserved in the id and is selectable as-is.

## Configuration

These fields are prompted during `astrid init` or when the capsule is selected during
`astrid distro install`. Every field except `api_key` has a default or can be left blank.

| Variable | Type | Default | Description |
|---|---|---|---|
| `api_key` | secret | -- | Provider API key, sent as `Authorization: Bearer ...`. Leave blank for keyless/local endpoints (LM Studio, llama.cpp). |
| `base_url` | string | `https://api.openai.com` | Provider origin without `/v1` |
| `model` | select | `gpt-5.4` | Default model; populated live from `{base_url}/v1/models` during onboarding |
| `context_window` | integer | `128000` | Context window (tokens) advertised to the registry |
| `max_output_tokens` | integer | `8192` | Sent as `max_tokens` on each request |
| `temperature` | string | _(unset)_ | Sampling temperature (`0.0`--`2.0`); blank uses the provider default |

### The `model` field is a live select

During `astrid init`, the installer fetches `{base_url}/v1/models` (using the entered `api_key`)
and presents a numbered menu of available models. The configured `model` default is pre-selected.
If the endpoint cannot be reached the installer falls back to free-text entry.

The manifest declaration looks like this:

```toml
[env]
model = { type = "select", request = "Enter the default model ID", default = "gpt-5.4",
          options_from = { http = "{base_url}/v1/models", bearer = "{api_key}",
                           select = "data[].id", after = ["base_url", "api_key"] } }
```

The `after` constraint means the model select only runs once `base_url` and `api_key` are known.
The installer attaches the bearer only to requests to the configured `base_url` host, and caps
the response at 5 MB.

### Keyless endpoints

For local servers that do not require authentication (LM Studio, llama.cpp, a local vLLM
instance), leave `api_key` blank. A blank, whitespace-only, or newline-only key is treated as
absent: no `Authorization` header is sent. This avoids sending `Authorization: Bearer ` (with a
blank value) which many permissive servers reject.

### Local endpoints and the SSRF airlock

The `astrid:http` host capability includes an SSRF airlock that blocks outbound requests to any
address that resolves to a loopback, private, or link-local range -- `127.0.0.1`, `::1`,
`192.168.x.x`, `10.x.x.x`, `169.254.x.x`, and similar. The airlock is on by default and protects
against server-side request forgery.

This matters for local LLM servers. If `base_url` points at a local address (for example LM
Studio on `http://localhost:1234`, Ollama on `http://localhost:11434`, llama.cpp on
`http://localhost:8080`, or a LAN box on `http://192.168.1.50:11434`), the capsule's runtime HTTP
calls are blocked by the airlock. Both the `/v1/models` describe request (so the model list comes
back empty) and the `/v1/chat/completions` generation request (so prompts fail silently or with a
connection error) are affected. Remote or cloud endpoints (`https://api.openai.com`, Groq, etc.)
are unaffected.

**A subtlety worth knowing:** onboarding can look like it succeeded even when a local endpoint will
fail at runtime. The installer's live model picker fetches `/v1/models` natively -- it runs outside
the sandboxed capsule and does not go through the airlock. So the model select menu populates
correctly during `astrid init`, but every prompt fails once the capsule is live. The fix is an
operator-level exemption (see below), not a retry.

**Granting a local-egress exemption (operator only)**

Exemptions are set in the operator's `config.toml`, under `[security.capsule_local_egress]`. A
capsule's own `Capsule.toml` cannot set this, and a project/workspace config layer cannot widen
it. The default is no exemptions.

```toml
[security.capsule_local_egress]
# host:port (or host:*) endpoints this capsule may reach even though they resolve to a local address
"astrid-capsule-openai-compat" = ["127.0.0.1:1234", "192.168.1.50:11434"]
```

The exemption only lifts the airlock for those specific `host:port` pairs. It does not change the
capsule's `net` allowlist, which is already `*`.

## Selecting a model at runtime

Model selection is per-principal and stored in the registry capsule's KV store. You can change the
active model at any time without touching the capsule configuration.

**CLI:**

```sh
# List all models available across all configured providers
astrid models list

# List with machine-readable output
astrid models list --json

# Show the currently active model for your principal
astrid models current
astrid models current --json

# Select a model by bare id (when unambiguous across providers)
astrid models set gpt-5.4
astrid models set llama3.3:70b

# Disambiguate when two providers serve the same model name
astrid models set astrid-capsule-openai-compat:gpt-5.4

# Clear the active selection (falls back to the auto-selected default)
astrid models unset
```

`astrid models` is a shorthand for `astrid capsule models` -- both reach the same registry
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

{ "id": "gpt-5.4" }
```

All three endpoints are scoped to the authenticated bearer principal.

### What happens when nothing is selected

If no model is selected and no provider is configured, prompts fail with a clear error:

```
No LLM model is selected. Run `astrid models` to choose one, or install/configure an LLM provider.
```

The react capsule never fabricates a default model -- a missing selection always surfaces as an
error, not a silent fallback to an arbitrary model.

## Onboarding during `astrid init`

When running `astrid init` or installing a distro that includes this capsule, the installer
presents all `group = "llm"` capsules as a multi-select ("which provider(s) do you want to set
up?"). For each chosen provider it then runs the onboarding sequence in order:

1. Enter `base_url`
2. Enter `api_key`
3. Pick a model from the live numbered menu (populated from `{base_url}/v1/models`)

This sequence applies to both this capsule and the first-party `openai` capsule. If you are
running a local server, set `base_url` to your local address and leave `api_key` blank. Note that
the SSRF airlock blocks local addresses at runtime by default; you will also need an operator
local-egress exemption before prompts work (see the "Local endpoints and the SSRF airlock" section
above).

## IPC protocol

| Direction | Topic | Payload |
|---|---|---|
| Subscribe | `llm.v1.request.generate.astrid-capsule-openai-compat` | `IpcPayload::LlmRequest` |
| Subscribe | `llm.v1.request.describe` | describe request (registry fan-out) |
| Publish | `llm.v1.stream.astrid-capsule-openai-compat` | `IpcPayload::LlmStreamEvent` |
| Publish | `llm.v1.response.describe` | provider descriptor array |

## Development

```bash
rustup target add wasm32-unknown-unknown
cargo build
```

## License

Dual-licensed under [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE).

Copyright (c) 2025-2026 Joshua J. Bouw and Unicity Labs.
