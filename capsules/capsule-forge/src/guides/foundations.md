# Foundations: what belongs where

## The stack

Unicity AOS is an operating system for agents. It supplies a governed user
space in which agents can compose instructions, skills, memory, context,
models, tools, capsules, providers, connectors, workers, traces, and
evaluations.

Astrid Runtime is the low-level mechanism beneath AOS. It routes typed IPC,
loads and meters WASM Components, enforces manifest capabilities and topic
ACLs, scopes work to principals, stores state, and audits effects. Keep the
kernel simple. Domain logic and policy intelligence belong at capsule edges.

Forge is AOS's construction workbench. It helps an agent inspect contracts,
scaffold a capsule, choose capabilities, validate a manifest, and diagnose an
installation. Forge is part of a meta-harness; it is not the supervisor and it
does not grant its own output authority.

## The artifact vocabulary

- **Instruction or memory:** a durable fact, preference, constraint, or lesson.
- **Skill:** reusable instructions and workflow knowledge that load when the
  current work matches the skill description.
- **Harness:** the user-space system around a model: context assembly,
  retrieval, memory, tools, control flow, recovery, and evaluation.
- **Meta-harness:** the reflexive loop by which an agent notices friction or
  leverage, inspects its world, improves it, evaluates the change, and retains
  what works.
- **Capsule:** a cohesive sandboxed WASM Component that provides tools,
  services, connectors, policy edges, state, or typed contracts on AOS.
- **Composition:** configuration and IPC wiring among existing capsules.
- **Worker:** an optional persistent or delegated role when the installed host
  supports it and the workload benefits from one.
- **Host plugin:** a client-side distribution adapter for a host such as Codex
  or Claude. It may vendor skills, hooks, MCP configuration, and commands. It
  does not replace the AOS capsule or AOS authority model.

## Choose the smallest durable artifact

Use this order. Stop as soon as the need is met:

1. Discover and reuse an existing skill, tool, capsule, or contract.
2. Configure or compose installed parts.
3. Remember a durable fact or preference.
4. Write a skill for reusable reasoning or procedure.
5. Change harness code for context, retrieval, planning, or recovery.
6. Build a capsule when new executable behavior or a new governed boundary is
   actually required.
7. Add a host plugin only when a particular client needs native startup
   discovery, hooks, or an MCP adapter.

A prompt-only rule is not authorization. A host plugin is not a security
boundary. A capsule does not become trusted because an LLM generated it.

## Capsule architecture

A capsule targets `wasm32-unknown-unknown`, not WASI. Host effects go through
audited `astrid:*` WIT imports exposed by `astrid-sdk`. Its `Capsule.toml`
declares:

- components and packaged assets;
- host capabilities such as file, network, process, or identity access;
- `[publish]` and `[subscribe]` topic ACLs and handler bindings;
- typed imports and exports;
- install-time configuration, commands, MCP servers, and context files.

The effective authority is the intersection of the component's imports, the
manifest, the principal's grants, operator policy, and any runtime consent.
Missing authority fails closed.

## Composition rule

Prefer small capsules with cohesive authority. If one proposed capsule needs
unrelated file, network, process, identity, and prompt-injection grants, split
it unless those powers genuinely belong to one security boundary. Compose
capsules over typed IPC rather than embedding every capability in one binary.

## What to load next

- Read `workspace` before choosing where source should live.
- Read `capsule`, `manifest`, and `capabilities` before implementation.
- Read `ipc` for tools, handlers, middleware, layering, or priority.
- Read `skills` when distributing agent instructions or a host plugin.
- Read `authority` before installing, granting, or activating a new ability.
- Read `meta-harness` when the agent is extending its own user-space world.
