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
- A signed release path for the 19 installable `aos-*` artifacts
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
- Resource-backed Linux workspace I/O over the frozen `astrid:fs@1.0.0`
  handle contract, replacing whole-file read/modify/write with bounded
  positional streaming, real truncate and sync, host mode reporting, and
  same-workspace rename without the former 10 MiB compatibility ceiling.
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
  steps, captured output, and an optional guest per-file ceiling, bounded by
  Astrid's admin-owned principal profile. Zero step or file ceilings delegate
  to the mandatory outer CPU/timeout or storage controls, respectively. Status
  distinguishes configured and active limits, while a changed envelope
  cold-reconfigures only that principal's warm Linux machine.
- Dynamic Linux Realm compute admission. Omitted daemon worker and memory
  ceilings derive from host CPU parallelism and physical RAM with a safety
  reserve, then intersect a process-wide pool, the invoking principal's memory
  and compute-worker quotas, existing reservations, and the signed worker
  maximum. Realm probes the admitted envelope, defaults guest RAM to half of
  its usable capacity so one Realm does not monopolize the pool, then reopens
  an exact reservation. RAM may still be fixed per principal through 3 GiB.
  Worker fuel joins the ordinary cross-capsule principal CPU account and rate
  limit.
- Deterministic virtual SMP for Linux Realm: exact 1–64-hart FDT topology,
  per-hart architectural, timer, interrupt, reservation, and translation state,
  round-robin aggregate metering, SBI HSM/IPI/RFENCE/TIME services, an
  SMP-enabled reproducible Linux image, and a signed-worker proof that brings
  two Linux CPUs online through Astrid's real generic-compute runtime.
  `linux_vcpus=0` derives a useful topology from current principal/host compute
  admission; explicit per-principal values select 1–64 logical CPUs without
  reserving unused native workers. Multi-hart machines cold-boot while the
  existing format-1 prewarm artifact remains exactly one hart.
- A reproducibly pinned Buildroot 2026.05.1, static musl, and BusyBox `ash`
  workbench for the resident Linux guest, with an unprivileged `agent` shell,
  token-bound command framing, bounded process resources, descendant cleanup,
  exact exit-status propagation, and a deliberately explicit `linux-sh` surface.
- A reproducible RV64GC/glibc development-image generation with the official
  Rust 1.97.1 compiler, Cargo, rustfmt, Clippy, and
  `wasm32-unknown-unknown` standard library; guest-native rustup 1.29.0 and
  `astrid-build` 0.10.4; and principal-local Cargo/rustup state rooted in the
  durable Realm home. Per-principal process and open-file ceilings now cross
  the versioned command frame alongside the existing RAM, CPU, output, and
  file-size envelope.
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
- Forge in the default Community Edition distribution, including a discoverable
  bootstrap and skill that teach fresh agents to build a user-space
  meta-harness on AOS by seeing instructions, memory, skills, harness code,
  tools, capsules, traces, and evaluations as an improvable world. Agents reach
  for Forge proactively when real work reveals a useful new capability, while
  optional workers remain a use-case choice rather than a prerequisite.
- Homebrew formula updates initiated by the tap's authenticated stable-release
  poll, eliminating the cross-repository dispatch credential.
- Strict, signed stable/dev/nightly channel and immutable release metadata
  contracts with exact workflow identities, expiry, replay-resistant generation
  state, and fail-closed direct installer resolution.
- A native release gate that initializes a clean AOS home, verifies the exact
  19-capsule CE lock, grants, and ready set, repeats initialization without
  changing runtime state, and proves clean daemon shutdown before publication.
- Native `aos status` output for authenticated running state and verified
  stopped state without invoking the runtime CLI.
- An opt-in daily nightly train with deterministic run-dated versions, exact
  Astrid compatibility pins, protected publication and promotion, and
  idempotent recovery after interrupted release or pointer updates. It is
  disabled by default; merging `main` never publishes a release.

### Changed

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
