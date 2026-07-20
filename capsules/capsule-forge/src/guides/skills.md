# Skills, capsule distribution, and host plugins

## A Skill is instructions, not authority

A Skill teaches an agent when and how to perform a reusable workflow. It can
tell the agent to inspect AOS, use Forge, or call a tool; it cannot grant that
tool, filesystem access, network access, or permission to install anything.

Every `SKILL.md` starts with YAML frontmatter:

```markdown
---
name: example-skill
description: What the skill does and the situations in which it should load.
---

# Example skill

Operational instructions...
```

The description is the trigger surface. Put both the capability and the
relevant situations there. Do not hide “when to use this” only in the body,
because the body is read after selection.

## Write the Skill like a natural collaborator

- Start with the outcome and the mental model.
- Give a short default workflow.
- Route to exact reference material only when the current decision needs it.
- Prefer scripts or tools for deterministic mechanical work.
- State hard safety and authority boundaries explicitly.
- Keep mutable version facts in an inspectable reference or tool response.
- Include examples for ambiguous choices, not decorative prose.

The `capsule-forge` Skill follows this shape: its trigger and workflow remain
compact, while `forge_guide` serves detailed chapters on demand.

## Distribute a Skill in user space

Skills do not belong in Capsule.toml. The Astrid kernel and generic capsule
archive must not pin an AI instruction protocol.

Use one or both user-space paths:

- a host plugin vendors a Skill directory for native startup discovery;
- an agent-level Skills service indexes workspace and principal-home Skill
  directories and exposes list/read over IPC.

For the standard AOS Skills index, workspace entries take priority over
principal-home entries with the same ID. This lets a project specialize or
replace a durable user workflow without modifying a capsule.

If a capsule owns detailed knowledge, expose that knowledge through a typed bus
tool and let the host Skill act as the compact trigger. Forge follows this
pattern: the plugin can vendor `capsule-forge` and `meta-harness`, while the
installed capsule serves version-matched chapters through `forge_guide`.

A future product registry may advertise opaque provider/capability strings so
an agent can discover that a Skills service exists. The registry and kernel do
not need to understand SKILL.md, its frontmatter, or its loading protocol.

## Host plugin role

A Codex-, Claude-, or other host plugin can provide native startup discovery,
hooks, commands, and MCP configuration. It may vendor important first-party
Skills so a fresh host recognizes them before any dynamic lookup.

Treat that plugin copy as a distribution adapter:

- keep one canonical source or a deterministic generation path;
- version or cache-bust it according to the host's plugin rules;
- do not mutate a user's installed plugin directory when an ordinary plugin
  update can deliver the change;
- do not assume a host plugin can discover user-space Skills unless the host
  calls the AOS `list_skills`/`read_skill` bridge;
- do not confuse plugin installation with capsule installation or capsule
  grants.

## Explicit and implicit loading

An integrated host may select a Skill implicitly from its description, or the
user may name it explicitly. An agent-level AOS service can expose dynamic
discovery:

1. call `list_skills` with `dir_path: "skills"`;
2. choose a relevant ID from its name and description;
3. call `read_skill` with the same directory and ID;
4. follow the loaded instructions within current capabilities and policy.

The meta-harness reflex is proactive discovery: when work exposes a missing
capability or recurring friction, inspect the skill index before reinventing a
workflow.

## Design checklist

- Does the description clearly say what and when?
- Can an agent follow the default path without prior AOS knowledge?
- Are detailed facts progressively disclosed rather than always loaded?
- Will every referenced asset exist after plugin/user-space installation?
- Is the Skill free of machine-specific paths and private repository state?
- Does it distinguish instructions from operational authority?
- Is user override precedence intentional?
