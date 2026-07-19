# AOS Community Edition

AOS Community Edition is the open agent operating system for people who want
an inspectable, composable environment for agents.

It owns the Community Edition product surface: the `aos` CLI, HTTP API,
distributions, first-party capsules, provider and model experience, and
Unicity Audit.

## Workspace layout

```text
crates/       Product CLI, HTTP API, control client, and shared product code
capsules/     First-party production capsules
distros/      Community distribution manifests and release metadata
docs/         Product and operator documentation
```

## Install

The supported installer installs the `aos` product command, its pinned runtime,
and the exact 19 Community Edition capsules built from this source tree under
the product-owned `~/.aos` root:

```sh
curl --proto '=https' --tlsv1.2 -fsSL https://aos.unicity.ai/install.sh | sh
aos init
```

`aos init`, including `aos init --offline`, provisions from those local,
product-versioned capsule assets. Re-running the installer performs a
coordinated product upgrade without
rewriting a standalone runtime installation. Every release publishes
checksums, Sigstore bundles, GitHub build-provenance attestations, and
`runtime-compatibility.toml`, which pins the exact runtime release and WIT commit.
Its machine-readable runtime-compatibility and upgrade/self-heal gates must both
be true before a tag can publish. The latter is approved only after the exact
candidate preserves a frozen standalone-home clone and boots with freshly
generated runtime coordination state.

## Command boundary

AOS owns its product roots, including `init`, `status`, `migrate`, `update`,
`distro`, and `serve-health`:

```sh
aos status
aos status --json
```

Every other root inherits the bundled Astrid CLI transparently. Arguments, exit
codes, and signals pass through unchanged:

```sh
aos doctor
aos capsule build
```

An AOS-owned root intentionally shadows the runtime root with the same name.
Use the standalone runtime CLI when the raw command is required:

```sh
astrid status
astrid init --help
```

## Build on AOS

Unicity AOS is the operating system in which agents and agent-native software
run. Capsules are general user-space building blocks: users can compose them
into harnesses, meta-harnesses, connectors, services, or other systems.
Community Edition ships Forge as OS construction tooling so a fresh agent can
inspect the running system, learn the capsule model, identify a real capability
gap, and build and verify a least-privilege capsule. Forge also installs the
`meta-harness` skill, which teaches an agent how to build a governed
meta-harness on AOS by treating its instructions, memory, skills, harness code,
tools, capsules, traces, and evaluations as an improvable user-space world. The
agent is instructed to notice useful extensions during real work and reach for
Forge proactively when new code is the right way to improve that world.

See [Extending an agent's world on AOS](docs/meta-harness.md) for the world
model, research loop, Forge boundary, optional worker pattern, and
representative user experiences.

Provisioning another principal keeps the authenticated operator separate from
the target environment:

```sh
aos --principal operator init --target-principal alice
```

This AOS release fixes its distribution state to Unicity CE. Use a standalone
`astrid` installation and runtime home to apply another distribution. Homebrew
installations update with `aos update`. Direct installs resolve the signed
`stable` channel by default and can select `dev`, `nightly`, or an exact version;
all remain fail-closed until their signed metadata is actually published. See
[Signed AOS release channels](docs/release-channels.md).

## Import an existing runtime

The `aos` CLI can deliberately copy compatible state from a standalone runtime
installation without changing the source. See
[Importing standalone runtime state](docs/runtime-migration.md) for the exact
allowlist, integrity checks, recovery behavior, and command.
