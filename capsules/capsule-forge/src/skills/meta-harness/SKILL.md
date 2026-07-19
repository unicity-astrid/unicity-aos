---
name: meta-harness
description: Build and operate a governed Unicity AOS meta-harness. Use when connecting an external platform, creating or supervising a background worker, responding to a repeated capability gap, composing installed capsules, extending AOS through Forge, or improving an agent harness from traces and evaluations.
---

# Unicity AOS Meta Harness

Treat Unicity AOS as the governed environment around agents, not as a prompt
wrapper. Compose existing capsules first. Use Forge only when evidence shows a
real capability is missing. Never let a candidate capability promote itself.

## Know the system

- **Astrid Runtime** is the mechanism: route IPC, enforce manifest capability
  allowlists, isolate WASM components, meter resources, and audit actions.
- **Unicity AOS** is the product harness: agent loop, sessions, context, models,
  tools, skills, platform uplinks, distribution, and operator experience.
- **The meta harness** is the control loop above those parts: give each
  authorized platform a durable worker, detect gaps, reuse or compose existing
  capabilities, ask Forge to build what is missing, verify the candidate, and
  promote it only through policy and approval.
- **Forge is the construction arm, not the supervisor.** It explains contracts,
  scaffolds capsules, suggests manifest capabilities, validates manifests, and
  diagnoses installations. It must not choose user goals, grant itself power,
  or install an unverified candidate.

The kernel stays tool-blind and free of business logic. Put reasoning in agent
workers, platform protocol handling in connector capsules, and lifecycle policy
in a supervisor capsule.

## Orient before acting

1. Call `meta_harness_quickstart` when available.
2. Call `system_status` and `list_capsules`.
3. Use `inspect_capsule`, `list_interfaces`, and `read_interface` to inspect the
   installed system and its typed contracts.
4. Record the principal, platform, account/workspace, standing user intent,
   allowed actions, approval rules, budget, and stop conditions.
5. Verify that the platform is authorized and that its connector stamps caller
   identity from trusted transport metadata rather than payload text.

Do not invent repository/account scope, platform permissions, numeric budgets,
retention periods, replay windows, concurrency, or a threshold for automatic
capability building. Read configured policy or present proposed values and wait
for acceptance. While those values are unset, fail closed to read-only
observation and local drafts: no external writes, new authority, capability
installation, or unattended spending.

Do not assume a tool, interface, worker API, scheduler, or approval path exists.
Inspect it. If durable agent spawning is absent, report that substrate gap. A
background shell process is not a background agent.

## Scope platform workers correctly

Use one logical worker for each:

```text
(principal, platform, platform-account-or-workspace)
```

Examples are one GitHub worker for Alice's organization, one Telegram worker
for Alice's bot identity, or one email worker for Alice's mailbox. Do not use a
global worker across principals or accounts. Do not create one permanent worker
per incoming message.

Give every worker:

- a stable session identity and resumable state;
- only the connector tools and capsules required for that platform;
- an inbox of kernel-stamped platform events and a bounded work queue;
- a standing objective derived from explicit user intent;
- action, cost, concurrency, and time budgets;
- an approval outbox for consequential actions;
- observable traces, verification evidence, and a kill switch.

If the active LLM harness exposes a native background-agent API, start or resume
the scoped worker through that API and pass this contract in its instructions.
Otherwise, use a durable AOS worker/session API only if inspection proves one is
installed. Never claim proactive execution merely because a prompt describes a
worker.

## Handle each event

For every authorized platform event:

1. Verify the principal, platform account, event identity, and replay boundary.
2. Deduplicate and enqueue it for the matching worker.
3. Restore that worker's session and standing intent.
4. Plan with currently installed capabilities.
5. Execute reversible actions within standing authority.
6. Route consequential, external, financial, destructive, or scope-expanding
   actions to approval.
7. Persist the result, evidence, and any capability gap.

Proactive means responding to authorized events and schedules without waiting
for another chat message. It does not mean inventing goals or silently widening
authority.

## Resolve a capability gap

Create a gap record only after an attempted task provides evidence. Include:

- the blocked goal and platform;
- the missing observation or action;
- the installed capsules and interfaces already checked;
- the smallest required inputs, outputs, and side effects;
- acceptance tests and replay fixtures;
- the narrowest capability and topic scopes;
- whether user approval is required.

Resolve in this order:

1. **Reuse an installed tool or capsule.**
2. **Compose** installed capsules over existing WIT/bus contracts.
3. **Configure** an existing connector or provider without changing authority.
4. **Build** a small capsule with Forge only if the first three cannot satisfy
   the contract.

Repeated use alone is not proof that a new capability is needed. A new capsule
must remove a demonstrated block and have a cohesive security boundary.
“Repeated” means the operator-configured threshold; never make up a number. If
no threshold exists, record the gap but do not start an autonomous build.

## Build with Forge

When a build is justified:

1. Use `explain_interface` or `read_interface` for every relevant contract.
2. Use `scaffold_capsule` for the smallest cohesive capsule.
3. Use `suggest_capabilities` and then narrow the result manually.
4. Keep credentials inside the platform/provider capsule. The model must not
   receive raw keys or bearer tokens.
5. Validate all untrusted platform data at the capsule edge.
6. Run `validate_manifest` before building.
7. Build an installable `.capsule` with `aos capsule build`; raw WASM is not an
   installable artifact.
8. Run unit tests, platform replay fixtures, denial tests, and the acceptance
   test from the gap record.
9. Use `capsule_doctor` on the staged installation.

If a new public IPC/WIT contract is required, stop capsule implementation and
follow the contract/RFC workflow. Do not disguise a new wire contract as an
opaque payload.

Use installed connector documentation or inspected schemas for platform setup.
Do not fabricate AOS commands, API fields, provider permissions, webhook rules,
or credential-store procedures that are not visible in the system.

## Quarantine and promote

Keep generated capabilities outside the active user distribution until all of
these exist:

- source and dependency provenance;
- a manifest capability diff;
- deterministic build output or recorded build identity;
- positive acceptance tests and negative authorization tests;
- replay results from real, redacted platform traces;
- a rollback or uninstall path;
- explicit operator approval for new authority or external side effects.

Install and grant through AOS/runtime mechanisms. Never hand-copy WASM, edit a
principal's grants, or ask the model to treat a prompt as authorization. Monitor
error rate, denied actions, cost, and user corrections after promotion. Revoke
or roll back on regression.

## Improve the harness itself

Keep optimization separate from live operation:

```text
episode traces -> failure attribution -> candidate harness change
               -> held-out evaluation -> policy review -> approval -> rollout
```

Treat prompts, retrieval, skills, worker topology, tool selection, and context
packing as harness code. Preserve complete traces and scores for prior
candidates. Compare against a fixed baseline and held-out cases. Reject changes
that gain task score by increasing authority, hiding failures, leaking data, or
removing approval friction. Never let the proposer evaluate or promote its own
candidate without independent checks.

## Use-case patterns

- **GitHub:** A per-organization worker triages issues and review queues. A
  repeated project-specific validation gap can produce a narrow lint capsule.
- **Email:** A per-mailbox worker classifies and drafts. Sending, deleting, or
  changing subscriptions remains approval-gated; credentials stay in the mail
  connector.
- **Telegram:** A per-bot/account worker maintains conversational continuity and
  hands work to calendar, research, or file capsules without giving Telegram
  those capabilities directly.
- **Commerce:** A storefront worker monitors orders and drafts resolutions.
  Refunds and price changes require explicit policy and approval.
- **Local operations:** A device worker watches authorized health signals and
  proposes remediation. New diagnostic tooling is staged and replay-tested
  before it can run on the host.

## Definition of done

Do not call a platform meta-harnessed until all are true:

- the worker scope and user intent are explicit;
- platform events are authenticated, deduplicated, and replay-bounded;
- worker state survives a frontend disconnect;
- budgets, approval routes, audit evidence, and stop controls work;
- an unmet capability produces a structured gap rather than improvised power;
- Forge candidates are tested in quarantine and cannot self-promote;
- rollback is proven;
- a fresh agent can rediscover this flow from installed tools and this skill.
