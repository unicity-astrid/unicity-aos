# Unicity AOS Meta Harness

Unicity AOS is already an agent harness: it supplies the model loop, sessions,
context, tools, policies, and sandboxed execution. It becomes a **meta harness**
when it can supervise durable platform workers and improve that harness from
evidence without allowing a model to grant itself power.

Forge is part of the meta harness. It is the construction arm:

```text
authorized platform event
  -> worker scoped to (principal, platform, account)
  -> use installed capabilities
  -> record a capability gap if blocked
  -> reuse -> compose -> configure -> Forge-build
  -> quarantine -> verify -> approve -> install -> observe/rollback
```

## Start

1. Call `system_status` and `list_capsules`.
2. Inspect relevant capsules and WIT contracts.
3. Define the platform worker's principal, account, standing intent, allowed
   actions, budgets, approval rules, and stop conditions.
4. Start or resume one logical background worker for that scoped platform using
   a real background-agent or durable AOS worker API.
5. Give the worker authenticated platform events, a bounded queue, persistent
   session state, an approval outbox, traces, and a kill switch.

Do not invent scopes, permissions, numeric budgets, or auto-build thresholds.
Until the user or operator accepts them, allow only read-only observation and
local drafts. Never fabricate setup commands or provider permissions that are
not visible through installed documentation or schemas.

If no durable worker API is installed, say so. A shell process is not an agent,
and a prompt promising future work is not proactive execution.

## When the worker is blocked

Capture the blocked goal, the installed capabilities checked, the smallest
missing contract, required side effects, acceptance tests, and least-privilege
manifest scope. Prefer an existing capsule or composition. If code is truly
missing, use Forge's `explain_interface`, `scaffold_capsule`,
`suggest_capabilities`, `validate_manifest`, and `capsule_doctor` tools.

Keep generated code quarantined until deterministic tests, negative permission
tests, platform replay fixtures, provenance, rollback, and any required operator
approval are complete. The proposer never promotes its own capability.

Load the `meta-harness` skill for the complete workflow and use cases.
