# Build Your First Astrid Capsule

A capsule is a small WASM module (compiled from Rust) that the kernel loads into
a sandbox and lets expose **tools** to the LLM over an event bus. The kernel is
dumb — it routes events and enforces capabilities; all intelligence lives in
capsules. Your `Capsule.toml`'s `[publish]`/`[subscribe]` keys *are* the
capability ACL the kernel enforces.

## The minimal file set (five files)

### `.cargo/config.toml`
```toml
[build]
target = "wasm32-unknown-unknown"

[target.wasm32-unknown-unknown]
rustflags = ["--cfg=getrandom_backend=\"custom\""]
```

### `rust-toolchain.toml`
```toml
[toolchain]
channel = "1.94.0"
targets = ["wasm32-unknown-unknown"]
components = ["rustfmt", "clippy"]
```

### `Cargo.toml`
```toml
[package]
name = "my-capsule"
version = "0.1.0"
edition = "2024"
publish = false

[lib]
crate-type = ["cdylib"]

[dependencies]
astrid-sdk = { version = "0.7", features = ["derive"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"

[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
strip = true
panic = "abort"
```

### `Capsule.toml`
```toml
[package]
name = "my-capsule"
version = "0.1.0"
description = "What this capsule does"
authors = ["Your Name <you@example.com>"]
astrid-version = ">=0.7.0"

[[component]]
id = "my-capsule"
file = "my_capsule.wasm"      # hyphens -> underscores, + .wasm
type = "executable"

[capabilities]
fs_read = ["home://"]

[publish]
"tool.v1.execute.*.result" = { wit = "@unicity-astrid/wit/types/tool-call-result" }
"tool.v1.response.describe.*" = { wit = "@unicity-astrid/wit/tool/describe-response" }

[subscribe]
"tool.v1.execute.hello" = { wit = "@unicity-astrid/wit/types/tool-call", handler = "tool_execute_hello" }
"tool.v1.request.describe" = { wit = "@unicity-astrid/wit/tool/describe-request", handler = "tool_describe" }
```

### `src/lib.rs`
```rust
#![deny(unsafe_code)]

use astrid_sdk::prelude::*;
use astrid_sdk::schemars;
use serde::Deserialize;

#[derive(Default)]
pub struct MyCapsule;

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct HelloArgs {
    /// Who to greet (shown to the LLM).
    pub name: String,
}

#[capsule]
impl MyCapsule {
    /// Greet someone. This doc-comment becomes the tool description.
    #[astrid::tool("hello")]
    pub fn hello(&self, args: HelloArgs) -> Result<String, SysError> {
        Ok(format!("Hello, {}!", args.name))
    }
}
```

## The build → install → verify loop
```bash
rustup target add wasm32-unknown-unknown        # one time
astrid capsule build                            # -> ./dist/my-capsule.capsule
astrid capsule install ./dist/my-capsule.capsule
astrid capsule list                             # confirm it loaded
astrid status                                   # confirm the daemon is healthy
# then ask the LLM to call `hello`
```

## Top footguns
1. **getrandom flag** in `.cargo/config.toml` is mandatory — without it `uuid`/`HashMap`
   fail to LINK on wasm32-unknown-unknown. #1 silent failure.
2. **`crate-type = ["cdylib"]`** — not bin, not rlib.
3. **No checked-in `wit/`** — the WIT is generated at build time.
4. **Install via `astrid capsule install`**, never hand-copy the `.wasm` — install is
   content-addressed.
5. **A Skill needs an `#[astrid::install]` hook** (`include_str!` + `fs::write`) to land;
   the static engine is a no-op.
6. **No hot-reload** — rebuild and reinstall to iterate.
7. **`tool_describe` must publish, not return** — handled for you by depending on
   `astrid-sdk = "0.7"` (0.7.1+). If a tool is in the manifest but the model can't see it,
   it's almost always a manifest mistake (run `capsule_doctor`), not a kernel race — the old
   describe fan-out race on first prompt is fixed in the current kernel.

Use `astrid capsule new <name>`, or the `scaffold_capsule` tool, to generate all five files
at once, then write them out.
