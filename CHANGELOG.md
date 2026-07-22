# Changelog

## [2026.1.3] - Unreleased

### Added

- The `aos` product command and product-owned `~/.aos` state boundary.
- A pinned Unicity CE distribution manifest over Astrid Runtime 0.10.4, emplaced
  as the bundled runtime's operator-enforced distro.
- Reproducible macOS and Linux release bundles with primary BLAKE3 and
  Homebrew-compatible SHA-256 checksum manifests, Sigstore bundles, GitHub
  build-provenance attestations, and explicit runtime/WIT compatibility
  metadata.
- An idempotent product installer and updater that preserve runtime state while
  replacing the coordinated AOS and Astrid executable set and atomically
  installing the product-versioned Community Edition capsule set.
- Schema-3 runtime-import receipts with canonical `blake3:<hex>` content digests
  and fail-closed rejection of pre-release SHA-256 receipts.
- Runtime import holds the standalone daemon's existing singleton lock without
  changing the source, and interrupted unreceipted cutovers always roll back
  before recopying the current locked source.
- A signed release path for the 21 installable `aos-*` artifacts
  built from this source tree and selected locally by Community Edition, with
  exact source/manifest identity checks, product-archive inclusion, offline
  provisioning, archive safety validation, BLAKE3 checksums, SHA-256
  compatibility checksums, Sigstore bundles, and provenance.
- Host-target unit-test coverage for the capsule workspace.
- Forge in the default Community Edition distribution, including a discoverable
  bootstrap and skill that teach fresh agents to build a user-space
  meta-harness on AOS by seeing instructions, memory, skills, harness code,
  tools, capsules, traces, and evaluations as an improvable world. Agents reach
  for Forge proactively when real work reveals a useful new capability, while
  optional workers remain a use-case choice rather than a prerequisite.
- An authenticated `aos hook` ingress and Community Edition Meta Harness
  capsule that normalize Codex, Claude, and Grok host events onto the generic
  hook bus, collect private same-turn prompt context, and route it back only to
  the exact originating session. Adaptive, propose, automatic, and off modes
  preserve the agent's judgment while respecting its existing authority.
- Homebrew formula updates initiated by the tap's authenticated stable-release
  poll, eliminating the cross-repository dispatch credential.
- Strict, signed stable/dev/nightly channel and immutable release metadata
  contracts with exact workflow identities, expiry, replay-resistant generation
  state, and fail-closed direct installer resolution.
- A native release gate that initializes a clean AOS home, verifies the exact
  21-capsule CE lock, grants, and ready set, repeats initialization without
  changing runtime state, and proves clean daemon shutdown before publication.
- Native `aos status` output for authenticated running state and verified
  stopped state without invoking the runtime CLI.
- An opt-in daily nightly train with deterministic run-dated versions, exact
  Astrid compatibility pins, protected publication and promotion, and
  idempotent recovery after interrupted release or pointer updates. It is
  disabled by default; merging `main` never publishes a release.

### Changed

- Own the host-facing `aos mcp serve` command, MCP server identity, constrained
  interaction fallback, and `aos-mcp` broker capsule in AOS CE. Hosts with MCP
  form elicitation continue to render their own approvals; hosts such as Grok
  fall back to AppKit on macOS, native confirmation on Windows, or Pinentry on
  Linux. Free-form and secret-shaped fields are refused by this bridge.
- Pin the exact public root-command inventory of the bundled runtime and fail
  validation when a runtime update adds or removes a verb before AOS classifies
  it as inherited, product-owned, or shared. Runtime verbs remain direct
  `aos <verb>` commands without a nested runtime namespace.

- Keep agent Skills out of `Capsule.toml` and the generic capsule release
  contract. Host plugins may vendor trigger Skills, the AOS Skills service
  indexes workspace and principal-home entries, and capsules expose detailed
  guidance over ordinary IPC tools without teaching the runtime an AI-specific
  file protocol.
- Make Capsule Forge a progressively disclosed, exhaustive AOS author manual:
  its compact Skill now routes fresh agents into installed reference chapters
  covering portable source placement, all manifest capabilities, IPC
  layering/priority, WIT, Skills and host plugins, construction-versus-activation
  authority, build/release practice, security, and proactive meta-harness design.
- Target the Telegram capsule's actual `aos-telegram` package name in CI so the
  WASI job and target-specific workspace exclusions run as intended.
- Parse AOS-owned commands with Clap-generated validation and help while
  preserving byte-for-byte delegation of inherited runtime commands and their
  help surfaces.
- Initialize against the operator-enforced Community Edition manifest and ask
  the runtime to grant its installed capsule set to the resolved target
  principal.
- Bootstrap the default CE system fleet through Astrid's canonical distro
  installer before the daemon-backed authorization pass, allowing a completely
  fresh AOS home and non-default targets to use the same runtime trust path.
- Keep the authenticated init operator separate from its target principal,
  prevent AOS distribution replacement, and fail closed while signed direct
  update channels remain unpublished.
- Require explicit machine-readable runtime-compatibility and upgrade/self-heal
  approvals before the tag-triggered workflow can package or publish a release,
  backed by a packaged migration/reinstall test over the frozen 2026-07-15
  Astrid 0.9.4 home shape and a final-candidate runtime boot hook.
- Present product-facing capsule copy consistently as Unicity AOS while
  preserving stable Astrid Runtime crate, WIT, topic, artifact, and ABI names.
- Treat the runtime's expected shutdown-response disconnect as a successful
  `aos stop` only after every coordination marker is gone and the singleton
  lock is available; all other inherited runtime failures retain their output
  and exit status.

### Removed

- The vendored `capsules/capsule-telegram` copy. The capsule is maintained in
  its own repository at `unicity-aos/capsule-telegram` and installs directly
  from there with `aos capsule install @unicity-aos/capsule-telegram`, so the
  in-tree duplicate had no consumer: it was absent from `Distro.toml` and
  `release/community-capsules.txt`, and was therefore never built, signed, or
  shipped. Keeping it only invited the drift it had already accumulated —
  it was the sole workspace member still pinned to `astrid-sdk` 0.5.3 and
  building for `wasm32-wasip1`, which is why CI had to exclude it from every
  workspace job. Those exclusions and the `telegram-wasi` job go away with it.
