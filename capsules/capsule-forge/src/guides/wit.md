# WIT contracts, imports, exports, and composition

WIT describes typed boundaries. It is both an ABI contract for host calls and
a vocabulary for payloads and capsule-provided services.

## Three surfaces that are easy to confuse

1. **Host imports:** `astrid:*` functions the WASM component calls through the
   SDK, such as filesystem, IPC, HTTP, process, identity, or time.
2. **Manifest imports/exports:** versioned interfaces a capsule requires from or
   provides to the installed composition.
3. **Topic payload references:** the `wit = ...` type attached to an IPC ACL
   row.

They reinforce one another but are not interchangeable. A topic convention can
exist without a manifest interface import, and declaring an import does not
grant a network or filesystem host capability.

## Standard tool payload references

```text
execute request   @unicity-astrid/wit/types/tool-call
execute result    @unicity-astrid/wit/types/tool-call-result
describe request  @unicity-astrid/wit/tool/describe-request
describe response @unicity-astrid/wit/tool/describe-response
```

Use these exact references for ordinary tool capsules. The SDK macro handles
serialization and result publication.

`wit = "opaque"` retains the topic ACL but waives payload type validation. It
is for protocol edges that genuinely forward unknown bytes, not a shortcut
around defining a known contract.

## Imports and exports

```toml
[imports]
"astrid:llm" = "^1.0"
"example:optional" = { version = "^1.0", optional = true }

[exports]
"example:search" = "1.0.0"
```

At load time the runtime resolves required imports against installed exports
and compatible versions. An unsatisfied required import prevents a valid
composition; an optional import lets the capsule boot and degrade deliberately.

Use `capsule_doctor` to compare an installed capsule's imports with available
providers. Do not infer that a similarly named tool satisfies a typed import.

## Inspect before inventing

Use the installed interface index:

- `list_interfaces` to discover available contracts;
- `read_interface` for the canonical text;
- `explain_interface` for raw WIT plus a short package/interface/record summary.

The installed mirror may contain a bundle file with several interfaces rather
than one `<interface>.wit` filename. Interface lookup should inspect declared
interface names as well as filenames.

When the current installation lacks a contract, inspect the pinned runtime/WIT
source used by the project. Do not rely on a remembered future interface.

## Authoring a new contract

Most new tool capsules should use existing contracts and do not need a local
WIT directory. A genuinely new public interface changes the ecosystem's typed
surface and belongs in the canonical WIT repository and RFC/review workflow.

Define:

- namespace, package, interface, and semantic version;
- request, response, stream, and error records;
- ownership of correlation IDs and principal attribution;
- cancellation, timeout, retry, and backpressure semantics;
- compatibility rules for additive and breaking changes;
- exact topic mapping when the contract is carried over IPC.

Then update SDK bindings and contract mirrors together. Generated WIT staging
is build output; canonical WIT source is not.

## Composition design

Prefer typed service boundaries when multiple capsules need a stable provider
contract. Prefer tool IPC for model-invoked operations. Prefer an ordinary
internal event topic for loosely coupled notifications that do not need a
provider interface.

The kernel remains service-blind: it resolves declared contracts, routes
events, and enforces authority. Provider selection, policy, retries, and domain
meaning belong in capsules.

## Compatibility checklist

- The manifest version requirement matches the WIT version actually consumed.
- Required and optional imports reflect real fallback behavior.
- Topic payload references name the correct record, not just the package.
- A public record change is additive or receives a new incompatible version.
- Principal identity is kernel-stamped metadata, not trusted payload data.
- Errors and timeouts have explicit semantics across both sides.
- Provider replacement is tested against the same contract, not just one
  implementation.
