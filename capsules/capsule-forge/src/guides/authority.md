# Initiative, authority, consent, and activation

## The central distinction

The agent can exercise judgment inside its user-space world. AOS still decides
what effects are possible. Treat these as separate questions:

1. Is the extension useful?
2. May the agent create or edit a candidate here?
3. May it compile the candidate?
4. May it install or replace the capsule?
5. May this principal use the capsule?
6. May this particular runtime effect occur?

Answering yes to an earlier question does not answer the later ones.

## When no extra permission is normally needed

If the user asked to build or change something, reversible source edits and
local compilation inside the authorized workspace are ordinary implementation
steps. Do not ask for a second ceremonial approval for each file.

When a missing ability is discovered proactively, an agent may inspect the
world, design the extension, and create an isolated candidate in writable
user-space when that is consistent with the user's instructions and host
policy. The agent decides whether to do it inline, after the immediate task, or
retain the idea for later.

The user can also set a standing preference such as “improve your setup when
useful” or “only bring me proposals.” Preserve and honor that instruction as
harness input. It does not manufacture OS authority.

## Boundaries that require real authority

Do not silently cross these boundaries merely because source was generated:

- installing, updating, replacing, or removing a capsule;
- granting a principal persistent access to a capsule;
- expanding manifest capabilities or IPC ACLs on an existing installation;
- starting persistent host processes or workers;
- writing outside the scoped workspace or AOS user-space boundary;
- sending messages, spending money, publishing, deploying, or changing an
  external system;
- persisting a new standing instruction in memory when the user has not
  authorized that kind of memory update.

Proceed when the user's current request, an existing standing instruction, or
an AOS/operator policy clearly authorizes the action. Otherwise stop at the
candidate and request the missing authority.

## Present the authority delta

Before activation, make the proposed change legible. Report the exact delta:

- capsule identity and artifact source;
- host capability keys and scopes (`net`, `fs_read`, `host_process`, and so on);
- publish and subscribe topic patterns;
- typed imports and exports;
- install hooks, contributed commands, MCP servers, and context files;
- any separate host-plugin or user-space Skill changes;
- configuration and secret prompts;
- persistent-process, uplink, identity, or prompt-injection privileges;
- whether the capsule grant is persistent and which principal receives it.

Do not summarize a broad network grant as merely “API access,” or a host
process grant as merely “automation.” Name the actual domains, binaries, paths,
topics, and persistence.

## The activation sequence

```text
source review
  -> compile and test
  -> build installable .capsule
  -> inspect authority delta
  -> install or update
  -> principal grant / first-use approval
  -> per-action or egress consent when required
  -> observe and evaluate
```

Installation means the runtime has accepted and placed the artifact. It does
not necessarily mean every principal may invoke it. A client may elicit a
capsule grant on first use; explicit acceptance persists that access, while
decline, cancellation, missing elicitation support, or error must deny it.

Some host effects, including governed egress paths, can have a further
per-action consent choice such as once, for the session, or always. This is
separate from the capsule grant and from the manifest's maximum capability.

## Generated code cannot self-promote

Forge can produce source and an artifact. It cannot approve its own manifest,
grant itself to a principal, bypass policy, or turn an instruction into a
cryptographic capability. This is a feature: an agent can be creative and
proactive without making the authority boundary imaginary.

## How to speak to the user

Keep presentation proportional:

- A small reversible skill edit can be mentioned as part of normal work.
- A candidate capsule should be introduced by the problem it solves and its
  authority delta.
- A proposal-only instruction means design and evidence now, activation later.
- If the current task is blocked on the new ability, explain that link and
  continue through every already-authorized step.

The goal is neither constant permission prompts nor invisible self-modification.
It is understandable initiative inside real authority.
