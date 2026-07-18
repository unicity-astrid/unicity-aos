# Changelog

## [2026.1.1] - Unreleased

### Added

- The `aos` product command and product-owned `~/.aos` state boundary.
- A pinned Unicity CE distribution manifest over Astrid Runtime 0.10.1, emplaced
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
- A signed release path for the 18 installable `aos-*` artifacts
  built from this source tree and selected locally by Community Edition, with
  exact source/manifest identity checks, product-archive inclusion, offline
  provisioning, archive safety validation, BLAKE3 checksums, SHA-256
  compatibility checksums, Sigstore bundles, and provenance.
- Host-target unit-test coverage for the capsule workspace.
- The installable `aos-linux-realm` seed: principal-owned durable home storage,
  an Astrid copy-on-write workspace projection, and bounded nested-WASM `pwd`,
  `echo`, `write-file`, and `cat` commands with no host-process authority.
- Crash-consistent `aos-linux-realm` home generations: a principal-scoped atomic
  KV head selects immutable BLAKE3-addressed file and manifest blobs, with
  concurrent-writer retry, corruption checks, daemon-restart recovery, and lazy
  migration from the original direct-home format.
- The host-testable `aos-realm-core` semantic kernel with monotonic process and
  pipe identities, explicit process transitions, direct-child wait/reap, typed
  terminal signals, deterministic FIFO admission, atomic descriptor inheritance,
  bounded pipe backpressure/EOF/broken-pipe behavior, and aggregate quotas.
- A signed `pipe-echo` realm workload that runs two isolated Wasmi process stores
  through the core scheduler and a four-byte stdout-to-stdin pipe, exercising
  partial writes, read/write suspension, wakeup, EOF, and exact output accounting.
- A long-lived `aos-linux-realm` service actor with one isolated Realm machine per
  kernel-verified principal, monotonic per-boot process identities, CAS-allocated
  boot sequences, bounded aggregate principal admission, foreground resource
  reaping, and live process/pipe accounting through the existing tool protocol.
- A private Realm `pipe`/`spawn-signed`/`wait`/`signal` ABI and signed
  `guest-pipe-echo` workload, with generation-checked process handles, bounded
  descendant admission, pre-partitioned request budgets, unified file/pipe
  descriptor allocation, deterministic foreground-tree cleanup, and bounded
  call-ID replay protection against duplicate mutating transport delivery.
- A versioned record-oriented signed-spawn ABI with bounded argv and environment
  vectors, absolute immutable-catalog resolution, multiple exact descriptor
  mappings, atomic parent-endpoint close actions, and the guest-side `realm-sh`
  workload for direct `echo`, environment, and `echo | cat` foreground jobs.
- Homebrew formula updates initiated by the tap's authenticated stable-release
  poll, eliminating the cross-repository dispatch credential.
- Strict, signed stable/dev/nightly channel and immutable release metadata
  contracts with exact workflow identities, expiry, replay-resistant generation
  state, and fail-closed direct installer resolution.
- A native release gate that initializes a clean AOS home, verifies the exact
  18-capsule CE lock, grants, and ready set, repeats initialization without
  changing runtime state, and proves clean daemon shutdown before publication.
- Native `aos status` output for authenticated running state and verified
  stopped state without invoking the runtime CLI.
- An opt-in daily nightly train with deterministic run-dated versions, exact
  Astrid compatibility pins, protected publication and promotion, and
  idempotent recovery after interrupted release or pointer updates. It is
  disabled by default; merging `main` never publishes a release.

### Changed

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
