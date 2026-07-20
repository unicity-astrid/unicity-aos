---
name: capsule-forge
description: Author and debug Unicity AOS capabilities from zero. Use when choosing between a Skill, harness, capsule, composition, worker, or host plugin; creating or locating capsule source; writing Rust SDK code, Capsule.toml capabilities and IPC ACLs, WIT contracts, shipped Skills, installable artifacts, or Forge workflows; reasoning about priority/layering, installation, grants, consent, or portable activation.
---

# Build naturally on Unicity AOS

Unicity AOS is the operating system for agents. Astrid Runtime supplies the
sandbox, typed IPC, principal scope, capability enforcement, metering, and
audit. Intelligence belongs in user space: instructions, skills, memory,
harnesses, capsules, providers, connectors, and their composition.

Forge is the construction workbench. Use it when work needs a new executable
ability or when an existing capsule must be understood, repaired, or extended.

## Start with the artifact, not the code

Choose the smallest durable answer:

- discover or configure an existing ability;
- remember a fact or standing preference;
- write a Skill for reusable reasoning or procedure;
- improve harness code for context, retrieval, planning, or recovery;
- compose existing typed services;
- build a capsule for new governed executable behavior;
- add an optional worker only when the host supports it and durable independent
  work makes it useful;
- add a host plugin only for client-native discovery, hooks, commands, or MCP
  adaptation.

Load the `meta-harness` Skill when the opportunity arose proactively from real
work or the agent is improving its own user-space world.

## Load the manual progressively

Call `forge_guide` with no topic to list the exhaustive author manual. Then load
only the chapters required by the current decision. A host plugin may vendor
the same chapters under `references/<topic>.md`; use that offline snapshot when
the Forge tool is unavailable.

| Topic | Load when |
|---|---|
| `foundations` | choosing an artifact or explaining AOS versus Astrid, capsules, harnesses, and plugins |
| `workspace` | deciding where source belongs on this machine or in a repository |
| `capsule` | writing Rust macros, SDK calls, lifecycle hooks, state, or principal-scoped behavior |
| `manifest` | authoring any Capsule.toml section or packaged asset |
| `capabilities` | selecting or reviewing host authority and VFS scope |
| `ipc` | tools, topics, wildcard matching, handlers, fan-out, middleware, layering, or priority |
| `wit` | imports, exports, typed payloads, providers, or a new contract |
| `skills` | shipping instructions, progressive references, precedence, or a host plugin |
| `authority` | deciding whether to draft, install, replace, grant, persist, or activate |
| `build` | scaffold, check, build, install, test, diagnose, upgrade, remove, or release |
| `security` | adversarial review, failure behavior, limits, isolation, or evaluation |
| `meta-harness` | building a proactive self-extension loop from the ground up |

Do not claim the compact Skill is the whole manual. Forge's guide chapters are
the authoritative detailed path. Host-plugin references are a distribution
snapshot, not mutable runtime state or a machine-specific source location.

## Default capsule workflow

1. Inspect repository instructions, the current workspace, installed capsules,
   relevant Skills, and existing WIT/topic contracts.
2. Load `foundations` and `workspace`; choose the owning repository or an
   isolated candidate workspace.
3. Scaffold with `aos capsule new <name> [--path <parent>]` or
   `scaffold_capsule`.
4. Load `capsule`, `manifest`, and the exact boundary chapters needed.
5. Implement one cohesive capability. Validate untrusted data at its edge.
6. Use `suggest_capabilities` as a draft, narrow every scope, then call
   `validate_manifest` and run `aos capsule check`.
7. Run formatting, compile checks, unit/integration tests, and denied-path
   tests proportional to the extension.
8. Build an installable artifact with `aos capsule build`.
9. Load `authority`, present the exact authority delta, and install/grant only
   through authority the user or operator actually supplied.
10. Exercise it in the real harness, observe traces, and preserve what worked.

## Portable source placement

Follow the current repository's established capsule layout. For a standalone
project, `aos capsule new` writes under the current directory by default and
`--path` selects a parent.

If an agent proactively creates a candidate and no durable owner can be
inferred, use an isolated writable scratch directory or worktree. Build and
evaluate there, then ask where it should live before moving, committing,
installing, or publishing it when that choice changes user scope.

Never develop inside an installed runtime tree or plugin cache. Never hardcode
a particular user's home or repository path.

## Authority model

Keep these states distinct:

```text
idea -> source candidate -> compiled artifact -> installed capsule
     -> principal grant -> per-action consent -> observed result
```

If the user asked for implementation, reversible source edits and compilation
inside the authorized workspace are normal work. Proactive agent judgment can
also create an isolated candidate when consistent with the user's standing
instructions.

Generated code does not self-promote. Installation, replacement, persistent
principal grants, broader capability/ACL changes, external effects, publishing,
and memory changes still require the authority supplied by the user, AOS, and
operator policy.

Before activation, name the actual capability paths/domains/binaries/endpoints,
publish and subscribe topics, imports/exports, install hooks, secrets, host
processes, persistence, uplink, identity, prompt-injection changes, and any
separate host-plugin or user-space Skill changes.

## Non-negotiable capsule boundaries

- Target `wasm32-unknown-unknown`, not WASI.
- Use `astrid-sdk`; host effects cross audited `astrid:*` WIT boundaries.
- Treat `[publish]` and `[subscribe]` keys as enforced ACLs.
- Read caller identity from kernel-stamped context, never a payload claim.
- Keep environment, home paths, secrets, and state principal-scoped; do not
  cache them globally.
- Build `.capsule` archives with `aos capsule build`; raw WASM is not
  installable.
- Keep Skills out of Capsule.toml. Distribute trigger instructions through the
  host plugin or an agent-level Skills service; expose capsule-owned detailed
  guidance over IPC when appropriate.
- Use `Deny`, not an ordinary error, when ordered policy middleware must stop a
  request.
- Keep public WIT changes in the canonical contract/RFC workflow.

## Layering warning

Priority is not just sort order. Equal-priority matching handlers use
independent concurrent fan-out. If any priorities differ, all matches become an
ordered middleware chain. In that chain lower priorities run first,
`Continue` can transform the next payload, `Final` and `Deny` stop, and ordinary
errors continue. Load `ipc` before setting any priority.

## Tools

- `forge_quickstart`: smallest compiling tool-capsule path.
- `forge_guide`: exhaustive, progressively loaded author chapters.
- `meta_harness_quickstart`: compact proactive world-extension loop.
- `scaffold_capsule`: complete starter files as a path/content map.
- `explain_interface`: inspect installed WIT by filename or declared interface.
- `suggest_capabilities`: candidate manifest scopes from an intent.
- `validate_manifest`: lint common manifest and tool-bus errors.
- `capsule_doctor`: diagnose installed wiring and unsatisfied imports.

Use the tools to inspect and construct. Use tests and AOS authority to decide
whether the result is correct and may become active.
