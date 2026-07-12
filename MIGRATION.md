# Migration ledger

This ledger records the provenance of every repository imported into Unicity
AOS. Do not delete source repositories or rewrite source commit identities as
part of an import.

| Source repository | Destination | Final source commit | Release tags | License | Status |
| --- | --- | --- | --- | --- | --- |
| `unicity-astrid/astralis` | `distros/community` | pending | pending | pending | planned |
| `unicity-astrid/capsule-cli` | `capsules/capsule-cli` | `e1e180a62f24d4f210c79d8330d625b28b4de3ce` | `v0.2.0` | MIT OR Apache-2.0 | imported |
| Remaining first-party `unicity-astrid/capsule-*` repositories | `capsules/<name>` | pending | pending | pending | planned |

Copied or local-only capsule directories require a source, license, and
ownership decision before import. The stale `capsule-anthropic` repository is
excluded and must not be revived.
