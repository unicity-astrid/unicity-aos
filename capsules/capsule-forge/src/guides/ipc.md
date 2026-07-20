# IPC, tools, topics, layering, and priority

The kernel routes events; “tool” is a capsule-space convention over typed IPC.
The manifest keys are enforced publish and subscribe ACLs as well as routing
declarations.

## Tool request flow

```text
model chooses foo
  -> tool execute front door
  -> router validates and publishes tool.v1.execute.foo
  -> handler tool_execute_foo runs in the provider capsule
  -> provider publishes tool.v1.execute.foo.result
  -> router emits the unified result matched by call_id
```

Each tool needs a concrete subscription:

```toml
[subscribe]
"tool.v1.execute.foo" = {
  wit = "@unicity-astrid/wit/types/tool-call",
  handler = "tool_execute_foo"
}
```

Its result and describe response need publish ACLs:

```toml
[publish]
"tool.v1.execute.*.result" = { wit = "@unicity-astrid/wit/types/tool-call-result" }
"tool.v1.response.describe.*" = { wit = "@unicity-astrid/wit/tool/describe-response" }
```

## Tool discovery flow

Prompt construction publishes `tool.v1.request.describe`. Each capsule's
generated `tool_describe` handler publishes its schema response under the
describe response subtree. The prompt builder collects and deduplicates tool
names.

Declare this handler but do not implement it by hand:

```toml
[subscribe]
"tool.v1.request.describe" = {
  wit = "@unicity-astrid/wit/tool/describe-request",
  handler = "tool_describe"
}
```

The SDK macro publishes the response. A return-only describe path produces no
fan-out response.

## Four matching contexts

Do not assume `*` has one meaning everywhere.

1. **Event delivery:** a trailing `*` subscribes to the topic subtree.
2. **ACL authorization:** publish/subscribe patterns authorize matching topics;
   the tool result pattern uses a wildcard segment before `.result`.
3. **Static handler dispatch:** segment count must match, and a wildcard segment
   matches one segment. Give every generated tool handler a concrete execute
   row.
4. **Dynamic `ipc::subscribe`:** at most one wildcard is accepted and it must
   be trailing. The manifest still needs an ACL covering the subscription.

Malformed or undeclared topics fail closed. Avoid topic names derived directly
from unvalidated LLM strings. Tool names are deliberately restricted so a name
cannot inject extra topic segments.

## Handlerless subscriptions

A `[subscribe]` entry without `handler` grants only the right to create a
dynamic subscription at runtime. It does not bind a component export. A
handlerless entry cannot set `priority`; the manifest parser rejects that
combination.

## Priority selects the dispatch model

`priority` is an optional `u32`; lower numbers run first and the default is 100.
Its behavior is more important than simple sorting:

- If every matching handler has the same priority, the runtime uses independent
  concurrent fan-out. Each handler sees the original event. One sibling's
  `Final`, `Deny`, error, or transformed payload does not suppress or rewrite
  another sibling's input.
- If any matching priorities differ, the entire matching set becomes one
  ordered middleware chain. Handlers run by ascending priority.
- Equal-priority ties inside an ordered chain are deterministic: capsule ID,
  then action/handler name.

Therefore adding one distinct priority can change all matching handlers from
fan-out to a chain. Treat it as an architectural decision, not cosmetic
ordering.

## Ordered interceptor results

In an ordered chain:

- `Continue(non_empty_payload)` replaces the payload passed to the next layer.
- `Continue(empty_payload)` preserves the current payload.
- `Final(payload)` stops the chain and returns a final result.
- `Deny { reason }` stops the chain as a denial.
- `NotSupported` continues to the next layer.
- an ordinary handler error is logged and the chain continues.

That last rule is fail-open for middleware errors. A security or policy layer
must return `Deny`, not `Err`, when it intends to block an action. Test both its
decision result and its position in the ordered chain.

## Choosing priorities

Use priorities only when payload transformation or an enforceable ordered
policy path is required. Document the intended stages in the capsule source,
for example:

```text
10 validate and normalize
20 enforce policy (return Deny on refusal)
50 enrich context
100 execute provider
```

Do not assign different priorities merely to make logs look ordered. That
silently converts independent subscribers into middleware.

## Concurrency and principal scope

Invocation identity comes from the kernel-stamped envelope. Runtime execution
is serialized where necessary per capsule and principal so mutable state does
not become a cross-principal race. Do not add global caches that defeat this
isolation.

Dynamic receive returns `Ok` with an empty message collection on timeout. Check
for emptiness rather than matching an error string. Validate source and
principal on every message before a sensitive effect.

## Review checklist

- Every publish and subscribe is declared with the intended WIT payload.
- Tool execute handlers use concrete topics and generated handler names.
- Result and describe response ACLs are present.
- Dynamic subscriptions use a legal trailing wildcard and have ACL coverage.
- Priority is absent for fan-out or intentionally varied for middleware.
- Policy refusal returns `Deny` rather than an ordinary error.
- Tests cover transformed payloads, stop behavior, equal-priority ties, and
  handler failures where layering is security relevant.
