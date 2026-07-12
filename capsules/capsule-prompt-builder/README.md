# astrid-capsule-prompt-builder

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![MSRV: 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](https://www.rust-lang.org)

**The prompt assembly pipeline for [Astrid OS](https://github.com/unicity-astrid/astrid).**

In the OS model, this capsule is the linker. It takes contributions from multiple plugin capsules and merges them into a single, final prompt that the LLM actually sees.

## How it works

On a `prompt_builder.v1.assemble` request:

1. Fires `prompt_builder.v1.hook.before_build` to all plugin capsules with the current messages, system prompt, model, and provider
2. Collects plugin responses within the timeout (default 2000ms, max 50 responses)
3. Filters responses by permission: capsules without `allow_prompt_injection` capability have system-prompt-mutating fields stripped
4. Merges responses using OpenClaw-compatible semantics
5. Publishes the assembled result on `prompt_builder.v1.response.assemble`
6. Fires `prompt_builder.v1.hook.after_build` as a notification

## Merge semantics

Four hook response fields, merged in this order:

1. **`prependContext`** - Concatenated in order. Becomes a user-visible context prefix.
2. **`systemPrompt`** - Last non-null value wins. Full override of the base system prompt.
3. **`prependSystemContext`** - Concatenated in order, prepended to the (possibly overridden) system prompt.
4. **`appendSystemContext`** - Concatenated in order, appended to the system prompt.

## Permission gating

Capsules without the `allow_prompt_injection` capability retain only `prependContext` (user-visible context). The `systemPrompt`, `prependSystemContext`, and `appendSystemContext` fields are stripped. Capability checks are cached per-capsule-UUID within a single assembly cycle.

## Configuration

| Key | Default | Description |
|---|---|---|
| `hook_timeout_ms` | `2000` | Max time to wait for plugin hook responses |

## Development

```bash
rustup target add wasm32-unknown-unknown
cargo build --target wasm32-unknown-unknown --release
cargo test
```

## License

Dual-licensed under [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE).

Copyright (c) 2025-2026 Joshua J. Bouw and Unicity Labs.
