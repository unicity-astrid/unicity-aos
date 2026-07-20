# Complete Capsule.toml author surface

The manifest is untrusted declarative input and the runtime's maximum authority
description. The normal required minimum is `[package]` plus at least one
`[[component]]`.

## Package

```toml
[package]
name = "my-capsule"
version = "0.1.0"
description = "One cohesive capability"
authors = ["Name <email@example.com>"]
astrid-version = ">=0.7.0"
license = "MIT OR Apache-2.0"
repository = "https://example.invalid/repo"
publish = false
```

Names use lowercase ASCII alphanumeric characters and hyphens. Versions are
three-part numeric semantic versions. `astrid-version` constrains runtime
compatibility; do not loosen it without testing the actual host ABI.

## Components

```toml
[[component]]
id = "my-capsule"
file = "my_capsule.wasm"
type = "executable"
```

The file is relative to `Capsule.toml`; Rust hyphens become underscores in the
WASM filename. Most capsules contain one executable component. A library
component supplies composition without an independent run loop.

Do not author content digests in the manifest. Install records and verifies
content-addressed component bytes.

## Host capabilities

```toml
[capabilities]
fs_read = ["home://input/"]
net = ["api.example.com"]
```

Read the `capabilities` guide for every accepted field and its type. Omitted
lists are empty and omitted booleans are false.

## IPC ACL and handlers

```toml
[publish]
"tool.v1.execute.*.result" = { wit = "@unicity-astrid/wit/types/tool-call-result" }
"tool.v1.response.describe.*" = { wit = "@unicity-astrid/wit/tool/describe-response" }

[subscribe]
"tool.v1.execute.hello" = {
  wit = "@unicity-astrid/wit/types/tool-call",
  handler = "tool_execute_hello"
}
"tool.v1.request.describe" = {
  wit = "@unicity-astrid/wit/tool/describe-request",
  handler = "tool_describe"
}
```

The keys are both routing declarations and enforced topic ACLs. Values accept
a short WIT string or a table containing `wit`; subscribe tables may also have
`handler` and `priority`.

A handlerless subscribe entry is ACL-only for dynamic `ipc::subscribe`. It may
not set priority. Read `ipc` before using wildcards or priority.

## Typed imports and exports

```toml
[imports]
"astrid:llm" = "^1.0"
"astrid:optional-service" = { version = "^1.0", optional = true }

[exports]
"example:search" = "1.0.0"
```

Imports express typed provider requirements; exports advertise interfaces a
capsule provides. A pure tool convention does not automatically require an
import/export. Optional imports permit boot without a provider. Uplink capsules
cannot declare imports.

## Install-time environment

```toml
[env]
API_KEY = { type = "secret", request = "API key", placeholder = "sk-..." }
REGION = { type = "select", enum_values = ["us", "eu"], default = "us" }
TAGS = { type = "array", request = "Comma-separated tags" }
NAME = { type = "text", request = "Display name", default = "Agent" }
```

Supported author types are `secret`, `select`, `array`, and `text`. Secrets are
stored through the runtime's protected principal-scoped path and exposed only
to the owning capsule. Non-secret values use principal-scoped capsule config.
Manifest authors cannot set operator-only sharing scope.

Read values with `env::var` at invocation time. Never cache them globally.

## Context files

```toml
[[context_file]]
name = "project-guidance"
file = "AGENTS.md"
```

Context files contribute named static context. Use them for always-relevant
capsule context, not a large workflow that should trigger selectively as a
Skill.

## Commands

```toml
[[command]]
name = "example"
description = "Run the example workflow"
kind = "cli"
```

Commands may be slash commands or top-level capsule CLI verbs. A CLI command is
provider-scoped over IPC; the kernel registers and routes it but does not
interpret its payload. Names must satisfy the runtime's command grammar and
must not collide with built-ins.

## MCP servers

```toml
[[mcp_server]]
id = "example"
description = "Legacy host MCP bridge"
type = "stdio"
command = "example-server"
args = ["--stdio"]
```

A stdio MCP server starts a host process and therefore requires an appropriate
`host_process` allowlist entry. Prefer a native WASM capsule and typed IPC when
possible; host MCP is an explicit airlock boundary.

## Uplinks and other declarations

`[[uplink]]` describes an external platform uplink and pairs with the `uplink`
capability. `[[tool]]` and `[[topic]]` are legacy/declarative surfaces; current
Rust tools normally come from macros plus `[publish]`/`[subscribe]`.

## Author review

- Package, Cargo crate, component filename, and artifact identity agree.
- Every packaged file is safe and relative.
- Every tool has a concrete handler subscription and the required result and
  describe publish ACLs.
- Every dynamic subscription is covered by the subscribe ACL.
- Capabilities are the narrowest useful scopes.
- Imports are satisfied or deliberately optional.
- Agent Skills stay in their host/plugin or user-space service rather than the
  kernel-facing capsule manifest.
- Secrets are install configuration, not literals in source or manifest.
- Run `validate_manifest` and `aos capsule check` before building.
