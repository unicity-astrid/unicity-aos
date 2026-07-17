# aos-context-engine

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![MSRV: 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](https://www.rust-lang.org)

**The context window compaction engine for [Unicity AOS](https://github.com/unicity-aos/aos-ce).**

In the OS model, this capsule is the memory manager. When the conversation grows beyond the LLM's context window, this capsule decides what stays and what gets trimmed.

## How it works

On a `context_engine.v1.compact` request:

1. Fires `context_engine.v1.hook.before_compaction` to all plugin capsules via IPC
2. Collects plugin responses within the timeout window (default 2000ms, max 50 responses)
3. Merges responses: any `skip: true` wins, pinned message IDs are unioned across all plugins
4. Runs `summarize_and_truncate` on the messages, respecting pinned IDs and the `keep_recent` setting
5. Fires `context_engine.v1.hook.after_compaction` as a notification (fire-and-forget)
6. Publishes the compacted result on `context_engine.v1.response.compact`

Also handles `context_engine.v1.estimate_tokens` for token count estimation.

## Configuration

| Key | Default | Description |
|---|---|---|
| `hook_timeout_ms` | `2000` | Max time to wait for plugin hook responses |
| `keep_recent` | `10` | Number of recent turns always preserved during compaction |

## IPC protocol

| Direction | Topic |
|---|---|
| Subscribe | `context_engine.v1.compact` |
| Subscribe | `context_engine.v1.estimate_tokens` |
| Publish | `context_engine.v1.response.compact` |
| Publish | `context_engine.v1.response.estimate_tokens` |
| Publish | `context_engine.v1.hook.before_compaction` |
| Publish | `context_engine.v1.hook.after_compaction` |

## Development

```bash
cargo build --target wasm32-unknown-unknown --release
cargo test
```

## License

Dual-licensed under [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE).

Copyright (c) 2025-2026 Joshua J. Bouw and Unicity Labs.
