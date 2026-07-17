# aos-session

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![MSRV: 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](https://www.rust-lang.org)

**The conversation session store for [Unicity AOS](https://github.com/unicity-aos/aos-ce).**

In the OS model, this capsule is the filesystem for conversations. Dumb, trustworthy, append-only. It holds clean messages: what the user said, what the assistant replied, what tools returned. It never transforms anything. Clean in, clean out.

## Operations

**`session.append`** - Appends messages to conversation history. Fire-and-forget.

**`session.request.get_messages`** - Returns conversation history via a scoped reply topic. Supports `append_before_read` for atomic append-then-fetch (eliminates the race between separate append and get calls).

**`session.v1.request.clear`** - Creates a new session with `parent_session_id` pointing to the old one. The old session's data stays intact in KV. History is never silently truncated.

## Session chaining

Sessions form a linked list. When a session is cleared or compacted, a new session is created pointing back to the old one. You can walk the chain to reconstruct the full conversation history across compaction boundaries.

## Schema versioning

Session data is schema-versioned for forward-compatible deserialization:
- **v0** (legacy, missing field): stamps to v1 and re-saves
- **v1** (current): used as-is
- **Unknown future version**: starts fresh (fail secure)

## Concurrency safety

Reply topics are scoped per-request: `session.v1.response.{operation}.{correlation_id}`. Correlation IDs must be non-empty with no dots (prevents extra topic segments that could break ACL pattern matching). This prevents cross-instance response theft under concurrent load.

Session isolation is enforced at the kernel's topic ACL layer, not within this capsule.

## Development

```bash
rustup target add wasm32-unknown-unknown
cargo build --target wasm32-unknown-unknown --release
cargo test
```

## License

Dual-licensed under [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE).

Copyright (c) 2025-2026 Joshua J. Bouw and Unicity Labs.
