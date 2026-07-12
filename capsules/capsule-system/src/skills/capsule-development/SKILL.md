---
name: Capsule Development
description: How to build, configure, and install Astrid capsules from scratch
---

# Capsule Development

An Astrid capsule is a WebAssembly (WASM) module compiled from Rust that runs inside the Astrid kernel sandbox. Capsules communicate exclusively via IPC events — they have no direct access to the host system except through kernel-mediated host functions.

## Project Layout

```
my-capsule/
├── Capsule.toml       # Manifest (name, version, capabilities, interceptors)
├── Cargo.toml         # Rust crate (lib crate, cdylib)
└── src/
    └── lib.rs         # Capsule logic
```

## Capsule.toml

```toml
[package]
name = "my-capsule"
version = "0.1.0"
description = "Short description of what this capsule does"
authors = ["Your Name"]
astrid-version = ">=0.5.0"

[[component]]
id = "my-capsule"
file = "my_capsule.wasm"   # underscores, not hyphens
type = "executable"

[capabilities]
# Filesystem access (read and/or write)
fs_read  = ["home://data/"]
fs_write = ["home://data/"]

# IPC topics this capsule publishes to
ipc_publish = ["my.namespace.events.*"]

# IPC topics this capsule subscribes to (must match interceptor events)
ipc_subscribe = ["my.namespace.request.*"]

# Spawn host processes (list allowed binary names)
host_process = ["git", "cargo"]

# Allow outbound HTTP requests
allow_http = true

# Allow outbound network connections
allow_network = true

[imports]
# Interfaces this capsule requires from others
astrid = { session = "^1.0" }

[exports]
# Interfaces this capsule provides to others
my-namespace = { "my-interface" = "1.0.0" }

[[interceptor]]
event  = "my.namespace.request.do-thing"
action = "tool_execute_do_thing"      # must match generated arm name

[[interceptor]]
event    = "system.v1.lifecycle.capsule_loaded"
action   = "on_capsule_loaded"
priority = 50   # lower fires first; default 100
```

## Cargo.toml

```toml
[package]
name = "my-capsule"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
astrid-sdk  = "0.5.0"
serde       = { version = "1", features = ["derive"] }
serde_json  = "1"
uuid        = { version = "1", features = ["v4"] }
```

## lib.rs — Skeleton

```rust
#![deny(unsafe_code)]
#![deny(clippy::all)]

use astrid_sdk::prelude::*;
use astrid_sdk::schemars;
use serde::{Deserialize, Serialize};

#[derive(Default)]
pub struct MyCapsule;

#[capsule]
impl MyCapsule {
    /// Run on first install. Set up VFS directories, write config, etc.
    #[astrid::install]
    pub fn on_install(&self) -> Result<(), SysError> {
        astrid_sdk::fs::create_dir("home://data/my-capsule")?;
        Ok(())
    }

    /// Run when capsule is upgraded to a new version.
    #[astrid::upgrade]
    pub fn on_upgrade(&self) -> Result<(), SysError> {
        Ok(())
    }

    /// Handle an IPC event. The action name must match the `action` field
    /// in Capsule.toml. InterceptResult controls the middleware chain:
    ///   Continue(payload) — next interceptor sees the (optionally mutated) payload
    ///   Final(payload)    — chain stops, payload is the final result
    ///   Deny { reason }   — chain stops, event is rejected
    #[astrid::interceptor("on_some_event")]
    pub fn on_some_event(&self, payload: Vec<u8>) -> Result<InterceptResult, SysError> {
        let _msg: serde_json::Value = serde_json::from_slice(&payload)?;
        Ok(InterceptResult::Continue(payload))
    }
}
```

## SDK Macros

### `#[astrid::tool("tool_name")]`

Sugar for `#[astrid::interceptor]` with tool IPC conventions baked in. Generates:
- `tool_execute_<tool_name>` — handles `tool.v1.execute.<tool_name>`, publishes result to `tool.v1.execute.<tool_name>.result`
- `tool_describe` — handles `tool.v1.request.describe`, returns all tool JSON schemas

The function receives a strongly-typed args struct and returns `Result<String, SysError>`:

```rust
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct MyArgs {
    /// The thing to process (shown to the LLM as the parameter description)
    pub input: String,
}

#[astrid::tool("my_tool")]
pub fn my_tool(&self, args: MyArgs) -> Result<String, SysError> {
    Ok(format!("processed: {}", args.input))
}
```

Add the Capsule.toml interceptors:
```toml
[[interceptor]]
event = "tool.v1.execute.my_tool"
action = "tool_execute_my_tool"

[[interceptor]]
event = "tool.v1.request.describe"
action = "tool_describe"
```

And capabilities:
```toml
ipc_publish   = ["tool.v1.execute.*.result", "tool.v1.response.describe.*"]
ipc_subscribe = ["tool.v1.execute.my_tool", "tool.v1.request.describe"]
```

### `#[astrid::interceptor("action_name")]`

Raw event handler. Action name matches the `action` field in `[[interceptor]]`. Receives raw bytes, returns `InterceptResult`:

```rust
#[astrid::interceptor("my_handler")]
pub fn my_handler(&self, payload: Vec<u8>) -> Result<InterceptResult, SysError> {
    Ok(InterceptResult::Final(b"done".to_vec()))
}
```

### `#[astrid::install]` / `#[astrid::upgrade]`

Lifecycle hooks. `install` runs once on `astrid capsule install`. `upgrade` runs on version update. Both have signature `fn(&self) -> Result<(), SysError>`.

## IPC Patterns

### Fire and forget

```rust
use astrid_sdk::ipc;
ipc::publish("my.namespace.event", b"{\"key\":\"value\"}")?;
```

### Request / response (correlation ID)

```rust
use astrid_sdk::ipc;
use uuid::Uuid;

let correlation_id = Uuid::new_v4().to_string();
let response_topic = format!("my.namespace.response.{correlation_id}");

ipc::subscribe(&response_topic)?;
ipc::publish("my.namespace.request", serde_json::to_vec(&serde_json::json!({
    "response_topic": response_topic,
    "data": "..."
}))?)?;

// Poll for response (500ms timeout)
let response = ipc::recv_bytes(&response_topic, 500)?;
ipc::unsubscribe(&response_topic)?;
```

### Trigger hook (fan-out, collect all responses)

Used to broadcast to all interceptors on a topic and collect their payloads:

```rust
use astrid_sdk::hooks;
let results: Vec<Vec<u8>> = hooks::trigger("tool.v1.request.describe", b"")?;
```

## VFS

All file paths are UTF-8 strings. Scheme `home://` maps to the calling principal's home directory.

```rust
use astrid_sdk::fs;

// Read
let content = fs::read_to_string("home://data/config.json")?;

// Write (requires fs_write capability)
fs::write("home://data/output.txt", b"hello")?;

// Directory
fs::create_dir("home://data/my-dir")?;
let entries = fs::read_dir("home://data/")?;
for entry in entries {
    println!("{}", entry.file_name());
}

// Check existence
if fs::exists("home://data/file.txt")? { ... }
```

## KV Store

Per-capsule, per-principal key-value store. Scoped automatically by the kernel.

```rust
use astrid_sdk::kv;

kv::set("my-key", b"value")?;
let val = kv::get("my-key")?;   // Option<Vec<u8>>
kv::delete("my-key")?;
let keys = kv::list_keys("prefix:")?;
```

## Logging

```rust
use astrid_sdk::log;

log::info("capsule started")?;
log::warn("something unusual")?;
log::error("something failed")?;
// Or with format:
log::info(format!("processed {} items", count))?;
```

## Reading WIT Interfaces at Runtime

WIT files are installed to `home://wit/` during `astrid init`. Use them to understand message schemas:

```rust
let session_wit = astrid_sdk::fs::read_to_string("home://wit/session.wit")?;
```

Available interfaces: `session.wit`, `tool.wit`, `llm.wit`, `prompt.wit`, `context.wit`, `hook.wit`, `registry.wit`, `spark.wit`, `types.wit`.

## Build

```bash
cargo build --target wasm32-unknown-unknown --release
# Output: target/wasm32-unknown-unknown/release/my_capsule.wasm
```

Requires the target:
```bash
rustup target add wasm32-unknown-unknown
```

## Install

Use the `install_capsule` system tool, or from the CLI:

```bash
astrid capsule install ./path/to/capsule
astrid capsule install @github-org/capsule-repo
```

## Common Errors

| Error | Fix |
|---|---|
| `capability denied: fs_write` | Add `fs_write = ["home://..."]` to `[capabilities]` |
| `ipc: topic not subscribed` | Add topic to `ipc_subscribe` in `[capabilities]` |
| `interceptor action not found` | Check `action` in `[[interceptor]]` matches generated arm name |
| `wasm trap: unreachable` | Panic in guest — check for unwrap/expect on error paths |
| `boot validation: unsatisfied import` | Install the capsule that exports the required interface |
