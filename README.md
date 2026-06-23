# astrid-capsule-registry

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![MSRV: 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](https://www.rust-lang.org)

**The LLM provider registry for [Astrid OS](https://github.com/unicity-astrid/astrid).**

In the OS model, this capsule is the device manager. It discovers which LLM provider capsules are loaded, resolves their IPC routing topics, and manages which model is currently active.

## How it works

1. Waits for `astrid.v1.capsules_loaded` from the kernel (all capsules booted)
2. Queries the kernel for capsule metadata via `GetCapsuleMetadata`
3. Resolves provider entries: model ID, description, capsule name, request/stream topics, capabilities
4. Stamps each entry's canonical selection id as `"<capsule>:<model>"`, where `<capsule>` is the publisher authenticated against the kernel-stamped IPC `source_id` (a provider cannot claim a qualifier it does not cryptographically own)
5. Persists the provider list and active model in the capsule KV store
6. Auto-selects a default for a fresh principal — the first discovered capsule's default-hint model (entry[0] of its contribution)

On capsule reload events, the registry re-discovers providers, reconciles a stale active model (remapping an old bare-provider selection to that capsule's default across a single->multi-model upgrade, clearing only when the model is genuinely gone), and auto-selects again if applicable.

## IPC protocol

| Direction | Topic | Description |
|---|---|---|
| Subscribe | `registry.v1.get_providers` | Returns the provider list |
| Subscribe | `registry.v1.get_active_model` | Returns the active provider |
| Subscribe | `registry.v1.set_active_model` | Sets active model by ID |
| Publish | `registry.v1.active_model_changed` | Emitted on model switch |
| Publish | `registry.v1.response.*` | Per-request responses |
| Subscribe | `cli.v1.command.run.registry` | Scriptable `models` verb runs |
| Publish | `cli.v1.command.result.*` | Scriptable verb results, keyed by request id |

## CLI integration

TUI slash command (`cli.v1.command.execute`):
- `/models` - emits a `SelectionRequired` payload for the TUI picker
- `/models <model_id>` - direct model switch

Scriptable verb (`astrid capsule models ...`, over `cli.v1.command.run.registry` with the reply on `cli.v1.command.result.<req_id>`):
- `models list [--json]` - list available models (canonical ids, active marked)
- `models current [--json]` - print the active model id (or `none`)
- `models set <id>` - switch the active model; accepts a bare model name when unambiguous, persists the canonical `"<capsule>:<model>"` form
- `models unset` - clear the active model

Selection ids are matched structurally so ollama-style model names with colons (`ollama:llama3.3:70b`) resolve correctly. A bare name that matches several capsules' models errors with the qualified candidates so you can disambiguate.

## Security

Reload/boot signals are only honoured from the kernel's system session UUID. Provider discovery entries are bound to a `<capsule>` qualifier only after the provider's self-reported routing is authenticated against the kernel-stamped IPC `source_id` (recomputed as `uuid_v5(namespace, candidate)`); a provider cannot publish entries shadowing another capsule's models. Entries that fail authentication, and messages from untrusted sources, are logged and discarded.

## Development

```bash
rustup target add wasm32-unknown-unknown
cargo build --target wasm32-unknown-unknown --release
```

## License

Dual-licensed under [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE).

Copyright (c) 2025-2026 Joshua J. Bouw and Unicity Labs.
