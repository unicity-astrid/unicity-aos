# aos-hook-bridge

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![MSRV: 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](https://www.rust-lang.org)

**The lifecycle-to-hook mapper for [Unicity AOS](https://github.com/unicity-aos/aos-ce).**

In the OS model, this capsule is the interrupt dispatcher. The kernel emits raw lifecycle events. This capsule translates each one to a named semantic hook, fans out to subscriber capsules via `hooks::trigger`, and applies a merge strategy to the collected responses.

This is a **policy** capsule. It owns the event-to-hook mapping table and the merge rules. The fan-out mechanism lives in the kernel's `astrid_trigger_hook` host function.

## Event mappings

### Session

| Lifecycle Event | Hook Name | Merge |
|---|---|---|
| `session_created` | `session_start` | None |
| `session_ended` | `session_end` | None |

### Tool

| Lifecycle Event | Hook Name | Merge |
|---|---|---|
| `tool_call_started` | `before_tool_call` | ToolCallBefore |
| `tool_call_completed` | `after_tool_call` | LastNonNull(`modified_result`) |
| `tool_result_persisting` | `tool_result_persist` | LastNonNull(`transformed_result`) |

### Message

| Lifecycle Event | Hook Name | Merge |
|---|---|---|
| `message_received` | `message_received` | None |
| `message_sending` | `message_sending` | LastNonNull(`modified_content`) |
| `message_sent` | `message_sent` | None |

### Sub-agent

| Lifecycle Event | Hook Name | Merge |
|---|---|---|
| `sub_agent_spawned` | `subagent_start` | None |
| `sub_agent_completed/failed/cancelled` | `subagent_stop` | None |

### Context and kernel

| Lifecycle Event | Hook Name | Merge |
|---|---|---|
| `context_compaction_started` | `on_compaction_started` | None |
| `context_compaction_completed` | `on_compaction_completed` | None |
| `kernel_started` | `kernel_start` | None |
| `kernel_shutdown` | `kernel_stop` | None |

## Merge strategies

**None** - Fire-and-forget. Subscriber responses are discarded. Used for observation-only hooks.

**ToolCallBefore** - Any `skip: true` wins. Last non-null `modified_params` wins. Lets any subscriber block a tool call, and lets the last subscriber to touch the params have final say.

**LastNonNull** - Last non-null value for a named field wins. Used for `after_tool_call`, `tool_result_persist`, and `message_sending`.

## Development

```bash
cargo build --target wasm32-unknown-unknown --release
```

## License

Dual-licensed under [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE).

Copyright (c) 2025-2026 Joshua J. Bouw and Unicity Labs.
