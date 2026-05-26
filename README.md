# astrid-capsule-identity

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![MSRV: 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](https://www.rust-lang.org)

**The system prompt builder for [Astrid OS](https://github.com/unicity-astrid/astrid).**

In the OS model, this capsule is `/etc/profile`. It reads workspace configuration and assembles the agent's identity, environment context, and tool usage guidelines into a system prompt.

## How it works

On an `identity.v1.request.build` event:

1. Reads the spark identity from `spark.toml` (callsign, class, aura, signal, core directives) or falls back to a default "You are Astrid" preamble
2. Adds environment context (working directory, platform)
3. Appends tool usage guidelines (file operations, search, execution, general principles)
4. Reads project instructions from `AGENTS.md` (or `ASTRID.md` as fallback)
5. Reads workspace bounds from `.astridignore`
6. Publishes the assembled prompt on `identity.v1.response.ready`

Stateless. Reads workspace files fresh on every request. Session ID is echoed back for react loop correlation.

## IPC protocol

| Direction | Topic |
|---|---|
| Subscribe | `identity.v1.request.build` |
| Publish | `identity.v1.response.ready` |

## Development

```bash
cargo build --target wasm32-unknown-unknown --release
```

## License

Dual-licensed under [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE).

Copyright (c) 2025-2026 Joshua J. Bouw and Unicity Labs.
