# astrid-capsule-system

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![MSRV: 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](https://www.rust-lang.org)

**System management tools for [Unicity AOS](https://github.com/unicity-aos/aos-ce) agents.**

This capsule gives the LLM typed tools to inspect and manage its own runtime. It makes Unicity AOS self-inspecting: the agent can inspect installed capsules, read interface contracts, and understand the health of its own system.

## Tools

| Tool | Description |
|---|---|
| `list_capsules` | List installed capsules with names and versions (use `inspect_capsule` for exports, imports, capabilities) |
| `inspect_capsule` | Read a capsule's Capsule.toml manifest and meta.json metadata |
| `list_interfaces` | List available WIT interface definitions |
| `read_interface` | Read a WIT interface definition (typed contract between capsules) |
| `system_status` | Runtime health: capsule count, interface coverage, unsatisfied imports |

## How it works

All operations go through the kernel's VFS and capability system. The capsule reads from `home://capsules/` to discover installed capsules and from `home://wit/astrid/` to read interface definitions. It cannot bypass sandbox boundaries.

The LLM uses these tools to understand its own runtime before making changes. A typical flow:

1. `list_capsules` -- see what's installed
2. `read_interface session` -- understand the session interface contract
3. `inspect_capsule astrid-capsule-session` -- read the current implementation
4. Build and install a replacement (via shell or future system tools)

## Security

- Read-only VFS access (`fs_read = ["home://"]`)
- Path traversal rejected on all inputs
- No capability to modify capsules directly -- write operations require separate tools with approval gates

## Development

```bash
cargo build --target wasm32-unknown-unknown --release
```

## License

Dual-licensed under [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE).

Copyright (c) 2025-2026 Joshua J. Bouw and Unicity Labs.
