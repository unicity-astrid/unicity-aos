# astrid-capsule-memory

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![MSRV: 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](https://www.rust-lang.org)

**Cross-session memory for [Unicity AOS](https://github.com/unicity-aos/aos-ce) agents.**

In the OS model, this capsule is the persistent swap file. It carries context across session boundaries so the agent remembers what happened last time.

## How it works

Hooks into `prompt_builder.v1.hook.before_build`. On each prompt assembly cycle:

1. Reads `.unicity-os/memory.md` from the workspace via the VFS
2. Wraps the content in a `# Memory` section
3. Publishes an `appendSystemContext` hook response on the per-request response topic

If the file is missing or empty, the capsule is a no-op.

## Size cap

Agent-written content can grow without limit (unlike human-authored `AGENTS.md`), so a 32KB hard cap prevents unbounded context window consumption. Content beyond the limit is truncated at a UTF-8 character boundary with a `[Memory truncated]` marker.

## Read-only

This capsule handles the read/inject side only. The agent writes to `.unicity-os/memory.md` using existing filesystem tools (`write_file`, `replace_in_file`) from `astrid-capsule-fs`. No new tools needed.

## Development

```bash
cargo build --target wasm32-unknown-unknown --release
```

## License

Dual-licensed under [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE).

Copyright (c) 2025-2026 Joshua J. Bouw and Unicity Labs.
