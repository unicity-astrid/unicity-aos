# AOS Community Edition

AOS Community Edition is the open agent operating system built on
[Astrid](https://github.com/astrid-runtime/astrid).

It owns the Community Edition product surface: the `unicity` CLI, HTTP API,
distributions, first-party capsules, provider and model experience, and
Unicity Audit. Astrid remains the portable capability-secure substrate beneath
it.

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
