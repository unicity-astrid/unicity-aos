# Extending an agent's world on Unicity AOS

## Decision

Unicity AOS is the operating system for agents. It is not itself a harness.
Harnesses are user-space systems built and run on AOS from agents, instructions,
skills, memory, state, models, tools, capsules, providers, and connectors. AOS
can host harnesses, meta-harnesses, and other agent-native software.

Astrid Runtime is the pinned low-level security and execution mechanism beneath
AOS. It routes IPC, enforces capabilities, manages the WASM sandbox, meters
resources, and audits actions. Those are the hard system boundaries; they do
not decide how imaginative or proactive an agent should be inside them.

The useful Meta-Harness idea is reflexivity: an agent can inspect the world
around itself, learn from prior work, and improve that world. Forge is the
agent-facing construction workbench for changes that require a new capsule or
other code. It is part of the available world, not the whole meta-harness.

## The agent's world

For an agent running on AOS, “my world” includes everything outside its fixed
model weights that shapes what it can perceive and do:

```text
Unicity AOS
|-- Astrid Runtime: isolation, capabilities, IPC, metering, audit
|-- OS services: identity, sessions, registry, policy
`-- agent user space
    |-- instructions, skills, memory, files, working state
    |-- context assembly, retrieval, planning, harness code
    |-- tools, capsules, providers, connectors
    |-- optional workers and platform sessions
    |-- traces, evaluations, and prior candidate artifacts
    `-- Forge: inspect, scaffold, validate, diagnose, build
```

Memory is continuity, but it is not the entire world. A useful extension might
be a remembered preference, a new skill, a better retrieval strategy, a changed
tool composition, a capsule, a connector, or an optional worker.

A meta-harness makes this world legible and changeable to the agent:

```text
do work
  -> notice friction, a missing ability, or reusable leverage
  -> inspect the current world and prior experience
  -> change the useful part
  -> evaluate the result
  -> retain the better world for later work
```

## What the research concluded

The Meta-Harness research system does not prescribe background agents or an
autonomy-mode state machine. It searches over harness code around a fixed base
model. A coding-agent proposer receives filesystem access to the source, scores,
and raw execution traces of all prior candidates, decides what to inspect, and
proposes a new harness. Each candidate is evaluated and its complete experience
is added back to the archive. See the [paper][meta-paper] and [reference
implementation][meta-code].

The central result is that raw, selectively inspectable experience is a much
better feedback channel than scores or compressed summaries. The outer loop is
deliberately small:

```text
baseline harness + task distribution + metrics + budget
  -> agent inspects prior source, scores, and traces
  -> agent proposes a coherent harness change
  -> candidate is evaluated
  -> source, score, and traces are archived
  -> repeat and retain the useful frontier
```

This is most useful for repeated or long-horizon work with a stable evaluation
signal, a fixed model/tool surface, useful historical traces, and a held-out
test set. Open-ended subjective improvement can still use ordinary agent
judgment, but it is not the same empirical search problem.

## Initiative comes from the agent and user

The user already steers initiative through normal instructions: “think widely,”
“decide for yourself,” “bring me proposals,” “implement what is useful,” or
“remember this preference.” Those instructions are part of the harness. An
approved standing preference can persist through the agent's memory or
configuration without becoming a new product-level autonomy taxonomy.

The agent should reach for extension proactively while doing real work. The
current objective remains the anchor:

- If a missing ability blocks the objective, extending the world can be the
  direct way to finish the task.
- If an improvement is valuable but would derail the immediate work, the agent
  can finish first and make or preserve the improvement while its evidence is
  fresh.
- If the value is primarily future reuse, memory, a skill, or an archived trace
  may be the right extension.
- If the world already contains the ability, discovery and composition are
  better than creating a duplicate.

This is judgment, not a mandatory threshold. AOS permissions remain the
operational ceiling. A natural-language instruction can guide the agent's
choices but cannot manufacture a capability the OS has not granted.

## Extension surfaces

### Memory and skills

Persist facts, preferences, failure patterns, and successful strategies when
future reasoning needs them. Turn a repeated workflow or domain-specific method
into a skill so it becomes available when relevant rather than occupying every
prompt.

Capsules contribute durable skills by writing valid
`home://skills/<id>/SKILL.md` files for the principal. The `aos-skills` service
indexes that directory generically: `list_skills` returns metadata and
`read_skill` loads the selected workflow. This works for user-authored capsules
as well as the Community Edition fleet. Host plugins can vendor important
first-party skills for native startup discovery and offline operation, while
the AOS index carries skills installed after the plugin was published.

### Harness code

Improve context construction, retrieval, prompt assembly, planning, tool
selection, memory updates, context compaction, or failure recovery. Preserve
the baseline and traces so the effect can be evaluated rather than guessed.

### Capsule composition

Inspect installed capsules and typed contracts before creating code. A useful
new behavior may come from configuring or composing existing providers,
connectors, state services, and tools over the event bus.

### New capabilities through Forge

When the world genuinely needs new code, Forge supports the authoring loop:

- `forge_quickstart`
- `meta_harness_quickstart`
- `scaffold_capsule`
- `explain_interface`
- `suggest_capabilities`
- `validate_manifest`
- `capsule_doctor`

The agent inspects relevant contracts, implements the cohesive capability,
validates its manifest, runs evidence appropriate to its consequences, builds
an installable `.capsule`, and activates it through the AOS mechanisms and
authority available to the user. New public IPC/WIT belongs in the canonical
contract/RFC workflow.

Future Forge work can make this loop stronger with isolated candidate
workspaces, trace replay, capability diffs, reproducible build evidence, and
candidate bundles. These help the agent construct and compare changes; they do
not decide the agent's goals.

### Workers and platforms

A worker is one optional extension pattern. Event-driven platforms may benefit
from a durable worker scoped to a principal and platform account:

```text
(principal_id, platform_id, account_or_workspace_id)
```

The connector owns protocol details, credentials, trusted identity,
deduplication, and rate limits. The worker owns the continuing task context.
Not every agent host has subagents, and not every task benefits from delegation
or a permanently represented worker. The agent uses the execution primitives
actually present in its world and creates a worker when the workload makes it
useful.

## Evaluation and retention

World changes should become inspectable experience. Record the relevant source,
reasoning, task outcome, score or qualitative evidence, cost, and trace. Match
the evaluation to the change: a local instruction edit, a retrieval algorithm,
and a connector that sends external messages have different consequences and
need different evidence.

For research-style harness search, keep search feedback separate from held-out
evaluation and let the proposer inspect full prior artifacts. For ordinary
agent work, tests, user feedback, later task performance, or a direct comparison
may be enough. The goal is to retain a demonstrably more useful world, not to
force every improvement through the same ceremony.

## Representative experiences

### Self-extending developer agent

While fixing several repositories, the agent repeatedly reconstructs the same
project convention. It writes a focused skill, uses it during the current task
if useful, and makes it available to future sessions. When a task later needs a
missing schema validator, it uses Forge to build a narrow capsule rather than
continuing to duplicate ad hoc checks.

### Proposal-oriented user

The user says, “Explore improvements, but bring them to me before changing the
setup.” The agent still inspects traces and designs world changes proactively;
it presents the candidate because that is the user's standing instruction, not
because AOS imposed a special proposal mode.

### Broadly autonomous user

The user says, “Think widely and improve your setup when it helps.” The agent
may add a skill, tune retrieval, recompose capsules, or build a capability while
pursuing the user's objectives. AOS capabilities bound the effects without
micromanaging how the agent reasons inside that boundary.

### Platform integration

An authorized Slack or GitHub workload benefits from durable event handling, so
the agent composes a connector and worker using the runtime primitives actually
available. If a repeated task exposes a missing action, the agent can extend
that world through Forge. The platform worker is a use case, not the definition
of the meta-harness.

### Harness optimization

Episode traces show that an agent repeatedly retrieves too much history. A
proposer changes retrieval and compaction, evaluates candidates on search
episodes, and checks the strongest candidates on held-out episodes. The useful
change becomes part of the agent's world.

## Delivery sequence

This implementation establishes the discoverable foundation:

1. Ship Forge in Community Edition.
2. Install the `meta-harness` skill and expose `meta_harness_quickstart`.
3. Teach agents to see AOS user space as their world and reach for extension
   during real work.
4. Keep user instructions and memory as the natural steering surface while AOS
   capabilities remain the hard boundary.

The next product increment should make the world more legible and testable: a
unified inventory of harness artifacts, durable trace/evaluation archives,
isolated candidate workspaces, and reusable evaluation runners. Durable
platform workers are a valuable separate extension when a use case needs them.

[meta-code]: https://github.com/stanford-iris-lab/meta-harness
[meta-paper]: https://arxiv.org/abs/2603.28052
