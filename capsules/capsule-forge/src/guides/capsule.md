# Capsule anatomy, Rust SDK, lifecycle, and state

## Minimal project

Use `aos capsule new <name>` or `scaffold_capsule` rather than reconstructing
boilerplate from memory. A standard Rust tool capsule contains:

```text
<name>/
|-- .cargo/config.toml
|-- rust-toolchain.toml
|-- Cargo.toml
|-- Capsule.toml
`-- src/lib.rs
```

The Cargo crate is a `cdylib`, targets `wasm32-unknown-unknown`, and depends on
`astrid-sdk` with the derive feature. The final crate's `.cargo/config.toml`
must select the WASM target and set the custom getrandom backend; a dependency
cannot set that flag on behalf of the final capsule.

Do not check generated WIT staging into an ordinary tool capsule. Use
`aos capsule build` to create the installable `dist/*.capsule`; raw Cargo WASM
output is only a compile artifact.

## Smallest tool implementation

```rust
#![deny(unsafe_code)]

use astrid_sdk::prelude::*;
use astrid_sdk::schemars;
use serde::Deserialize;

#[derive(Default)]
pub struct Capsule;

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct HelloArgs {
    /// Who to greet. This is visible in the model's parameter schema.
    pub name: String,
}

#[capsule]
impl Capsule {
    /// Greet someone. This comment becomes the tool description.
    #[astrid::tool("hello")]
    pub fn hello(&self, args: HelloArgs) -> Result<String, SysError> {
        Ok(format!("Hello, {}!", args.name))
    }
}
```

The struct must implement `Default`. Tool arguments implement
`Deserialize + JsonSchema`; return values implement `Serialize`; errors
implement `Display`. Prefer `SysError` at the host boundary.

## Macro surface

`#[capsule]` belongs on the implementation block. It generates the component
exports, dispatch wiring, panic hook, and tool description support.

Tool forms:

```rust
#[astrid::tool("name")]
#[astrid::tool("name", mutable)]
#[astrid::tool(mutable)]
#[astrid::tool]
```

An explicit or inferred tool `foo` binds to handler `tool_execute_foo`. The
`mutable` marker describes the effect to approval/UI layers; routing still
comes from the manifest.

Other handler macros:

- `#[astrid::install]`: install lifecycle hook.
- `#[astrid::upgrade]`: upgrade hook receiving the prior version.
- `#[astrid::run]`: long-lived run loop; signal readiness after initialization.
- `#[astrid::interceptor("topic")]`: raw event middleware returning
  `Continue`, `Final`, or `Deny`.
- `#[astrid::command("name")]`: command handler not exposed as an LLM tool.

Each lifecycle hook is a singleton. Keep install and upgrade hooks narrow and
idempotent. Skills are an agent user-space protocol, not a capsule manifest
section; do not use a lifecycle hook merely to smuggle host-plugin instructions
through the runtime.

## Stateful mode

Use `#[capsule(state)]` or a mutable handler receiver when the capsule struct is
the state model. The generated path loads `__state` from principal-scoped KV
before a handler and persists it after a successful handler. Failed handlers do
not save partial state.

Install starts from `Default`; upgrade loads prior state when available. A run
loop loads once and is not automatically saved after every iteration, so write
explicit checkpoints for long-lived state.

Stateless `&self` capsules use an in-memory singleton and avoid KV.

## Principal scope

Caller identity is stamped by the kernel. Read it from the invocation/message
context, not from an untrusted payload field. `home://`, capsule environment,
secrets, and KV resolve per principal. Never cache `env::var` or another
principal-specific value in a process-global static.

When consuming subscription batches, validate the principal on every message;
one batch can contain multiple publishers.

## Core SDK modules

- `fs`: VFS read/write, directory, and metadata operations. Requires matching
  `fs_read`/`fs_write` prefixes.
- `kv`: principal-and-capsule-scoped JSON or bytes, key listing, deletion, and
  compare-and-swap. Normal KV needs no manifest grant today.
- `http`: outbound requests and streaming responses under `net`.
- `ipc`: publish, subscribe, poll, receive, and request/response over topic ACLs.
- `process`: allowlisted host commands under `host_process`; persistent spawn
  also needs `allow_persistent`.
- `net`: raw connect/bind under `net_connect` and `net_bind`.
- `identity`: resolve/link/admin operations under the identity level granted.
- `env`: install-configured values and secrets, resolved per invocation.
- `approval`: request governed human approval for an action/resource.
- `elicit`: interactive install/upgrade prompting only.
- `runtime`: caller context, readiness, randomness, and runtime information.
- `time` and `log`: time/sleep and structured guest logging.

Most SDK functions return `Result<_, SysError>`. Validate all LLM-supplied and
external data before calling the host. Do not treat a host error string as a
stable typed protocol unless the API explicitly documents it.

## Run loops

Most tool capsules need no run loop. If one is necessary:

- perform bounded initialization and call `runtime::signal_ready()`;
- yield through receive, sleep, or host calls rather than burning CPU;
- treat receive timeout as `Ok` with an empty message list;
- checkpoint state deliberately;
- make shutdown and restart idempotent;
- use an uplink or persistent process only when its stronger authority is
  genuinely part of the capsule's job.
