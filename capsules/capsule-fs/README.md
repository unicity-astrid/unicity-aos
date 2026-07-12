# astrid-capsule-fs

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![MSRV: 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](https://www.rust-lang.org)

**Filesystem tools for [Astrid OS](https://github.com/unicity-astrid/astrid) agents.**

In the OS model, this capsule is the coreutils package. It gives agents the ability to read, write, search, and navigate the workspace filesystem through the kernel's VFS airlock.

## Tools

| Tool | Description |
|---|---|
| `read_file` | Read file contents with optional line range (`start_line`, `end_line`) |
| `write_file` | Write content to a file |
| `replace_in_file` | Replace an exact string match in a file (rejects 0 or >1 occurrences) |
| `list_directory` | List entries in a directory |
| `grep_search` | Recursive content search with depth, file count, and match count caps |
| `create_directory` | Create a directory |
| `delete_file` | Delete a file (session-created files only, no whiteout support yet) |
| `move_file` | Move a file with 10MB size limit, existence checks, and rollback on failure |

All operations go through the VFS airlock. The kernel enforces path boundaries, copy-on-write isolation, and capability checks before any host filesystem access occurs.

## Development

```bash
cargo build --target wasm32-unknown-unknown --release
```

## License

Dual-licensed under [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE).

Copyright (c) 2025-2026 Joshua J. Bouw and Unicity Labs.
