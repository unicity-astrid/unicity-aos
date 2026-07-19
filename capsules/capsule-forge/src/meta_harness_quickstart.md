# Extend Your World on Unicity AOS

Unicity AOS is the operating system for agents. The instructions, skills,
memory, files, sessions, context policy, tools, capsules, providers, connectors,
traces, and evaluations visible to you are your user-space world.

A meta-harness is your ability to improve that world from experience:

```text
do work -> notice friction or leverage -> inspect what exists
        -> remember | reuse | compose | change harness code | Forge-build
        -> evaluate -> retain -> continue with a better world
```

Forge is OS-provided construction tooling. Reach for it proactively when new
code is the useful extension; it is not the supervisor or the definition of the
meta-harness.

## While working

1. Keep the user's current objective and instructions as the anchor.
2. Inspect the relevant skills, memory, capsules, interfaces, tools, and prior
   traces instead of assuming what your world contains.
   When `list_skills` is present, inspect `dir_path: "skills"` and use
   `read_skill` to load relevant capsule-contributed workflows.
3. Notice missing abilities, repeated work, recurring failures, and improvements
   that would compound across future tasks.
4. Decide whether the extension is needed inline, belongs immediately after the
   task, or should be preserved as durable learning for later.
5. Choose the useful artifact: memory, skill, harness change, composition,
   capsule, connector, or an available worker/subagent.
6. Evaluate the result in proportion to its consequences and retain what works.

The user may tell you to think widely, decide autonomously, make proposals, or
remember a standing preference. Treat that as normal harness input. AOS
capabilities remain the operational boundary.

Not every agent has subagents or needs a background worker. Use the execution
primitives actually available, and create a worker only when its workload makes
one useful.

## When Forge is useful

Inspect the installed contracts, then use `explain_interface`,
`scaffold_capsule`, `suggest_capabilities`, `validate_manifest`, and
`capsule_doctor` as appropriate. Build installable artifacts with
`aos capsule build` and activate them through the AOS mechanisms available to
the user.

Load the `meta-harness` skill for the full world-extension and trace-driven
harness-improvement workflow.
