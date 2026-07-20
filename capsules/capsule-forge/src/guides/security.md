# Security, failure behavior, limits, and evaluation

## Trust boundaries

Treat all of these as untrusted unless the kernel or operator proves otherwise:

- LLM tool arguments and prompt text;
- capsule manifests and packaged asset paths;
- external API responses, files, and platform events;
- identity strings carried inside payloads;
- generated source and suggested capabilities;
- another capsule's ordinary data output.

Kernel-stamped principal attribution, verified content identity, manifest gates,
topic ACLs, principal grants, and consent are the enforceable boundaries.

## Validate at the capsule edge

- Reject empty, malformed, or oversized values before host calls.
- Reject `/`, `\\`, `..`, NUL, schemes, or absolute paths where an argument is
  intended to be one safe component.
- Parse URLs and compare normalized hosts against the intended domain set.
- Pass process arguments as argv; never interpolate untrusted text into a shell.
- Bound loops, recursion, collection size, response size, and retry count.
- Authenticate kernel-stamped caller identity for sensitive operations.
- Avoid reflecting secrets into errors, logs, tool output, or traces.

The VFS and host gates still enforce their boundary; local validation gives the
model a clear failure and protects domain invariants before the host call.

## Fail closed deliberately

Host capability and ACL checks fail closed. Capsule logic must do the same when
it is the policy decision point.

In an ordered interceptor chain, returning an ordinary error logs the problem
and continues. A policy capsule that intends to stop the request must return
`InterceptResult::Deny { reason }`. Test this behavior; an `Err` is not a denial.

Optional imports and best-effort background work are legitimate only when the
degraded behavior is explicit and safe.

## Current resource boundaries to design around

Limits are runtime-versioned; inspect the pinned source before relying on an
exact number. Current author-relevant defaults include bounded filesystem call
sizes and directory listings, a cap on dynamic subscriptions, bounded
concurrent host processes, WASM execution time/epoch interruption, and bounded
tool-description collection.

Design as though every external resource is finite:

- stream or page large data;
- cap message and trace retention;
- apply timeouts to request/response flows;
- use compare-and-swap or idempotency keys for retries;
- deduplicate platform events;
- surface backpressure rather than spawning unbounded work;
- checkpoint long-lived work at safe boundaries.

`uplink` and persistent processes relax ordinary lifetime assumptions and
therefore need stronger shutdown, ownership, and recovery design.

## Principal isolation

State, home paths, environment, and secrets are selected per invocation. Never
cache principal-specific values globally. Avoid mutable global state for data
that belongs in principal-scoped KV.

For cross-principal services, make the shared/operator scope explicit and keep
authorization separate from lookup. A caller claiming another principal in a
JSON field has no authority to act as them.

## Supply chain and installation

- Build installable `.capsule` archives through the official builder.
- Reject unsafe archive paths, links, duplicate portable names, and undeclared
  assets.
- Keep source, artifact, checksum/provenance, installed bytes, and running
  identity traceable.
- Do not retag or silently replace public release bytes.
- Treat host processes and MCP servers as explicit sandbox exits.
- Review lifecycle hooks because they run during install or upgrade.

## Evaluation proportional to consequence

A local formatting Skill, a read-only connector, an identity administrator,
and a message-sending platform worker need different proof.

At minimum evaluate:

- functional success on representative cases;
- invalid, malicious, and denied inputs;
- authority scope and negative capability tests;
- principal isolation;
- restart, retry, duplicate, and timeout behavior;
- upgrade from supported prior state;
- effects on the real harness and held-out tasks;
- logs/traces without secret leakage.

For a meta-harness search, keep baseline, candidate source, task distribution,
metrics, costs, and raw traces. Do not optimize on the held-out evaluation or
claim general improvement from one anecdotal task.

## Adversarial review questions

- What happens if the model supplies a traversal path or open-proxy URL?
- Can a payload spoof a principal or provider?
- Does an error accidentally continue a policy chain?
- Can retries duplicate an external effect?
- Can one principal's config leak through a static cache?
- Can a broad wildcard be narrowed?
- Does a host process escape the intended executable/argument boundary?
- What state survives a crash halfway through an upgrade?
- Are generated Skills or plugins instructing an action the OS has not granted?
- Is every completion claim supported by the exact artifact tested?

Security is the composition of real gates and well-designed edges, not a claim
in a prompt.
