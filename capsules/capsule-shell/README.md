# astrid-capsule-shell

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![MSRV: 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](https://www.rust-lang.org)

**Shell execution tools for [Astrid OS](https://github.com/unicity-astrid/astrid) agents.**

In the OS model, this capsule is the process spawner with a built-in safety net. It gives agents shell access while blocking catastrophic commands before they ever reach the human approval prompt.

## Tools

| Tool | Description |
|---|---|
| `run_shell_command` | Execute a shell command (blocks until complete) |
| `spawn_background_process` | Start a long-running process |
| `read_process_logs` | Read buffered stdout/stderr from a background process |
| `kill_process` | Terminate a background process and collect final output |

All commands execute via the host's Process Airlock (Seatbelt on macOS, bwrap on Linux).

## Approval system

Before execution, the capsule extracts an approval action from the command. Known CLIs (git, cargo, docker, kubectl, npm, and 30+ others) extract program + subcommand at a safe depth:

```text
git push --force origin main     -> "git push"
docker compose up -d             -> "docker compose up"
kubectl config set-context --cur -> "kubectl config set-context"
```

Unknown programs use the full command string as the action to prevent dangerously broad session allowances. `rm -rf /tmp/foo` becomes the exact action `rm -rf /tmp/foo`, not `rm`.

## Catastrophic command blocking

These are hard-denied before the approval prompt:

- **Fork bombs:** `:(){ :|:& };:`
- **Blocked programs:** `mkfs`, `shutdown`, `reboot`, `halt`, `poweroff`, `init`
- **Block device writes:** `dd if=/dev/zero of=/dev/sda`
- **Recursive permission changes on system paths:** `chmod -R 777 /`
- **rm on protected paths:** `/`, `/etc`, `/usr`, `/home`, `/System`, `/Library`, and more
- **Chained attacks:** `ls && rm -rf /` - each sub-command in `&&`, `||`, `;`, `|` chains is checked independently

Workspace-relative and `/tmp` paths are always allowed through to the approval prompt.

## Development

```bash
rustup target add wasm32-unknown-unknown
cargo build --target wasm32-unknown-unknown --release
cargo test
```

## License

Dual-licensed under [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE).

Copyright (c) 2025-2026 Joshua J. Bouw and Unicity Labs.
