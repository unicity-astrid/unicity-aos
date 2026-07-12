# astrid-capsule-cli

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![MSRV: 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](https://www.rust-lang.org)

**The CLI proxy for [Astrid OS](https://github.com/unicity-astrid/astrid).**

In the OS model, this capsule is the display server. It bridges the gap between the kernel's IPC event bus and the TUI frontend running on the other side of a Unix domain socket.

## How it works

The capsule binds a Unix socket (path injected by the kernel at boot) and runs a multi-client accept loop:

1. **Subscribes** to TUI-relevant IPC topics: agent responses, stream deltas, onboarding, elicitation, approval, capsule events, registry, and session responses
2. **Accepts** up to 8 concurrent CLI client connections
3. **Reads** from each connected client (50ms timeout per stream) and publishes allowed messages to the IPC bus
4. **Broadcasts** IPC events to all connected clients by pre-serializing each message once, then writing to every stream

### Ingress allowlist

Not all IPC topics are writable from the CLI side. The proxy enforces an explicit allowlist:

- **Exact topics:** `user.v1.prompt`, `cli.v1.command.execute`
- **Prefix-matched:** `astrid.v1.request.*`, `astrid.v1.elicit.response.*`, `astrid.v1.approval.response.*`, `registry.v1.selection.*`, `session.v1.request.*`

Messages to any other topic are dropped with a warning.

### Connection lifecycle

Dead streams are detected during read or broadcast phases and cleaned up with explicit `close()` calls. This releases the host-side `active_streams` entry, preventing slot exhaustion after cumulative disconnects.

## Development

```bash
cargo build --target wasm32-unknown-unknown --release
```

## License

Dual-licensed under [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE).

Copyright (c) 2025-2026 Joshua J. Bouw and Unicity Labs.
