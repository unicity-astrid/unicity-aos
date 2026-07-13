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
docs/         Product and migration documentation
```

## Migration status

This repository is the destination for the existing first-party capsule and
distribution repositories. Each import must retain its source URL, final commit,
release tags, license, and artifact digest in [MIGRATION.md](MIGRATION.md).
