# Build a meta-harness from first principles

## The agent's world

For an agent on Unicity AOS, “my world” means the user-space state it can
inspect and improve:

- instructions, skills, memory, files, and working state;
- sessions, context assembly, retrieval, planning, and recovery;
- tools, capsules, providers, connectors, and composition;
- optional workers and platform sessions;
- traces, evaluations, source, and prior candidate artifacts.

The model weights are not the only thing that makes the agent capable. The
harness around the model determines what it sees, remembers, invokes, and
learns from.

## The reflexive loop

```text
do real work
  -> notice friction, failure, missing ability, or reusable leverage
  -> inspect the current world and prior experience
  -> reuse | configure | remember | write a Skill | change harness | Forge-build
  -> evaluate the change
  -> retain the useful result
  -> continue in a better world
```

The magic is the proactive reach: the agent need not wait for the user to name
“a capsule” or “a skill.” It notices what would improve its world and selects
the appropriate artifact. The user's current objective and instructions remain
the anchor.

## Start from what exists

1. Inspect `system_status`, installed capsules, tools, and interfaces.
2. Discover `skills` with `list_skills`; load relevant entries with
   `read_skill`.
3. Inspect memory, harness files, traces, and previous evaluations relevant to
   the current friction.
4. Prefer reuse or composition when the world already contains the needed
   ability.
5. Identify the exact missing observation, action, knowledge, state, or control
   behavior before building.

Not every agent has subagents, and a background worker is not the definition of
a meta-harness. Use the execution primitives actually present. A worker is
useful for independent, durable event handling or delegation; an ordinary
inline capsule or Skill may be the right extension elsewhere.

## Decide when to extend

- Extend inline when the current task is blocked or the extension is the
  clearest route to a good outcome.
- Finish the task first when extension would be a distracting side path, then
  improve the world while the evidence is fresh.
- Preserve an insight in memory, a Skill, or a trace when its main value is
  future reuse.
- Produce a proposal and candidate only when the user has constrained changes
  to approval.

This is agent judgment, not a product-wide autonomy switch. “Think widely,”
“decide for yourself,” “implement useful improvements,” and “bring me
proposals” are normal harness inputs.

## Build a new executable ability

When a capsule is the right artifact:

1. Load `foundations` and `workspace` from `forge_guide`.
2. Inspect the installed WIT and topic contracts; do not invent a parallel
   protocol if a typed one exists.
3. Load `capsule`, `manifest`, `capabilities`, and `ipc` as needed.
4. Scaffold in the owning repository or an isolated candidate workspace.
5. Implement one cohesive boundary and validate untrusted inputs at its edge.
6. Run manifest checks, compile checks, tests, and adversarial cases.
7. Build an installable `.capsule`.
8. Present the exact authority delta described in the `authority` chapter.
9. Install and grant only through authority already supplied by the user or
   operator.
10. Observe the result, record evidence, and retain or revert based on what it
    actually improves.

Generated code never self-promotes. Proactivity chooses and constructs a useful
candidate; AOS capabilities, principal grants, consent, and operator policy
still control activation.

## Evolve harness code from experience

For repeated tasks with a meaningful evaluation signal:

1. Define the harness interface, fixed model/tool surface, task distribution,
   metrics, and budget.
2. Keep a baseline and held-out evaluation separate from search feedback.
3. Archive each candidate's source, scores, costs, and raw traces.
4. Let the proposing agent selectively inspect the complete archive.
5. Diagnose which harness decisions caused success or failure.
6. Make a coherent change in an isolated candidate workspace.
7. Evaluate it and retain the strongest useful candidate.

Raw inspectable experience matters more than a compressed score alone. Scores
rank; traces explain. Keep claims bounded to the evaluations actually run.

## Platform examples

- A coding agent repeatedly reconstructs repository conventions, so it writes
  a focused Skill and lets future sessions load it from the workspace.
- A support agent's traces show poor policy retrieval, so it changes context
  assembly and evaluates held-out tickets.
- A task is blocked on a read-only API, so the agent builds a narrowly scoped
  connector capsule, presents the domain grant, and resumes after activation.
- A platform produces durable events, so an agent creates one worker per
  principal/platform/account only when the host supports workers and the event
  model benefits from them.
- A user wants proposals, so the agent still discovers, designs, scaffolds, and
  evaluates useful candidates but does not install or grant them.

## Definition of done

The meta-harness is real when a fresh agent can see its world, proactively
notice a valuable extension, choose the right artifact, find a portable place
to build it, evaluate it, preserve it for later sessions, and remain inside the
user's intent and AOS's actual authority.
