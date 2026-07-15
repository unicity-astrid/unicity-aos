---
name: Capsule Development
description: Build, test, package, install, and debug a current Unicity AOS capsule
---

# Capsule Development

A capsule is a Rust WebAssembly component that runs inside the Astrid Runtime
sandbox shipped by Unicity AOS. The runtime routes IPC, enforces capabilities,
mediates host access, and audits execution. Business logic belongs in capsules.

The manifest is part of the security boundary. Its `[publish]` and `[subscribe]`
tables are the IPC access-control list: a topic that is not declared is denied.
Capsules target `wasm32-unknown-unknown`, not WASI, and reach the host only through
the audited SDK/WIT imports granted by the manifest.

## Start from the supported scaffold

```bash
aos capsule new my-capsule
cd my-capsule
```

The generated project contains:

```text
my-capsule/
├── .cargo/config.toml
├── rust-toolchain.toml
├── Cargo.toml
├── Capsule.toml
└── src/lib.rs
```

Do not begin from an old `[[interceptor]]` manifest or an `ipc_publish` /
`ipc_subscribe` capability array. Those formats are obsolete.

## Compiler target

`.cargo/config.toml` selects the capsule target and activates the SDK's custom
randomness backend:

```toml
[build]
target = "wasm32-unknown-unknown"

[target.wasm32-unknown-unknown]
rustflags = ["--cfg=getrandom_backend=\"custom\""]
```

The `getrandom_backend` setting must live in the final capsule crate. Without it,
dependencies such as UUID generators or randomized hash maps can fail to link.

Pin the toolchain and target in `rust-toolchain.toml`:

```toml
[toolchain]
channel = "1.94.0"
targets = ["wasm32-unknown-unknown"]
components = ["rustfmt", "clippy"]
```

## Cargo package

Use a `cdylib` and the current 0.7 SDK surface:

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

## Capsule manifest

This minimal tool capsule exposes one `hello` tool:

```toml
[package]
name = "my-capsule"
version = "0.1.0"
description = "A small example capsule"
authors = ["Your Name <you@example.com>"]
astrid-version = ">=0.7.0"

[[component]]
id = "my-capsule"
file = "my_capsule.wasm"
type = "executable"

[capabilities]
fs_read = ["home://data/my-capsule/"]
fs_write = ["home://data/my-capsule/"]

[publish]
"tool.v1.execute.*.result" = { wit = "@unicity-astrid/wit/types/tool-call-result" }
"tool.v1.response.describe.*" = { wit = "@unicity-astrid/wit/tool/describe-response" }

[subscribe]
"tool.v1.execute.hello" = { wit = "@unicity-astrid/wit/types/tool-call", handler = "tool_execute_hello" }
"tool.v1.request.describe" = { wit = "@unicity-astrid/wit/tool/describe-request", handler = "tool_describe" }
```

The published `@unicity-astrid/wit/...` strings and `astrid:*` WIT namespaces
are stable runtime identifiers. Keep them exact even though the product CLI is
`aos`.

For a tool capsule, both publish entries are required: one returns tool results,
and the other answers the describe fan-out that makes tools visible to agents.
Every tool needs its own concrete execute subscription. A subscribe wildcard may
have one trailing `*`; publish permissions should be as narrow as practical.

Manifest data is untrusted input. Do not put operator authority, principal
identity, or a secret scope in a capsule-controlled field. The kernel stamps the
caller identity and decides operator-only policy.

## Implement a tool

```rust
#![deny(unsafe_code)]
#![deny(clippy::all)]

use astrid_sdk::prelude::*;
use astrid_sdk::schemars;
use serde::Deserialize;

#[derive(Default)]
pub struct MyCapsule;

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct HelloArgs {
    /// Person to greet.
    pub name: String,
}

#[capsule]
impl MyCapsule {
    /// Greet a person by name.
    #[astrid::tool("hello")]
    pub fn hello(&self, args: HelloArgs) -> Result<String, SysError> {
        Ok(format!("Hello, {}!", args.name))
    }
}
```

The tool macro derives the input schema from `JsonSchema`, uses the method's doc
comment as the tool description, exports `tool_execute_hello`, and contributes
the generated `tool_describe` handler. The manifest binds those generated export
names to their IPC topics.

Use `#[astrid::interceptor("handler_name")]` only when you need a raw handler;
bind that handler from a `[subscribe]` entry. The obsolete manifest-level
`[[interceptor]]` table must not be used.

## State and lifecycle

Use `#[capsule(state)]` when mutable capsule state must be persisted by the
runtime. State is scoped to the capsule and principal; never cache per-principal
configuration in a process-global static.

Lifecycle hooks are explicit:

- `#[astrid::install]` runs on first install and may create initial VFS data or
  elicit configuration.
- `#[astrid::upgrade]` migrates state when an installed version is replaced.
- `#[astrid::run]` owns a long-running loop and must call
  `runtime::signal_ready()` after initialization.

An install hook returns `Result<(), SysError>`. An upgrade hook also receives the
previous version. Keep migrations idempotent and fail closed before partially
rewriting persisted state.

## IPC

Use typed JSON helpers when the contract is JSON:

```rust
ipc::publish_json("my.v1.event.ready", &payload)?;
let subscription = ipc::subscribe("my.v1.request.*")?;
let batch = subscription.recv(500)?;
for message in batch.messages {
    // Parse and handle each envelope independently.
}
```

`recv(timeout)` returns `Ok` with an empty message batch on timeout. It does not
return a timeout error. A received batch can contain multiple publishers; for a
sensitive operation, read the kernel-stamped principal from each envelope and
require verified attribution. Never trust a principal copied into the payload.

Fan-out responses must be published on their declared response topics. Returning
a value from one interceptor does not publish it to other subscribers.

## Runtime-mediated host access

Host access is denied unless the manifest and runtime grant it.

- `fs` accepts VFS paths such as `home://` and `cwd://`; it does not expose raw
  host paths.
- `kv` is automatically scoped by capsule and principal.
- `http` performs outbound requests only when the capsule has the required HTTP
  authority.
- `env::var` resolves ordinary configuration or an owning capsule's secret at
  call time. Secrets are not ordinary env JSON and must never be cached across
  principals.
- `log` emits structured capsule logs under
  `~/.unicity-os/runtime/home/<principal>/.local/log/<capsule>/`.

Ask only for the narrow paths, hosts, topics, and processes the capsule needs.
Do not add broad authority merely to make a failing test pass.

## Build, package, and install

```bash
rustup target add wasm32-unknown-unknown
aos capsule build
aos capsule install ./dist/my-capsule.capsule
aos capsule list
aos status
```

`aos capsule build` packages the component and manifest into an installable,
content-addressed `dist/*.capsule` artifact. Plain `cargo build --release` is a
useful compile check, but its raw `.wasm` output is not installable. Do not copy a
raw WASM file into the runtime home.

There is no hot reload. Rebuild and reinstall the `.capsule` artifact for each
iteration. Installation replaces the previous installed version while preserving
configuration unless removal is explicitly purged:

```bash
aos capsule remove my-capsule
aos capsule remove my-capsule --purge
```

## Test before installation

Keep parsing, validation, and business rules in ordinary Rust functions so they
can run in host-target unit tests. Separately validate the guest surface:

```bash
cargo fmt --all -- --check
cargo check
aos capsule build
```

Then install into an isolated AOS home and test the real boundary:

1. Confirm the capsule appears in `aos capsule list`.
2. Confirm `aos status` remains healthy.
3. Exercise every declared tool with valid and invalid input.
4. Prove undeclared IPC topics and host operations are denied.
5. Test a second principal so configuration and state cannot bleed across callers.
6. Revoke access and prove the next invocation observes the revocation.
7. Reinstall and upgrade to prove state migration and configuration retention.

## Common failures

| Symptom | Check |
|---|---|
| Tool is absent | Declare the execute and describe subscriptions plus both result/describe publish topics. |
| Linker fails around randomness | Restore the custom `getrandom_backend` rustflag in `.cargo/config.toml`. |
| IPC publish or subscribe is denied | Add the exact topic to `[publish]` or `[subscribe]`; do not add an unrelated wildcard. |
| Build produced only `.wasm` | Run `aos capsule build` and install the artifact from `dist/`. |
| Request times out without an error | Treat an empty `recv` batch as timeout. |
| One user's configuration appears for another | Remove static/global env caching and resolve per invocation. |
| Capsule panics or exits | Inspect the per-principal capsule log and remove `unwrap`/`expect` from guest error paths. |
| Upgrade corrupts state | Make the migration idempotent and write the new state only after validation succeeds. |

For a generated, code-grounded starting point, prefer `aos capsule new` and the
capsule-forge tooling over copied examples from older releases.
