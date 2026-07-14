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

The supported installer installs both the `aos` product command and its pinned
runtime under the product-owned `~/.unicity-os` root:

```sh
curl --proto '=https' --tlsv1.2 -fsSL https://aos.unicity.ai/install.sh | sh
aos init
```

Re-running the installer performs a coordinated product upgrade without
rewriting a standalone runtime installation. Every release publishes
checksums, Sigstore bundles, GitHub build-provenance attestations, and
`runtime-compatibility.toml`, which pins the exact runtime release and WIT commit.

## Command boundary

AOS owns its product roots, including `init`, `status`, `migrate`,
`self-update`, and `serve-health`:

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

## Import an existing runtime

The `aos` CLI can deliberately copy compatible state from a standalone runtime
installation without changing the source. See
[Importing standalone runtime state](docs/runtime-migration.md) for the exact
allowlist, integrity checks, recovery behavior, and command.
