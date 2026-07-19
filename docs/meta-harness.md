# Meta-harness architecture

## Decision

Unicity AOS is the user-facing agent harness. Astrid Runtime supplies the
security and execution substrate beneath it. AOS becomes a meta harness by
adding a governed control loop that supervises platform-scoped agents and can
improve its own composition from measured failures.

Forge is necessary but is not the whole meta harness. Forge is the capability
construction subsystem. It gives an agent the knowledge and tools to inspect
contracts, scaffold a capsule, select manifest capabilities, validate the
manifest, and diagnose the installed artifact. The surrounding control plane
owns worker lifecycle, gap evidence, evaluation, approval, promotion, audit,
and rollback.

This terminology combines two established uses of “meta-harness”:

- An agent harness is the runtime around a model that drives tool calls,
  persistence, context, approvals, observability, background delegation, and
  completion. See [Microsoft's agent harness description][ms-harness] and
  [OpenAI's Codex harness architecture][codex-harness].
- The Meta-Harness research system is an outer loop that searches over harness
  code using prior candidates, scores, and traces. See the [paper][meta-paper]
  and [reference implementation][meta-code].
- Product meta-harnesses also provide a common governed layer over different
  agent implementations. [Omnigent][omnigent] is a current example.

Unicity should support heterogeneous models and frontends, but its distinctive
advantage is stronger: generated capabilities remain ordinary AOS capsules
behind kernel-enforced identities, manifests, IPC ACLs, budgets, approvals, and
audit.

## System boundary

```text
Platform API / local event source
          |
          v
Platform capsule ------------------------------------------------+
  protocol, credentials, trusted identity, dedupe, rate limits   |
          | kernel-stamped platform event                         |
          v                                                       |
Harness supervisor                                                |
  worker registry, queue, budgets, gap ledger, promotion state   |
          |                                                       |
          v                                                       |
Worker session: (principal, platform, account)                    |
  intent, context, planning, tool use, approval requests          |
          |                                                       |
          +---- existing capsules / typed bus composition --------+
          |
          +---- capability gap ----> Forge ----> candidate capsule
                                          |            |
                                          v            v
                                      evaluation -> quarantine
                                                       |
                                      operator policy/approval
                                                       |
                                                       v
                                              registry / installer
                                                       |
                                             observe or roll back
```

The boundaries are deliberate:

- The kernel routes and enforces. It does not decide what users want or what a
  capability means.
- A platform capsule owns protocol details and secrets. It emits normalized,
  authenticated events and performs allowed platform actions. The LLM never
  receives raw credentials.
- A worker owns reasoning for one principal and platform account. It cannot
  silently share memory or authority with another worker.
- The supervisor owns lifecycle and policy state, not platform business logic.
- Forge builds small capabilities. It cannot approve, grant, or promote them.
- The installer and runtime remain the only authority for capsule activation
  and principal grants.

## Worker identity and lifecycle

The durable worker key is:

```text
(principal_id, platform_id, account_or_workspace_id)
```

This provides continuity without conflating trust domains. Multiple chats in
one Telegram identity can share platform knowledge while retaining distinct
conversation sessions. Two GitHub organizations, two mailboxes, or two users
never share a worker merely because the provider is the same.

One logical worker does not require one permanently scheduled model process.
Persist its session and queue, then activate it on an authorized event or
schedule. The supervisor may use short-lived task executors underneath, but the
worker identity, budgets, memory, and audit stream remain stable.

Lifecycle states should be explicit:

```text
disconnected -> connected -> active -> paused -> revoked
                               |
                               +-> gap-recorded -> candidate-staged
                                                   -> awaiting-approval
                                                   -> active | rejected
```

Required controls:

- user-owned standing intent and stop conditions;
- maximum concurrency, spend, elapsed time, and external actions;
- queue bounds, replay protection, and idempotency keys;
- pause, inspect, resume, and revoke operations;
- async approval routing that survives frontend disconnects;
- per-worker traces and outcome metrics.

Unset policy fails closed. A worker may observe authorized read-only events and
prepare local drafts, but it cannot infer repository/account scope, platform
permissions, numeric budgets, retention or replay windows, external-write
authority, or automatic-build thresholds. The user or operator must configure
or accept those values.

“Proactive” means the worker can act on authorized platform events or schedules
without another chat turn. It does not authorize self-chosen goals.

## Capability acquisition loop

An agent should not generate code merely because it can. A capability gap is a
typed, auditable record containing:

- blocked objective and platform;
- observed failure and attempts already made;
- installed capsules and interfaces inspected;
- smallest missing input/output/action contract;
- side effects and required authority;
- acceptance test and representative replay fixtures;
- expected reuse frequency and owner.

The decision order is:

1. Reuse an installed capability.
2. Compose installed capsules over current contracts.
3. Configure an existing capability without widening authority.
4. Build a cohesive capsule through Forge.

Forge's current tools cover the first authoring loop:

- `forge_quickstart`
- `scaffold_capsule`
- `explain_interface`
- `suggest_capabilities`
- `validate_manifest`
- `capsule_doctor`
- `meta_harness_quickstart`

Future Forge work should add a sandboxed test harness, trace replay, capability
diffs, reproducible build evidence, and candidate bundles. Those are Forge
functions because they construct and prove an artifact. Worker registration,
scheduling, approval, and promotion remain supervisor functions.

## Candidate safety and promotion

Generated capabilities begin quarantined. Promotion requires:

1. source and dependency provenance;
2. an exact manifest capability and IPC ACL diff;
3. deterministic build identity;
4. functional acceptance tests;
5. negative tests proving denied authority stays denied;
6. redacted platform-event replay;
7. bounded resource and cost measurements;
8. rollback/uninstall proof;
9. explicit approval for new authority or consequential effects.

Evaluation and promotion must be independent of the proposer. A candidate must
not improve its score by hiding failures, changing the test set, increasing its
own permissions, or weakening approvals.

## Harness improvement loop

Once the operational control plane produces trustworthy episode traces, AOS can
apply the research meaning of Meta-Harness:

```text
baseline harness + episode archive + objective
  -> proposer creates one candidate change
  -> candidate runs against training episodes
  -> independent evaluator runs held-out episodes
  -> compare task quality, authority, cost, and reliability
  -> human/policy approval
  -> canary -> rollout or rollback
```

Candidate surfaces include prompt assembly, skill routing, memory retrieval,
context compaction, tool selection, worker topology, and failure recovery.
Kernel security rules, identity provenance, approval requirements, and audit
integrity are constraints, never optimization variables.

OpenAI's harness-engineering account reinforces the practical prerequisites:
repository-local knowledge, agent-legible observability, isolated worktrees,
mechanically enforced architecture, and feedback loops that turn recurring
failures into durable capabilities. See [Harness engineering][openai-harness].

## Representative user experiences

### Developer organization

A GitHub worker watches authorized repositories, classifies issues, prepares
small fixes, and requests review. When several tasks fail on the same internal
schema check, it records the gap. Forge builds a narrow validation capsule,
replays the failing cases, and stages it. The organization approves the new
read scope before activation.

### Personal communications

Separate email and Telegram workers maintain their own platform context but can
compose through user-granted calendar and memory capsules. They draft freely
within policy. Sending mail, deleting messages, or inviting attendees crosses
an approval boundary. Provider credentials never enter model context.

### Storefront operations

A commerce worker monitors orders and support messages, prepares responses, and
identifies repeated carrier-data failures. Forge can add a read-only carrier
adapter. Refunds, price changes, and customer-data exports remain separate
capabilities with explicit approval.

### Local and private operation

A device worker responds to authorized local health events, collects bounded
diagnostics, and proposes remediation. If a new diagnostic is necessary, it is
built and tested in a constrained realm before host-process authority is
considered.

### Harness optimization

A user opts into improvement experiments. Redacted episodes show the email
worker repeatedly retrieves too much history. A proposer changes retrieval and
compaction, an independent evaluator runs held-out mailboxes, and AOS rolls out
only if task quality improves without increasing disclosure, cost, or authority.

## Delivery sequence

This implementation establishes the discoverable foundation:

1. Ship Forge in Community Edition rather than leaving it source-only.
2. Install the `meta-harness` skill and expose `meta_harness_quickstart`.
3. Keep the worker/supervisor contract explicit and honest about missing
   durable spawn support.

The next runtime increment should define a narrow supervisor/worker contract in
the canonical WIT repository and implement one end-to-end platform (Telegram or
GitHub) with durable worker state, async approval, and a structured gap ledger.
The optimization loop should follow only after those traces and evaluations are
trustworthy.

[codex-harness]: https://openai.com/index/unlocking-the-codex-harness/
[meta-code]: https://github.com/stanford-iris-lab/meta-harness
[meta-paper]: https://arxiv.org/abs/2603.28052
[ms-harness]: https://learn.microsoft.com/en-us/agent-framework/agents/harness
[omnigent]: https://docs.databricks.com/aws/en/omnigent/
[openai-harness]: https://openai.com/index/harness-engineering/
