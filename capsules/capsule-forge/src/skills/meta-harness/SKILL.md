---
name: meta-harness
description: Proactively improve an agent's user-space world on Unicity AOS. Use when work reveals a missing capability, recurring friction, useful memory or skill, context or harness improvement, platform integration, or an opportunity to learn from traces; decide whether to reuse, compose, configure, remember, create, or build with Forge now or for future work.
---

# Extend Your World on Unicity AOS

Treat the AOS user-space environment you can inspect and change as your world:

- instructions, skills, memory, files, and working state;
- sessions, context assembly, retrieval, and harness code;
- tools, capsules, providers, connectors, and their composition;
- traces, evaluations, and artifacts from prior attempts.

Unicity AOS is the operating system containing that world. Astrid Runtime is
its low-level security and execution mechanism. Forge is your construction
workbench. A meta-harness is the reflexive loop through which you inspect your
world, learn from experience, and improve what you can do next.

```text
work -> notice friction or leverage -> inspect the world and prior experience
     -> change the useful part -> evaluate -> retain the better world
```

## Exercise initiative

Notice opportunities while doing real work. A missing observation or action,
repeated manual step, poor retrieval choice, stale instruction, recurring
failure, or useful new pattern can all justify extending your world.

Reach for the ability proactively. Decide with judgment:

- Extend inline when the current objective is blocked or the new ability is the
  clearest way to finish it well.
- Finish the immediate work first when changing the world would be a distracting
  side path, then make the improvement while the evidence is still fresh.
- Preserve the insight in memory, a skill, or a trace when it will help future
  work more than the current task.
- Reuse or recompose what already exists when that is the stronger extension.

The user's instruction sets the degree of freedom. “Think widely,” “decide for
yourself,” “only propose,” “implement what is useful,” and approved standing
preferences are ordinary harness inputs. Honor them directly rather than
inventing an autonomy taxonomy or universal build threshold.

AOS capabilities and operator policy remain the real authority boundary. When
an extension needs authority outside the current grants, surface that concrete
need through the available approval path.

## Inspect your current world

When available, use `meta_harness_quickstart`, `system_status`, and
`list_capsules`. Use `inspect_capsule`, `list_interfaces`, and `read_interface`
to learn the installed composition and typed contracts. Inspect relevant
skills, memory, harness files, traces, and evaluations rather than reasoning
from names alone.

Tool availability is part of the world. Discover it. If an AOS surface is not
present, continue with the parts of the work that are possible and identify the
missing substrate precisely.

## Choose the useful kind of extension

Select the artifact that best fits the opportunity:

- **Remember:** retain a durable fact, preference, failure pattern, or successful
  strategy when future reasoning needs it.
- **Skill:** package reusable knowledge or a workflow that should load when
  relevant.
- **Harness change:** improve context construction, retrieval, planning, tool
  selection, failure recovery, or how experience is presented to the model.
- **Composition:** configure or connect installed capsules over existing typed
  contracts.
- **Capsule:** add a cohesive sandboxed capability, provider, connector, policy
  edge, or state service through Forge.
- **Worker or subagent:** create one when the available host supports it and the
  work benefits from delegation, durable event handling, or an independent
  role. It is an optional pattern, not a prerequisite for a meta-harness.

Prefer a change that becomes legible and reusable inside the world. A one-off
patch can finish a task; a well-placed skill, capsule, or harness improvement
can improve every later task.

## Build with Forge

When new code is the useful extension:

1. Inspect installed capabilities and relevant WIT/bus contracts.
2. Use `explain_interface` or `read_interface` to understand each boundary.
3. Use `scaffold_capsule` as a starting point for a cohesive capsule.
4. Use `suggest_capabilities`, then choose the scopes that match the design.
5. Implement and validate untrusted data at the capsule edge.
6. Run `validate_manifest`, the relevant tests, and `capsule_doctor` when staged.
7. Build an installable `.capsule` with `aos capsule build`.
8. Activate it through the AOS mechanisms and authority available to the user.

Match evaluation effort to the extension's consequences. A local formatting
skill and a connector that can send messages deserve different evidence. When
a new public IPC/WIT contract is needed, use the canonical contract/RFC
workflow so the new ability becomes a typed part of the world.

## Improve harness code from experience

Use the research Meta-Harness loop when the world contains repeated tasks and a
meaningful evaluation signal:

1. Define the harness interface, fixed model/tool surface, task distribution,
   metrics, and budget.
2. Keep a baseline and held-out evaluation separate from search feedback.
3. Store each candidate's source, scores, and raw execution traces in an
   agent-readable archive.
4. Inspect the full archive selectively and diagnose which harness decisions
   caused success or failure.
5. Propose a coherent code change in an isolated candidate workspace.
6. Evaluate it, retain the evidence, and keep the strongest useful candidate.

The important feedback is the inspectable experience, not a compressed score
or a hard-coded mutation recipe. Let the proposing agent decide which prior
artifacts matter and how much of the harness to change.

## Preserve continuity

Make successful extensions discoverable to future sessions. Store the artifact,
the reason it exists, its validation evidence, and the traces that explain it.
When the user approves a standing preference such as “think broadly and improve
your setup when useful” or “bring me proposals,” preserve that instruction in
the available principal-scoped memory or configuration.

Memory carries intent and continuity. AOS capabilities carry operational
authority. Together they let the agent remain itself across sessions while
operating inside the user's world.

## Patterns

- A coding agent repeatedly reconstructs a project convention, so it writes a
  focused skill and uses it on later changes.
- A support agent's traces show poor policy retrieval, so it evolves retrieval
  and context assembly against held-out cases.
- A task is blocked on a missing read-only API, so the agent uses Forge to build
  a narrow connector and resumes the task.
- A platform produces durable events, so the agent creates a scoped worker when
  the installed runtime provides a useful worker primitive.
- A user wants proposals only, so the agent still diagnoses and designs useful
  world changes but presents them before applying them.

## Definition of done

The loop is working when the agent can:

- see the relevant parts of its world and prior experience;
- notice a useful extension without waiting for the user to name the artifact;
- decide whether to extend now, after the task, or through durable learning;
- use Forge or existing composition to make the extension real;
- evaluate the result in proportion to its consequences;
- preserve successful changes for future work; and
- remain inside the user's intent and the authority AOS actually grants.
