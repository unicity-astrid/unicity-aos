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
- A versioned Linux Realm path-identity contract separating semantic mount IDs,
  guest paths, Astrid resource URIs, and human display paths. Execution and
  status responses distinguish the Linux guest's invocation-mounted workspace,
  principal-generation home, and boot-local temporary storage.
- A bounded Linux workspace portal: a GPL `trans=aos` 9P transport crosses a
  private experimental SBI request into the Rust Realm machine and 9P server,
  then resolves only the current invocation's Astrid `cwd://` COW capability.
  PID 1 remounts it before every shell call so FIDs cannot cross invocation
  boundaries.
- Crash-consistent `aos-linux-realm` home generations: a principal-scoped atomic
  KV head selects immutable BLAKE3-addressed file and manifest blobs, with
  concurrent-writer retry, corruption checks, daemon-restart recovery, and
  bounded migration from the original direct-home format.
- A second bounded 9P/SBI channel mounts those same principal-home generations
  at Linux `/home/agent`. Linux create, positional write, truncate, directory,
  rename, unlink, and flush operations select complete generations, and the
  home survives clean guest shutdown and cold boot while the initramfs root
  remains disposable.
- The host-testable `aos-realm-core` semantic kernel with monotonic process and
  pipe identities, explicit process transitions, direct-child wait/reap, typed
  terminal signals, deterministic FIFO admission, atomic descriptor inheritance,
  bounded pipe backpressure/EOF/broken-pipe behavior, and aggregate quotas.
- A signed `pipe-echo` realm workload that runs two isolated Wasmi process stores
  through the core scheduler and a four-byte stdout-to-stdin pipe, exercising
  partial writes, read/write suspension, wakeup, EOF, and exact output accounting.
- A principal-affine `aos-linux-realm` service with one resident Wasmtime Store
  and semantic Realm machine per kernel-verified principal, monotonic per-boot
  process identities, CAS-allocated boot sequences, an inner owner guard,
  foreground resource reaping, and live process/pipe accounting through direct
  metered tool entry points.
- A principal-resident Linux lifecycle inside `aos-linux-realm`: Linux 6.18.39
  remains alive in evictable per-principal RAM, accepts bounded framed console
  commands across separately metered tool invocations, preserves userspace state,
  shuts down cleanly through SBI, and restarts lazily without host-process
  authority.
- Principal-resolved AOS Realm resource envelopes for guest RAM, interpreted
  steps, and captured output, bounded by Astrid's admin-owned principal profile;
  status distinguishes configured and active limits, while a changed envelope
  cold-reconfigures only that principal's warm Linux machine.
- A reproducibly pinned Buildroot 2026.05.1, static musl, and BusyBox `ash`
  workbench for the resident Linux guest, with an unprivileged `agent` shell,
  token-bound command framing, bounded process resources, descendant cleanup,
  exact exit-status propagation, and a deliberately explicit `linux-sh` surface.
- A private Realm `pipe`/`spawn-signed`/`wait`/`signal` ABI and signed
  `guest-pipe-echo` workload, with generation-checked process handles, bounded
  descendant admission, pre-partitioned request budgets, unified file/pipe
  descriptor allocation, deterministic foreground-tree cleanup, and bounded
  bounded execution and deterministic resource cleanup.
- A versioned record-oriented signed-spawn ABI with bounded argv and environment
  vectors, build-manifest-generated immutable-catalog resolution, multiple exact
  descriptor mappings, atomic parent-endpoint close actions, kernel-owned file
  and pipe descriptor allocation, and the guest-side `realm-sh` workload for
  direct `echo`, environment, `echo | cat`, and file-backed `echo > PATH` jobs.
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
