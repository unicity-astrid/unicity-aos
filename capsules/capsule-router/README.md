# astrid-capsule-router

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![MSRV: 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](https://www.rust-lang.org)

**The tool execution router for [Astrid OS](https://github.com/unicity-astrid/astrid).**

In the OS model, this capsule is the syscall dispatcher. It sits between the react loop and every tool capsule, validating requests and routing them to the correct IPC topic.

## How it works

1. Receives `tool.request.execute` events from the react loop
2. Validates the tool name: must be non-empty, alphanumeric with hyphens, underscores, or colons. Dots are rejected to prevent topic injection.
3. Forwards valid requests to `tool.v1.execute.{tool_name}`
4. Routes execution results back to `tool.v1.execute.result`

Returns error results for invalid tool names or failed publishes. Completely stateless - pure routing middleware.

## IPC protocol

| Direction | Topic | Description |
|---|---|---|
| Subscribe | `tool.request.execute` | Incoming tool requests from react loop |
| Publish | `tool.v1.execute.{tool_name}` | Forwarded to specific tool capsule |
| Subscribe | `tool.v1.execute.*` (results) | Results from tool capsules |
| Publish | `tool.v1.execute.result` | Routed back to react loop |

## Development

```bash
rustup target add wasm32-unknown-unknown
cargo build --target wasm32-unknown-unknown --release
```

## License

Dual-licensed under [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE).

Copyright (c) 2025-2026 Joshua J. Bouw and Unicity Labs.
