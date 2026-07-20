# Capability reference and least authority

Every field fails closed. A list defaults to empty; a boolean defaults to
false. Capability declaration is necessary but may not be sufficient: the
principal, operator policy, or runtime consent can further restrict use.

## Complete field catalog

| Field | Type | Maximum authority declared |
|---|---|---|
| `uplink` | boolean | Act as a long-lived uplink and use attributed publish operations. Removes the ordinary WASM call timeout. |
| `net` | list | Outbound HTTP to listed hostnames or `"*"`. |
| `kv` | list | Reserved manifest surface; normal principal-and-capsule-scoped KV works without declaring it today. |
| `fs_read` | list | Read under listed VFS prefixes. |
| `fs_write` | list | Write under listed VFS prefixes. |
| `host_process` | list | Spawn the named host binaries. |
| `allow_persistent` | boolean | Permit host-owned processes that outlive a capsule instance, in addition to `host_process`. |
| `net_bind` | list | Bind listed raw socket endpoints. |
| `net_connect` | list | Connect to listed raw `host:port` endpoints. |
| `identity` | list | Identity operations at `resolve`, `link`, or `admin` level. |
| `allow_prompt_injection` | boolean | Permit hook output to modify the system prompt. |

Those are the complete current fields. Do not invent `shell`, `internet`,
`filesystem`, or a generic `admin` manifest capability.

## Examples

```toml
[capabilities]
fs_read = ["home://documents/"]
fs_write = ["home://data/my-capsule/"]
net = ["api.example.com"]
host_process = ["git"]
```

Prefer concrete domains, binaries, endpoints, and path prefixes. `"*"` is a
meaningful broad grant and should be presented that way during review.

## Filesystem rules

- `home://` is the calling principal's home and changes with invocation scope.
- `cwd://` is the runtime-provided capsule/workspace root.
- `"*"` is broad workspace-confined access, not the host's entire filesystem.
- Parent traversal is rejected even if a broad prefix is declared.
- `fs_read` does not imply write, and `fs_write` does not imply read.

Use a capsule-specific data directory for mutable files. Prefer KV for small
structured state and compare-and-swap for contested updates.

## Network rules

`net` gates the high-level HTTP client by hostname. It is distinct from raw
socket capabilities:

- `net_connect = ["host:443"]` grants raw outbound TCP to endpoints;
- `net_bind` grants listeners and is rare for bus-native capsules;
- HTTP, connect, and bind grants do not imply one another.

Normalize and validate user-provided URLs before the host call. Do not allow an
LLM argument to convert a narrow static domain grant into an open proxy.

## Process rules

`host_process` is an allowlist of executable names. It is a stronger boundary
than ordinary WASM because the host process runs outside the component sandbox.
Pass arguments as an argv vector; do not concatenate untrusted input into a
shell command.

Ordinary child processes are bounded and reaped with the capsule. A persistent
process additionally requires `allow_persistent = true` and deserves explicit
review because it can outlive one invocation or instance.

## Identity levels

Identity values form an increasing authority order:

```text
resolve < link < admin
```

Grant only the lowest required level. Treat caller-supplied identity strings as
claims until resolved or kernel-stamped. Never trust a payload field as the
principal merely because it looks like an ID.

## Prompt injection and uplink

`allow_prompt_injection` authorizes a capsule hook to affect system-prompt
construction. It is not needed for returning ordinary tool text. Review the
source and the exact hook path before enabling it.

`uplink` is for trusted long-lived protocol edges that publish with external
attribution. It changes timeout and attribution behavior; do not use it simply
to keep an ordinary task alive.

## KV caveat

The `kv` list exists in the manifest schema but is reserved and not the active
gate for normal SDK KV. Capsule KV is already isolated by capsule and principal.
Omit `kv` unless a version-specific contract explicitly requires it; do not
suggest a fictitious store name as if it created a new KV namespace.

## Capability selection workflow

1. List every host call the design will make.
2. Map each call to the exact manifest field.
3. Narrow lists to the smallest stable scope.
4. Separate unrelated authority into another capsule.
5. Use `suggest_capabilities` as a candidate generator, not an approval oracle.
6. Compare the new manifest with the installed version and present the delta.
7. Test denied paths as well as allowed paths.

Instructions and Skills can guide this selection; only the installed manifest,
principal grants, and policy make it operational.
