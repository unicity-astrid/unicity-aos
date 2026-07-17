# aos-identity

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![MSRV: 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](https://www.rust-lang.org)

**The identity and system prompt builder for [Unicity AOS](https://github.com/unicity-aos/aos-ce).**

In the OS model, this capsule is `/etc/profile`. It owns the agent's spark identity, persists it as capsule state, and assembles the identity plus environment context into a system prompt.

## How it works

On a `spark.v1.request.build` event:

1. Loads the persisted spark identity from capsule KV state.
2. Auto-detects `home://.config/spark.toml` if KV state is empty.
3. Adds environment context (working directory, platform).
4. Publishes the assembled prompt on `spark.v1.response.ready`.

If no identity exists yet, the prompt includes onboarding instructions. After onboarding, the LLM calls `save_identity`, which saves the chosen callsign, class, aura, signal, and core directives to capsule state and writes `home://.config/spark.toml` as a recovery copy.

State is scoped by the runtime's capsule KV isolation for the calling principal. Session ID is echoed back for react loop correlation.

## IPC protocol

| Direction | Topic |
|---|---|
| Subscribe | `spark.v1.request.build` |
| Subscribe | `tool.v1.execute.save_identity` |
| Subscribe | `tool.v1.request.describe` |
| Subscribe | `cli.v1.command.execute` |
| Publish | `spark.v1.response.ready` |
| Publish | `tool.v1.execute.*.result` |
| Publish | `tool.v1.response.describe.*` |
| Publish | `agent.v1.response` |

## Development

```bash
cargo build
cargo test
```

## License

Dual-licensed under [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE).

Copyright (c) 2025-2026 Joshua J. Bouw and Unicity Labs.
