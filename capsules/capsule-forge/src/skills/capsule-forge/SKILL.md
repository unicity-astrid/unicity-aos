---
name: capsule-forge
description: Author a Unicity AOS capsule from zero. Use when creating, building, scaffolding, or debugging a sandboxed WASM capsule, its Rust SDK code, Capsule.toml capability and bus ACL, WIT contracts, installable artifact, or Forge workflow.
---

# Capsule Forge — Author a Unicity AOS Capsule From Zero

You are about to write a **capsule**: a small WebAssembly Component, compiled
from Rust, that Astrid Runtime loads into a sandbox and lets it expose
**tools** to the LLM over an event bus. You need no prior AOS knowledge —
**this page is the whole map.** Everything you need to write the WIT references,
the `Capsule.toml`, and the Rust is here. You should never have to leave it.

> **The one sentence that anchors everything:** A capsule is a sandboxed WASM
> tool provider that talks *only* over a message bus. The kernel is **dumb** — it
> routes events, enforces capabilities, and runs the sandbox; it contains no tool
> logic. **All intelligence lives in capsules.** Your `Capsule.toml`'s
> `[publish]`/`[subscribe]` keys are not config — they *are* the capability ACL
> the kernel enforces, and they are **fail-closed**: a topic you don't list, you
> cannot touch.

---

## Table of contents

1. 60-second quickstart
2. What you are actually building
3. The minimal file set (copy verbatim)
4. The macro reference (`#[capsule]`, `#[astrid::tool]`, lifecycle hooks)
5. The SDK surface (`astrid_sdk::*`)
6. `Capsule.toml` — the complete manifest reference
7. The capability catalog (lists vs. bools — get this right)
8. WIT / interface references at author depth
9. How a tool call actually flows (the IPC lifecycle)
10. The three topic matchers (the conceptual footgun)
11. The build → install → call dev loop
12. Footguns (read before you waste an hour)
13. Security mindset
14. Design principles (write small capsules)
15. The forge tools

---

## 1. 60-Second Quickstart

Get a complete, *compiling* skeleton one of three ways, then build → install →
call:

- **Forge tool (always available with this Skill):** call
  `scaffold_capsule { "name": "my-capsule" }`. It returns a JSON map of
  `path -> file content` for a complete skeleton — write each file out.
- **CLI:** `aos capsule new my-capsule` scaffolds the same full project
  (`.cargo/config.toml`, `rust-toolchain.toml`, `Cargo.toml`, `Capsule.toml`,
  `src/lib.rs`, `README.md`) ready to `cargo build` on the first try.
- **By hand:** copy the files in section 3, substituting your name.

Then:

```bash
# 1. Install the WASM target once (the scaffold pins it in rust-toolchain.toml):
rustup target add wasm32-unknown-unknown

# 2. Build the capsule. `aos capsule build` produces ./dist/<name>.capsule.
#    Plain `cargo build` also works — .cargo/config.toml selects the target,
#    so DO NOT pass --target.
aos capsule build

# 3. Install it into the running daemon (content-addressed — see footgun 4):
aos capsule install ./dist/my-capsule.capsule

# 4. Verify it loaded:
aos capsule list          # your capsule should appear
aos status                # daemon should still be healthy

# 5. Ask the LLM to call your tool.
```

---

## 2. What You Are Actually Building

```
my-capsule/
├── .cargo/config.toml     # selects the wasm target + the getrandom flag (#1 footgun)
├── rust-toolchain.toml    # pins the toolchain + the wasm target
├── Cargo.toml             # cdylib crate, depends on astrid-sdk
├── Capsule.toml           # the manifest — capabilities + the bus ACL
└── src/
    └── lib.rs             # your tools
```

A capsule:

- runs in a **WASM sandbox** (`wasm32-unknown-unknown`, Component Model) with
  **zero `wasi:*` imports**. Every host call is an audited `astrid:*` call routed
  through the SDK. The WIT import list *is* the capsule's literal capability list.
- talks to the world **only over the bus**. It cannot open a socket, read a file,
  or spawn a process except through a capability its manifest declares and the
  kernel grants. Any undeclared syscall is denied before it reaches the bus.
- exposes **tools** the LLM can call. A tool is just a Rust method; the
  `#[astrid::tool]` macro wires it onto the bus and publishes a JSON-schema
  description of it so the model discovers it.

There is also a **per-principal sandbox**: `home://` resolves to *the calling
principal's* home, and `__state`/KV are auto-scoped per capsule **and** per
principal. Two users invoking your capsule never see each other's data.

---

## 3. The Minimal File Set (copy verbatim, substitute the name)

### `.cargo/config.toml`

```toml
[build]
target = "wasm32-unknown-unknown"

[target.wasm32-unknown-unknown]
# THE #1 FOOTGUN. Activates astrid-sys's custom getrandom backend, which routes
# entropy to the host's astrid:sys.random-bytes. WITHOUT THIS, anything pulling
# getrandom — uuid::v4, HashMap's RandomState seed — fails to LINK on
# wasm32-unknown-unknown. A library CANNOT set this for its dependents; it MUST
# live here, in the final capsule crate. This is the most common confusing build
# failure for new authors.
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
license = "MIT OR Apache-2.0"
publish = false

[lib]
crate-type = ["cdylib"]      # capsules are cdylib — NOT bin, NOT the default rlib

[dependencies]
# 0.7 resolves to 0.7.1+, which carries the tool_describe publish fix (a
# return-only describe collects zero tools), so the floor matters.
astrid-sdk = { version = "0.7", features = ["derive"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"

[profile.release]
opt-level = "z"      # optimise for size — capsules ship as wasm
lto = true
codegen-units = 1
strip = true
panic = "abort"
```

### `Capsule.toml`

```toml
[package]
name = "my-capsule"            # lowercase ASCII alphanumeric + hyphens only
version = "0.1.0"             # MUST be three-part numeric MAJOR.MINOR.PATCH
description = "What this capsule does"
authors = ["Your Name <you@example.com>"]
astrid-version = ">=0.7.0"    # semver requirement against the host runtime

[[component]]
id = "my-capsule"
file = "my_capsule.wasm"      # crate name with hyphens -> underscores, + .wasm
type = "executable"           # "executable" (default) or "library"

[capabilities]
# Declare ONLY what you use. Least capability. (See section 7.)
fs_read = ["home://"]

# --- The mandatory tool-bus ACL. Without these two publish keys, tool results
#     never return and the describe fan-out cannot answer. ---
[publish]
"tool.v1.execute.*.result" = { wit = "@unicity-astrid/wit/types/tool-call-result" }
"tool.v1.response.describe.*" = { wit = "@unicity-astrid/wit/tool/describe-response" }

# One subscribe row per tool, plus the describe request. The `handler` is
# `tool_execute_<tool_name>`. `tool_describe` is AUTO-GENERATED by the macro —
# you declare it here but never write it.
[subscribe]
"tool.v1.execute.hello" = { wit = "@unicity-astrid/wit/types/tool-call", handler = "tool_execute_hello" }
"tool.v1.request.describe" = { wit = "@unicity-astrid/wit/tool/describe-request", handler = "tool_describe" }
```

### `src/lib.rs`

```rust
#![deny(unsafe_code)]
#![deny(clippy::all)]

use astrid_sdk::prelude::*;   // capsule, SysError, fs, ipc, kv, http, log, runtime, ...
use astrid_sdk::schemars;     // for #[derive(schemars::JsonSchema)] on the args type
use serde::Deserialize;

#[derive(Default)]            // the struct MUST derive Default
pub struct MyCapsule;

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct HelloArgs {
    /// Who to greet (this doc-comment becomes the parameter description the LLM sees).
    pub name: String,
}

#[capsule]
impl MyCapsule {
    /// Greet someone. This method doc-comment becomes the tool description the
    /// LLM sees, so write it for the model.
    #[astrid::tool("hello")]
    pub fn hello(&self, args: HelloArgs) -> Result<String, SysError> {
        Ok(format!("Hello, {}!", args.name))
    }
}
```

> **Do NOT create a checked-in `wit/` directory.** The WIT the manifest
> references (`@unicity-astrid/wit/...`) is resolved at build time — it is
> *generated*, never hand-authored in your repo. A `wit/` directory will confuse
> the build.

---

## 4. The Macro Reference

Everything lives behind one prelude import:

```rust
use astrid_sdk::prelude::*;   // brings capsule, SysError, and the modules: fs,
                              // ipc, kv, http, log, runtime, env, time, net,
                              // process, identity, approval, capabilities, ...
use astrid_sdk::schemars;     // for #[derive(schemars::JsonSchema)] on args
```

### `#[capsule]` — on the `impl` block

`#[capsule]` (or `#[capsule(state)]` for stateful mode) goes on your `impl Struct`
block. It generates the four WASM exports the kernel calls
(`astrid_hook_trigger`, `run`, `astrid_install`, `astrid_upgrade`), the
`export!()` wiring, the auto `tool_describe`, and a panic hook that routes guest
panics to `log::error`. **The struct must `#[derive(Default)]`** — this is
enforced by a generated assertion.

### `#[astrid::tool]` — on a method

Accepted forms:

```rust
#[astrid::tool("name")]            // explicit tool name
#[astrid::tool("name", mutable)]   // explicit name, marked mutable
#[astrid::tool(mutable)]           // name inferred from the method name, mutable
#[astrid::tool]                    // name inferred, not mutable
```

The method signature is:

```rust
fn name(&self, args: ArgsType) -> Result<T, E>
```

where:

- **`ArgsType`** derives `serde::Deserialize + schemars::JsonSchema` (and
  `Default`). The JsonSchema derive produces the parameter schema the LLM sees;
  field doc-comments become parameter descriptions.
- **`T: serde::Serialize`** — `String` is the common choice (return JSON or
  markdown text). Anything `Serialize` works.
- **`E: Display`** — use `SysError`. The error's `Display` string is returned to
  the model as the tool's error content.
- **The method doc-comment becomes the tool description** shown to the LLM.
- **`mutable`** (or a `&mut self` receiver) flags a state-mutating tool. The flag
  rides in the tool schema so the approval layer can present the right copy; it
  does **not** change routing or auto-gate anything. With `&mut self`, the macro
  loads `self` from KV before the call and persists it after (on success only) —
  see "Stateful mode" below.

Behind the scenes: a tool named `foo` becomes an interceptor handler
`tool_execute_foo`, dispatched when an event arrives on `tool.v1.execute.foo`,
and its result is published on `tool.v1.execute.foo.result`.

### Lifecycle hooks (each is a singleton; duplicates are a compile error)

- **`#[astrid::install]`** — `fn(&self) -> Result<(), SysError>`. Runs once at
  `aos capsule install`, *before* the capsule enters the normal runtime.
  This is the only place `elicit` works (interactive secret/value prompting).
  **This is also how a Skill lands on disk** — see footgun 5.
- **`#[astrid::upgrade]`** — `fn(&self, prev_version: &str) -> Result<(), SysError>`.
  Runs on version upgrade; receives the previous version string.
- **`#[astrid::run]`** — `fn(&self) -> Result<(), SysError>`. A long-lived
  background loop (rare — most capsules are pure tool providers and need none).
  A run-loop capsule must call `runtime::signal_ready()` once it is initialized,
  and should `recv` regularly (the kernel epoch-interrupts a run loop that burns
  CPU without yielding). `run` never auto-persists state.
- **`#[astrid::interceptor("topic")]`** — `fn(&self, payload: Vec<u8>) -> Result<InterceptResult, SysError>`.
  The raw event handler. Tools are sugar over this. Returns
  `InterceptResult::Continue(payload)` / `Final(payload)` / `Deny { reason }`. A
  `handler =` in a `[subscribe]` row binds a topic to one of these.
- **`#[astrid::command("name")]`** — registers a slash-command handler (dispatched
  like a tool, but not surfaced as an LLM tool / no describe entry).

### Stateful mode

Two ways to make a capsule stateful: put `#[capsule(state)]` on the impl block, or
take `&mut self` on any handler. Then:

- before each handler, the macro loads your struct from KV key `__state`
  (`kv::get_json("__state")`), falling back to `Default` on a decode error;
- after a **successful** handler call, it persists with
  `kv::set_json("__state", &self)` (skipped on error, so a failure never writes
  partial state);
- `#[astrid::install]` starts from `Default` and persists the result;
  `#[astrid::upgrade]` starts from the saved state (or `Default`);
- `#[astrid::run]` loads once at startup and **never** auto-saves — manage your
  own persistence with explicit `kv::set_json(...)` calls.

Stateless capsules (`&self`, no `state`) use an in-memory `OnceLock<T>` singleton
and never touch KV.

---

## 5. The SDK Surface (`astrid_sdk`)

The prelude re-exports `SysError` and every module below; you can write `fs::read(...)`
after `use astrid_sdk::prelude::*`, or fully-qualify as `astrid_sdk::fs::read(...)`.
Almost every function returns `Result<_, SysError>`.

**`SysError`** variants: `HostError(String)` (any host-call failure — all typed
host error codes collapse here), `JsonError`, `BorshError`, `ApiError(String)`
(your own logic errors — `SysError::ApiError("…".into())`).

### `fs` — VFS file I/O (paths are VFS schemes, e.g. `home://...`)

These are live and cover the common cases:

```rust
fs::exists(path) -> Result<bool>
fs::read(path) -> Result<Vec<u8>>
fs::read_to_string(path) -> Result<String>
fs::write(path, contents: &[u8]) -> Result<()>      // truncate-or-create; 10 MB/call cap
fs::create_dir(path) -> Result<()>                  // strict: fails if it exists
fs::create_dir_all(path) -> Result<()>              // idempotent
fs::remove_file(path) -> Result<()>
fs::read_dir(path) -> Result<ReadDir>               // iterator of DirEntry (.file_name()); 4096-entry cap
fs::metadata(path) -> Result<Metadata>              // .is_file(), .is_dir(), .len(), .modified()
```

The SDK also *exposes* `fs::append`, `fs::copy`, `fs::rename`, `fs::remove_dir_all`,
`fs::canonicalize`, `fs::read_link`, `fs::hard_link`, `fs::symlink_metadata`, and a
`fs::File` offset-I/O handle — but several of these are **host-stubbed** today and
return a "port pending" error. For a new capsule, stick to read/write/`read_dir`/
`metadata`/`create_dir_all`/`remove_file` and read whole files at once.

### `kv` — per-(capsule, principal) key-value store (no capability needed)

```rust
kv::get_json::<T>(key) -> Result<T>            // T: DeserializeOwned
kv::get_json_opt::<T>(key) -> Result<Option<T>>
kv::set_json::<T>(key, &value) -> Result<()>   // T: Serialize
kv::get_bytes(key) / kv::get_bytes_opt(key) / kv::set_bytes(key, &[u8])
kv::delete(key) -> Result<()>
kv::list_keys(prefix) -> Result<Vec<String>>
kv::clear_prefix(prefix) -> Result<u64>
kv::cas(key, expected: Option<&[u8]>, new: &[u8]) -> Result<bool>   // compare-and-swap
// Versioned + borsh variants also exist.
```

### `http` — outbound HTTP (needs `net` capability)

```rust
let resp = http::send(&http::Request::get("https://api.example.com")
    .header("authorization", "Bearer …")
    .json(&body)?)?;            // .post/.put/.delete; .body/.body_bytes too
resp.status() -> u16; resp.is_success() -> bool;
resp.text() -> Result<&str>; resp.json::<T>() -> Result<T>; resp.bytes() -> &[u8]
// http::stream_start(&req) -> HttpStream for chunked/SSE responses (.read_chunk()).
```

### `ipc` — the event bus (publish/subscribe; the heart of capsule comms)

```rust
ipc::publish(topic, payload: &str) -> Result<()>
ipc::publish_json::<T>(topic, &payload) -> Result<()>           // T: Serialize
ipc::subscribe(topic_pattern) -> Result<Subscription>          // trailing-* only (see §10)
sub.recv(timeout_ms) -> Result<PollResult>                     // empty messages = timeout
sub.poll() -> Result<PollResult>                               // non-blocking
ipc::request_response::<Req, Resp>(req_topic, resp_namespace, &req, timeout_ms) -> Result<Resp>
// publish_as / publish_json_as exist but are UPLINK-ONLY (need uplink = true).
```

A `PollResult` carries `messages: Vec<Message>`; each `Message` has
`topic`, `payload`, `source_id`, and `principal: PrincipalAttribution`
(`Verified(_)` / `Claimed(_)` / `System`). For any sensitive action, gate on
`message.principal.verified()` per message — a `recv` batch can mix publishers.
**Note:** a `recv` timeout returns `Ok` with an empty message list, *not* an
error — treat an empty `PollResult` as the timeout signal.

### `log` — structured logging (infallible)

```rust
log::trace(msg); log::debug(msg); log::info(msg); log::warn(msg); log::error(msg);
// arg is `impl Display`. ERROR-level guest logs also surface in the daemon log.
```

### `runtime`

```rust
runtime::signal_ready() -> Result<()>          // run-loop capsules call this when initialized
runtime::caller() -> Result<CallerContext>     // who invoked this handler
runtime::random_bytes(len) -> Result<Vec<u8>>  // host entropy (OsRng)
runtime::socket_path() -> Result<String>
```

### Other modules (use only with the matching capability)

- **`process`** (needs `host_process`) — `process::Command::new("git").arg("status").spawn()? -> Output`
  (`.stdout/.stderr/.exit`); `.spawn_background()? -> Process` (RAII, drop reaps);
  `.spawn_persistent()` needs `allow_persistent`. Per-capsule cap 8 concurrent.
- **`net`** (needs `net_connect` / `net_bind`) — `TcpStream`, `TcpListener`,
  `UnixListener`. Raw sockets; rare.
- **`identity`** (needs `identity`) — `resolve`, `link`, `unlink`, `list_links`,
  `create_user`.
- **`approval`** — `approval::request(action, resource) -> Result<bool>` blocks for
  human approval (or hits an existing allowance). No capability required.
- **`elicit`** — interactive prompting; **only valid inside an install/upgrade
  hook**. `elicit::secret(key, desc)`, `elicit::text(...)`, `elicit::select(...)`,
  `elicit::array(...)`.
- **`env`** — `env::var(key) -> Result<String>`, `env::var_opt(key)`. Reads the
  per-invocation, per-principal capsule config (declared in `[env]`). **Never
  cache the result in a `OnceLock`/static** — `env::var` resolves the active
  principal's overlay each call; a global cache pins one principal's value for all.
- **`capabilities`** — `capabilities::enumerate() -> Vec<String>` (your own held
  capability names, infallible); `capabilities::check(uuid, cap)`.
- **`time`** — `time::now()`, `time::sleep(duration)`, `time::monotonic()`.

---

## 6. `Capsule.toml` — The Complete Manifest Reference

The manifest is the authoritative, declarative source of truth. The kernel reads
it before touching a byte of WASM. Every section below is optional except
`[package]` (needs `name` + `version`) and a `[[component]]` for a WASM capsule.

### `[package]`

```toml
[package]
name = "aos-http"   # required; lowercase ASCII alphanumeric + hyphens
version = "0.1.0"             # required; three-part numeric
description = "HTTP fetch tool"
authors = ["Name <e@mail>"]
astrid-version = ">=0.7.0"     # semver req against the host runtime
license = "MIT OR Apache-2.0"
repository = "https://github.com/…"
keywords = ["http", "fetch"]
categories = ["networking"]
publish = true                # set false to block registry publication
```

### `[[component]]`

One per WASM binary (a capsule usually has exactly one).

```toml
[[component]]
id = "http-tools"
file = "aos_http.wasm"  # path relative to Capsule.toml; crate name, hyphens->underscores
type = "executable"               # "executable" (default) or "library"
```

Do not hand-author a component digest here. Installation records the WASM's
BLAKE3 content address in `meta.json`, and the runtime verifies those installed
bytes before loading them.

### `[publish]` and `[subscribe]` — the IPC surface AND the ACL

Each **key** is an IPC topic name or wildcard pattern; the **keys are exactly the
kernel's IPC ACL** (`effective_ipc_publish_patterns` / `_subscribe_patterns`).
A capsule may publish only to topics matching a `[publish]` key and subscribe only
to topics matching a `[subscribe]` key — anything else is denied. There is no
separate ACL array.

Each value carries a typed WIT payload reference, in **short** or **long** form:

```toml
[publish]
"tool.v1.execute.*.result" = "@unicity-astrid/wit/types/tool-call-result"   # short form
"tool.v1.response.describe.*" = { wit = "@unicity-astrid/wit/tool/describe-response" }  # long

[subscribe]
"tool.v1.execute.fetch_url" = { wit = "@unicity-astrid/wit/types/tool-call", handler = "tool_execute_fetch_url" }
"tool.v1.request.describe"  = { wit = "@unicity-astrid/wit/tool/describe-request", handler = "tool_describe" }
```

The `wit` value may be:

- an `@scope/repo/<iface>/<record>` reference (the standard tool-bus form), or
- a bare local record name (resolved from your own `wit/` — rare for tool capsules), or
- the literal `"opaque"` — declares the ACL but waives payload type-checking. Used
  by uplink/proxy capsules that forward bytes they do not own.

On a `[subscribe]` entry, **`handler = "..."`** binds the topic to a generated
WASM export (your `#[astrid::tool]`/`#[astrid::interceptor]` method, or the auto
`tool_describe`). An optional **`priority` (u32, default 100, lower fires first)**
orders the interceptor chain. A `[subscribe]` entry **without** a handler is
ACL-only — it grants you the right to `ipc::subscribe()` that topic at runtime,
but binds no export.

**The mandatory tool-bus boilerplate** for any tool capsule is always:

- `[publish]`: `tool.v1.execute.*.result` (so results return) **and**
  `tool.v1.response.describe.*` (so the describe fan-out can answer).
- `[subscribe]`: one `tool.v1.execute.<tool>` per tool (handler
  `tool_execute_<tool>`) **plus** `tool.v1.request.describe` (handler
  `tool_describe`). `tool_describe` is auto-generated — list it, never write it.

If `[publish]` or `[subscribe]` is **empty/missing**, the capsule cannot talk on
the bus at all. Fail-closed means silence, not a loud error.

### `[capabilities]`

What the capsule may ask of the OS. Every field is fail-closed (empty list or
`false`). See section 7 for the full catalog and the list-vs-bool table.

### `[imports]` / `[exports]` — the WIT interface contract

Declares which `astrid:*` interfaces (or another capsule's exported interface)
this capsule depends on or provides. Two equivalent surface forms:

```toml
# Flat (cargo-like):
[imports]
"astrid:llm" = "^1.0"
"astrid:kv"  = { version = "^1.0", optional = true }   # optional: boot even with no provider

[exports]
"astrid:llm" = "1.0.0"

# Nested (equivalent):
[imports.astrid]
llm = "^1.0"
```

Most pure tool capsules need *neither* — the tool-bus topics are a convention, not
a WIT import. You need `[imports]`/`[exports]` only when you provide or consume a
*typed interface* (e.g. an LLM provider exports `astrid:llm`). **Uplink capsules
may not declare `[imports]`** (the loader rejects it).

### `[env]` — capsule configuration, elicited at install

```toml
[env]
API_KEY = { type = "secret", request = "Enter your API key", placeholder = "sk-..." }
REGION  = { type = "select", enum_values = ["us-east-1", "eu-west-1"], default = "us-east-1" }
TAGS    = { type = "array",  request = "Comma-separated tags" }
NAME    = { type = "text",   request = "Your name", default = "Agent" }
```

`type` is `secret` | `text` | `select` | `array`. **`secret`** is masked at the
install prompt and stored 0600 under
`~/.aos/runtime/secrets/<scope>/<capsule>/<key>`. The owning capsule
receives the plaintext only when it calls `env::var("API_KEY")`; it is not
stored in ordinary env JSON. Other values land in per-principal env JSON.
**`scope` is operator-only**
(`skip_deserializing`) — a manifest **cannot** set it; the kernel decides per-agent
vs. shared from operator action. (The forge `validate_manifest` warns if you try.)

### Other sections (declarative, less common)

- `[[command]]` — slash-command registrations.
- `[[mcp_server]]` — stdio MCP servers (the "airlock override"; `command` must be
  in `host_process`; this breaks out of the WASM sandbox into a host process).
- `[[skill]]` — Skills the capsule contributes. Set `name`, `description`, and a
  relative `file` path. `astrid capsule build` packages the declared file, and
  the AOS Skills index discovers it from installed capsule introspection.
- `[[context_file]]`, `[[uplink]]`, `[[tool]]`, `[[topic]]` (legacy).

---

## 7. The Capability Catalog (lists vs. bools — get this right)

Declare only what you use (least capability). The kernel's `ManifestSecurityGate`
enforces these at every host call; an undeclared capability is denied before it
reaches the bus. **The single most-missed detail:** some keys take a **list**,
some take a **bool**. Getting this wrong is a parse/semantics error.

| Key | Type | Grants |
|---|---|---|
| `uplink` | **bool** | Act as a long-lived uplink/daemon; enables `publish_as`. Disables the WASM timeout. |
| `net` | **list** | Outbound HTTP via `http`. Entries are hostnames or `"*"`. `["api.openai.com"]`, `["*"]`. |
| `net_connect` | **list** | Raw outbound TCP. Each entry `"host:port"` or `"host:*"` (no DNS wildcards). |
| `net_bind` | **list** | Bind a listening socket. `["unix:*"]`. Rare. |
| `kv` | **list** | Reserved (declared, NOT yet gate-enforced). Per-capsule KV already works without it (auto-scoped per capsule + principal). Use `kv = []` or omit. |
| `fs_read` | **list** | Read under the given VFS prefixes. `["home://"]`. |
| `fs_write` | **list** | Write mutable data under the given prefixes. Prefer a capsule-specific path such as `["home://data/my-capsule/"]`. |
| `host_process` | **list** | Spawn the named host binaries via `process`. `["git", "cargo"]`. The "airlock override". |
| `allow_persistent` | **bool** | Operator sub-grant on top of `host_process`: allow persistent (instance-outliving) child processes. |
| `identity` | **list** | Identity ops. Values: `"resolve"` < `"link"` < `"admin"` (each implies the lesser). `["resolve"]`. |
| `allow_prompt_injection` | **bool** | Hook output may modify the system prompt. Off by default — unprivileged capsules cannot inject system-prompt instructions. |

```toml
# Worked example — a capsule that reads home, fetches one API, and runs git:
[capabilities]
fs_read      = ["home://"]
net          = ["api.example.com"]
host_process = ["git"]
```

**VFS schemes** for `fs_*` prefixes:

- `home://` — the *calling principal's* home directory, resolved per-invocation
  (per-principal isolation; most common).
- `cwd://` — the capsule's install directory, resolved at construction.
- `"*"` — workspace-confined (broad; prefer a narrow prefix). Does **not** grant
  whole-filesystem access. Paths containing `..` are always rejected.

---

## 8. WIT / Interface References at Author Depth

You almost never hand-write WIT for a tool capsule. What you need to know:

- **WIT is the contract surface** — the host ABI (`astrid:fs`, `astrid:ipc`,
  `astrid:kv`, `astrid:http`, `astrid:sys`, `astrid:process`, `astrid:identity`,
  …) is a set of frozen `@1.0.0` interfaces. The SDK wraps them; you call the SDK.
- **The `wit = "@unicity-astrid/wit/..."` references** in `[publish]`/`[subscribe]`
  name the *payload type* for a topic. For tool capsules they are always exactly
  these four — copy them verbatim:

  | Topic role | WIT reference |
  |---|---|
  | tool execute request (subscribe) | `@unicity-astrid/wit/types/tool-call` |
  | tool result (publish) | `@unicity-astrid/wit/types/tool-call-result` |
  | describe request (subscribe) | `@unicity-astrid/wit/tool/describe-request` |
  | describe response (publish) | `@unicity-astrid/wit/tool/describe-response` |

- **The tool payloads** are simple. A request carries `call_id`, `tool_name`,
  `arguments` (JSON). A result is `ToolCallResult { call_id, content: String,
  is_error: bool }` — there is no structured error type; you produce a string and a
  boolean. The SDK macro builds these for you from your `Result<T, E>`.
- **`wit = "opaque"`** waives type-checking for uplink/proxy capsules (keeps the
  ACL). You won't need it for a tool capsule.
- **Inspect a real interface** with the forge `explain_interface { "name": "tool" }`
  tool (reads from `home://wit/` and summarizes package/interfaces/records), or the
  system capsule's `list_interfaces` / `read_interface`.
- **Do not check a `wit/` directory into your repo** — it's generated at build.

---

## 9. How a Tool Call Actually Flows (the IPC lifecycle)

Tools are a *convention* over bus primitives — the kernel has no notion of a "tool".
Understanding the round trip explains the boilerplate:

1. The **react** loop decides the LLM wants tool `foo`; it publishes a
   `ToolExecuteRequest` on `tool.v1.request.execute`.
2. The **router** capsule (stateless middleware) catches it, validates the tool
   name (rejects anything but alphanumeric / `-` / `_` / `:` — dots are forbidden
   to prevent topic injection), and forwards to `tool.v1.execute.foo`.
3. **Your capsule** is subscribed to `tool.v1.execute.foo` with handler
   `tool_execute_foo`. The macro deserializes the args, runs your method, and
   publishes the result on `tool.v1.execute.foo.result` (covered by your
   `tool.v1.execute.*.result` publish ACL).
4. The **router** catches the per-tool result and republishes it on the unified
   `tool.v1.execute.result`, which react polls and matches by `call_id`.

**The describe fan-out** (how the model learns your tools exist):

1. The **prompt-builder** subscribes `tool.v1.response.describe.*`, then publishes
   an empty `tool.v1.request.describe`.
2. **Your capsule's** auto `tool_describe` handler fires and publishes its
   `{ "tools": [...] }` descriptor on `tool.v1.response.describe.self` (covered by
   your `tool.v1.response.describe.*` publish ACL; the real source_id rides in
   kernel-stamped metadata).
3. The prompt-builder drains responses over a bounded ~500ms window, dedups by
   tool name (first wins), and caches the schema.

**Critical:** `tool_describe` must **publish**, never return — a return-only
describe collects zero tools (the bug SDK 0.7.1 fixed). The `#[capsule]` macro does
this correctly; just depend on `astrid-sdk = "0.7"` (resolves to 0.7.1+).

---

## 10. The Three Topic Matchers (the conceptual footgun)

`*` in a topic does not mean one consistent thing — there are **three** matchers
with **three** different rules. Name this so you don't trip:

1. **Event delivery / route matcher (subtree).** A **trailing `*`** matches the
   whole subtree at any depth ≥ prefix+1: `a.b.*` delivers `a.b.c` *and*
   `a.b.c.d`. This decides which subscriptions receive a published event.
2. **ACL authorization (publish + subscribe).** As of the current kernel, the
   publish/subscribe ACL authorizes via the same subtree matcher as delivery —
   a trailing `*` is recursive. (`tool.v1.execute.*.result` authorizes
   `tool.v1.execute.foo.result`.)
3. **Interceptor dispatch (strict equal-segment).** When the kernel routes a
   *concrete* event to your *handler*, segment count must match exactly and a mid
   `*` matches exactly one segment. **This is why each tool needs its own concrete
   `tool.v1.execute.<tool>` subscribe row** — you cannot collapse them into one
   wildcard for dispatch.
4. **Runtime `ipc::subscribe` syntactic gate.** At runtime you may subscribe with
   **at most one `*`, and it must be trailing** (`foo.bar.*`). A mid-segment or
   multi-`*` pattern is host-rejected at runtime — even though it's legal in a
   static `[subscribe]` ACL key. Per-capsule cap: 128 subscriptions.

**Practical rules:**

- **Subscribe:** use one *trailing* `*` and let subtree delivery handle variable
  depth. Never enumerate `.*.*` — redundant *and* runtime-illegal.
- **Publish:** the publish ACL is fine with the concrete patterns you declare
  (`tool.v1.execute.*.result`); a fixed deep front-door topic needs its own exact
  publish row.
- **Tool dispatch is strict** — one concrete `tool.v1.execute.<tool>` row per tool.

---

## 11. The Build → Install → Call Dev Loop

```
edit src/lib.rs / Capsule.toml
      │
      ▼
aos capsule build               # -> ./dist/<name>.capsule
      │                         #    (plain `cargo build` works too; no --target)
      ▼
aos capsule install ./dist/<name>.capsule      # content-addressed; replaces prior version
      │
      ▼
aos capsule list                # confirm it loaded
aos status                      # confirm the daemon is healthy
      │
      ▼
ask the LLM to call the tool    # verify behaviour
      │
      └── tools missing? -> capsule_doctor (forge tool), then re-prompt
```

- **There is NO hot-reload** — the watcher is dead code (issue #296). To iterate,
  rebuild and **reinstall**; each install replaces the prior version.
- **Capsule logs** live under
  `~/.aos/runtime/home/<principal>/.local/log/<capsule>/`. System logs
  live separately under `~/.aos/runtime/log/`. A guest panic shows as
  `capsule panic at src/lib.rs:NN` (the SDK installs a panic hook). ERROR-level
  guest logs also surface in the daemon log. Grep the per-capsule log when a tool
  traps or a run loop exits.
- If `aos` isn't on PATH, it's at `~/.aos/bin/aos`.
- Plain `cargo build --release` is useful as a compile check because
  `.cargo/config.toml` selects the target, but its raw `.wasm` is not
  installable. Use `aos capsule build` to produce `dist/*.capsule`.

---

## 12. Footguns (read these before you waste an hour)

1. **The getrandom flag.** `.cargo/config.toml` must carry
   `rustflags = ["--cfg=getrandom_backend=\"custom\""]`. Without it, `uuid::v4`
   and `HashMap` fail to **link** on `wasm32-unknown-unknown`. A library can't set
   it for you — it must live in *your* crate. The #1 cause of confusing build
   failures.
2. **`crate-type = ["cdylib"]`.** Not `bin`, not the default `rlib`.
3. **No checked-in `wit/`.** The WIT contracts are generated at build time.
4. **Content-addressed install.** Install with
   `aos capsule install ./dist/<name>.capsule`. **Do not** hand-copy the
   `.wasm` into the runtime home — install records a BLAKE3 hash in `meta.json`, and a
   capsule whose binary doesn't match (or wasn't installed this way) fails to load.
5. **Declare every shipped Skill.** Put the `SKILL.md` inside the capsule source
   tree and reference it with `[[skill]] name`, `description`, and `file`.
   Current `astrid capsule build` packages that asset; AOS discovers it from the
   installed capsule mirror. Do not copy shipped Skills from an install hook or
   request `fs_write` merely for distribution.
6. **`tool_describe` must publish, not return** — covered by depending on
   `astrid-sdk = "0.7"` (0.7.1+). The macro does it right; don't hand-roll describe.
7. **Empty `[publish]`/`[subscribe]` = silent muteness.** Fail-closed means no
   error you'll notice quickly — the capsule just can't talk on the bus.
8. **`env::var` is per-principal; never cache it** in a `OnceLock`/static — you'd
   pin one principal's config for everyone.
9. **`recv` timeout returns `Ok` with empty messages,** not an error. Branching on
   an error string for timeout is dead code.

> **No longer a footgun:** earlier kernels had a describe fan-out that was
> non-deterministically incomplete on the first prompt after boot. That race is
> **fixed** in the current kernel — if your tools are in the manifest and the
> capsule loaded, the model will see them. If they don't appear, it is almost
> certainly a real manifest problem (run `capsule_doctor`), not a kernel race.

---

## 13. Security Mindset

Capsules run other people's prompts and handle untrusted, LLM-supplied arguments.
Three rules, always:

1. **Validate untrusted input.** Reject path traversal in any name/path argument
   (`/`, `\`, `..`). The LLM will, eventually, hand you `../../etc/passwd`. The VFS
   gate also rejects `..`, but validate at your edge too.
2. **Least capability.** Declare the narrowest `fs_*`/`net`/`host_process` scope
   that works. The manifest ACL is your blast radius — and the first thing a
   reviewer reads.
3. **Fail closed.** On any doubt — missing capability, malformed input, denied
   topic — refuse rather than guess. Gate sensitive actions on
   `message.principal.verified()`. The kernel fails closed; your code should too.

---

## 14. Design Principles — Write Small Capsules

The kernel is dumb on purpose, and the consequence is: **a capsule should do one
thing.** The runtime composes many small capsules over the bus; it does not host a
few large ones. This isn't style — it's what the security model rewards:

- **Least privilege is a function of scope.** A file-reader needs only `fs_read`.
  Fold in writes, network, and process-spawn and its floor becomes the union of
  all of them — a permanent, maximal blast radius. A prompt injection into a
  single-purpose reader can at worst read files it could already read.
- **Compose, don't embed.** Capsules don't call each other — they publish and
  subscribe. Need a capability you don't own? Publish to the capsule that owns it.
  Adding behaviour = adding a capsule, never forking a monolith.
- **Keep tools cohesive.** The tools a capsule exports should share a domain. A
  capsule whose tools have nothing in common is a bundle, not a capsule.
- **Push state to where it belongs.** Routing/transform capsules stay stateless;
  durable state lives in session/memory/KV.

Read the `[capabilities]` block as a job description. If it spans unrelated
domains (files *and* network *and* process), that's the smell, not the feature.

---

## 15. The Forge Tools

This Skill ships with the **forge** capsule, which gives you tools to do all of
the above without leaving the chat:

| Tool | Use it when |
|---|---|
| `forge_quickstart` | You want the condensed build-your-first-capsule guide inline. |
| `meta_harness_quickstart` | Work reveals a useful way to extend the agent's memory, skills, harness, composition, or capabilities. |
| `scaffold_capsule { name }` | You want a complete compiling skeleton as `path -> content` JSON to write out. |
| `explain_interface { name }` | You need to read a WIT contract (e.g. `tool`, `llm`, `session`) plus a plain-English summary. |
| `suggest_capabilities { intent }` | You describe what the capsule should do and get the exact manifest lines (incl. real LLM-provider topics). |
| `validate_manifest { toml }` | You want your `Capsule.toml` linted for the common mistakes before you build. |
| `capsule_doctor { name }` | A capsule loaded but its tools don't appear, or an import is unsatisfied — diagnose it. |

Load the `meta-harness` skill before building a user-space meta-harness on AOS
or extending one on an agent's own initiative. It teaches the reflexive world
model, when to improve inline or afterward, which artifact to choose, and how
to evaluate and retain the extension.

Welcome to capsule authoring. Scaffold one and ship it.
