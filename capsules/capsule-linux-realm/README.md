# AOS Realm Capsule

This directory contains the executable seed of AOS Realm: a principal-owned
agent workbench whose guest interface is Linux-shaped and whose outer authority
is an ordinary Astrid capsule.

It now keeps Linux resident for each active principal, but it is not yet the
useful AOS Realm distribution. The capsule runs signed, embedded core WebAssembly
command modules under Wasmi and a pinned Linux 6.18.39 image inside the bounded
`aos-rv64-virt-v0` machine. Linux now reaches an AOS-controlled PID 1 and a
static Buildroot 2026.05.1/musl/BusyBox userland, accepts token-bound bounded
command frames, and remains alive until an explicit shutdown or runtime eviction.
Its `ash` shell is useful today, although it is not yet Bash or a development
distribution.
Commands receive structured `argv` and an explicit current directory; there is
no host shell command line and the manifest requests no `host_process`
capability.

```text
agent -> realm tool -> signed nested WASM command -> private realm ABI
                  \-> admitted RV64 machine slice -> virtual hardware
      -> realm policy and accounting -> audited Astrid imports
```

## What works

`linux_realm_exec` currently admits signed core-WASM workloads, two diagnostic
RV64 instruction images, and the first resident Linux boot image:

- `pwd`
- `echo`
- `pipe-echo`, a two-process resumable echo-to-stdin-cat pipeline
- `guest-pipe-echo`, whose supervisor guest creates that pipe and both children,
  then waits for and reaps them itself
- `realm-sh`, a guest-side shell over structured tokens. Its current grammar is
  `echo TEXT`, `env KEY=VALUE`, `echo TEXT | cat`, or `echo TEXT > PATH`
- `rv64-smoke`, which runs 23 real RV64I instructions in bounded slices, writes
  `AOS RV64` through the virtual 16550 UART, and halts through the standard test
  finisher
- `rv64-supervisor`, which starts at reset in Machine mode, enters Supervisor
  mode with `mret`, delegates a Supervisor `ecall` through `stvec`, returns with
  `sret`, writes `STR\n`, and halts from Supervisor mode. It charges 31 bounded
  steps while retiring 30 instructions because `ecall` does not retire
- `linux-boot`, which lazily boots the embedded, reproducibly built Linux 6.18.39
  `Image` in a 32 MiB guest and returns when `/init` reports `AOS READY`; calling
  it again while warm is a zero-step readiness check
- `linux-console`, which lazily boots if needed, sends one validated line to the
  resident `/init`, and returns one framed result while preserving Linux RAM;
  the proof commands are `ping`, `counter`, and `echo ...`
- `linux-sh`, which executes one bounded script with BusyBox `ash` as UID/GID
  1000 in `/home/agent` or the invocation's mounted `/workspace`, propagates its
  exact exit status, kills/reaps background descendants, and preserves the
  in-RAM home across warm calls
- `linux-shutdown`, which cleanly powers a warm guest down through SBI and
  releases its RAM; stopping an already-cold realm is an idempotent zero-step
  operation
- `write-file`
- `cat`
- `smoke-write`, the original interpreter smoke test

`linux_realm_status` reports the guest-visible mount and command surface without
exposing physical host paths. It also reports the caller's actor boot sequence,
completed-command count, next process identifier, and live process/pipe resource
accounting. Linux-specific fields state whether the one-vCPU guest is cold or
running, whether RAM is currently resident, the number of boots, completed
commands, clean shutdowns, and exact guest-step totals for the current
principal-affine Store. Outer Wasm metering is charged to the verified invoking
principal. The response's versioned path contract reports the semantic mount,
guest path, Astrid resource URI where one really exists, human display path,
reference lifetime, and the projection state for nested WASM and Linux. The
invocation workspace is mounted into Linux through bounded 9P; the separate
durable Realm home is not yet a Linux mount. Every execution result identifies
the kernel-stamped owner principal and exact execution backend, and includes the
requested and effective CWD, path context, outcome, exit status, stdout, stderr,
fuel or instruction accounting, memory ceiling, and process identifiers where
the semantic process kernel allocated them.

The first execution call lazily creates the declared Realm layout and its
in-memory machine for the invoking principal. Status reports `uninitialized` and
an idle actor before that point without allocating a machine or advancing durable
boot state:

```text
/home/agent   durable realm home
/workspace    Astrid COW projection of the invocation workspace
/tmp          principal-private temporary subtree
```

The nested core-WASM lane can use all three projections today. Its `/home/agent`
is a versioned filesystem. One principal-scoped KV value atomically selects its
current generation; immutable manifests and file contents are stored as
BLAKE3-addressed blobs beneath the caller's private realm store. It survives
daemon restart. Its `/workspace` is deliberately transactional: writes enter
Astrid's copy-on-write view and do not change the source workspace until the
outer Astrid workflow promotes them. An unpromoted workspace overlay is
discarded on daemon restart. `/tmp` is not durable state.

The Linux lane mounts the invocation's `cwd://` resource at `/workspace` before
every `linux-sh` call. Linux 9P requests cross a bounded private SBI channel into
the capsule's Rust 9P server and then the Astrid filesystem imports. Because the
runtime does not yet expose a stable workspace attachment ID or epoch, the mount
is torn down and recreated for every call; no guest FID or path reference is
allowed to outlive the invocation. Linux `/home/agent` and `/tmp` remain guest
RAM, preserved only while that principal's machine remains warm.

The default realm name is `default`, giving one durable home per principal. The
principal is never accepted from tool input. It comes from the kernel-stamped
invocation, so two principals using the same capsule bytes receive different
KV heads and different blob namespaces.

## Path identity contract

A path has three audience-specific spellings, but one typed identity:

| Audience | Example | Purpose |
| --- | --- | --- |
| Agent shell | `/workspace/src/lib.rs` | Path passed to a program inside the Realm |
| Astrid runtime | `cwd://src/lib.rs` | Capability-checked resource, never a physical host path |
| Person | `Workspace/src/lib.rs` | Stable display label that makes the mounted root explicit |

`PathRef` carries the mount role and optional mount ID, relative path, guest
spelling, optional Astrid resource URI, display path, reference lifetime, and the
home generation or Realm boot observed when the call was admitted.
`MountContext` adds the consumer (`nested-core-wasm`, `linux-guest`, or
`bare-rv64`), all declared mount projections, and an explicit
`physical_host_paths_visible=false` invariant.

The enclosing execution response supplies the verified owner principal. A durable
home reference is therefore identified by owner, Realm home ID, relative path,
and admitted generation. A Linux RAM path instead carries the `linux-rootfs`
mount ID and the admitting Realm boot sequence. Workspace references are
invocation-scoped and have `mount_id=null`: the `cwd://` host import does not yet
supply a stable attachment ID or generation, and the capsule does not invent
one. Both nested WASM and Linux workspace references carry the real `cwd://`
resource URI. Bare RV64
diagnostics have no active path.

A client may show a person a local path such as `/Users/me/project`, but that
string is not sent into the guest or exposed by status. The client/runtime must
first attach the selected directory as a workspace and translate a child to
`Workspace/<relative>` for conversation and `/workspace/<relative>` for guest
execution. If no attachment exists, the correct result is an unresolved-path
error, not a guessed rewrite. When a person and agent mention different
spellings, receipts and UI should show both the display path and guest path while
retaining the typed reference underneath.

## Durable home format

The selected metadata is deliberately small:

```text
principal KV: realm/default/fs/head
  -> { format, generation, manifest_digest }

principal file store: .../store/blobs/<blake3>
  -> immutable file bytes or immutable manifest bytes
```

A create-or-truncate close writes and verifies the file blob, writes and verifies
a new manifest whose parent is the prior selected manifest, then swaps the head
with KV compare-and-swap. A crash before the swap can leave unreachable blobs but
cannot select a partial generation. A losing concurrent writer reloads the winner,
merges its own replacement, and retries up to a fixed bound.

Existing format-0 homes are not discarded. Their direct files are imported into
the versioned store lazily on first read. The old direct path remains a rebuildable
compatibility projection; reads prefer the selected versioned generation.

The current seed supports regular-file create/truncate and read, with a 64 KiB
per-file limit and 1 MiB manifest limit. It does not yet implement delete, rename,
directory metadata, permissions, links, garbage collection, named checkpoints, or
a guest `fsync`/flush call. `linux_realm_status` exposes the format, selected
generation, file count, and manifest digest without exposing a physical path.

## Process-kernel model

`crates/realm-core` is now the host-testable semantic kernel for the next runtime
increment. It provides monotonic typed process, pipe, and backing-file-description
identifiers, one kernel-allocated descriptor-number space, explicit
created/runnable/running/waiting/zombie transitions, direct-child wait and reap,
typed signal termination, a single-running-process FIFO reference scheduler,
atomic pipe-descriptor inheritance, bounded partial pipe I/O, backpressure, EOF,
broken-pipe behavior, and aggregate quotas.

The capsule now opts into Astrid's principal-affine service mode. The runtime
lazily creates one Wasmtime Store per active principal, bounds the resident set,
and evicts only idle Stores by least-recent use. Inside each Store the capsule
keeps exactly one semantic kernel and monotonic PID namespace. It repeats the
runtime's owner check and fails closed if that component instance ever sees a
different principal.
Completed foreground jobs are reaped, so their process and pipe resources return
to zero while their identifiers are never reused during that Store residency. A
principal-scoped boot sequence is advanced with KV compare-and-swap and makes PID
reuse after eviction or restart explicit as `(boot sequence, PID)`.

`crates/realm-machine` is the host-testable full-system backend seed. It owns only
admitted guest CPU/CSR state, contiguous RAM, bounded serial input/output, the
standard test finisher, and slice execution. Its current surface is RV64IMA plus
Zicsr, typed M/S CSRs, general synchronous
exception delivery/delegation, `mret`/`sret`, Sv39 translation, and `sfence.vma`
under the ratified RISC-V Machine and Supervisor ISA 1.13. The page walker is
bounded and checks canonicality, PTE/superpage form, U/S and R/W/X permissions,
SUM/MXR, MPRV, and A/D updates against admitted RAM. It also owns independent
architectural counters, the deterministic single-hart CLINT, interrupt selection
and vector entry, and bounded `wfi`. The Linux boot contract loads a raw RV64
`Image` at the standard 2 MiB boundary, page-aligns an admitted initramfs after
it, generates the versioned `aos-rv64-virt-v0` FDT without a host tool, and
enters the kernel in S-mode with `a0=hartid` and `a1=FDT`. Its private firmware
implements the SBI 3.0 Base, TIME, DBCN, and SRST subsets needed by this
single-hart profile. The private implementation ID is deliberately unregistered;
it is not presented as an assigned RISC-V SBI implementation ID. The machine has
no browser, JavaScript, JIT, host process, host filesystem, or network dependency
and compiles for the capsule's `wasm32-unknown-unknown` target.

The pinned kernel, Buildroot rootfs, and AOS-controlled `/init` provide a
token-bound serial command channel inside this machine. The static BusyBox shell
is a useful Linux-workbench seed, not a claim of Debian compatibility: Bash,
Python, an in-guest compiler, networking, durable block storage, and PTYs remain
absent. The only guest file portal is the synchronous 9P workspace transport;
compressed instructions, PLIC, and virtio block are deliberately deferred until
the selected kernel/device profile requires them.

The private ABI exposes bounded `pipe`, compatibility `spawn-signed`, record-based
`spawn-signed-record`, `wait`, and `signal` operations. The record form selects an
absolute path from an immutable catalog, carries up to 64 argv entries and 64
validated `KEY=VALUE` environment entries, and applies at most 16 explicit
descriptor actions. The catalog is generated from validated `guests/catalog.tsv`
metadata at build time. A `dup` action grants one named pipe endpoint at one child
descriptor. A `close-parent` action atomically releases the parent's copy only
after child creation succeeds; rejected records leave its table unchanged.
File descriptor numbers are also kernel-owned, but file child actions fail closed
until a realm-wide open-file-description table can preserve shared offsets and
last-close behavior.

`realm-sh` is ordinary nested guest code, not host parsing. It reads the structured
tokens passed in `argv`, resolves the three generated catalog paths
(`/bin/echo`, `/bin/cat`, and `/usr/bin/env`), writes spawn records into its own
linear memory, starts the children, and waits for exact terminal records. Every
process has an isolated Wasmi store and memory. A guest cannot submit module bytes,
inherit Astrid authority, search the host `PATH`, or start a host process.
Generation-checked handles prevent stale PID use, the foreground command admits at
most two descendants, budgets are partitioned before execution, and all process
and pipe records are cleaned before the tool result returns. Background jobs still
do not survive calls.

The direct SDK tool entry point does not expose the transport call ID to capsule
code. The earlier manual run-loop replay cache therefore cannot be retained in
this execution mode without pretending that duplicate delivery is safe. Durable
exactly-once mutation receipts belong in the SDK/runtime tool-dispatch boundary;
until that lands, callers must treat a lost mutating response as indeterminate
and inspect state before retrying.

## Linux lifecycle and metering

The principal-affine component Store owns one optional `Rv64Machine`. The first
`linux-boot`, `linux-console`, or `linux-sh` allocates admitted RAM and advances
the guest in bounded 100,000-step slices until `/init` is ready. Later calls
resume the same kernel and userspace memory, so the `counter` proof advances
across separate tool invocations. There is no background CPU: Linux advances
only inside an admitted, metered invocation. A clean `linux-shutdown`, execution failure, output-limit
failure, runtime eviction, daemon restart, or capsule unload destroys RAM.

RAM residency is therefore an evictable cache, not durable process state. The
existing durable Realm home is not yet attached to Linux, and status continues to
return `linux_storage_persistent=false`. `/workspace` has Astrid's outer COW and
promotion semantics; it is not evidence of a durable Linux root or home.

Astrid Runtime now has the experimental primitive required to make this boundary
honest: an opt-in Store permanently keyed by `(capsule, component, verified
principal)`, with per-call fuel charging, exact aggregate resident-memory
accounting, live quota enforcement, bounded LRU residency, and idle-only
eviction. `Capsule.toml` requests it through fail-closed package metadata and
requires Astrid `>=0.10.2`; 0.10.1 must not silently run this capsule with its old
free-pool or shared-run-loop semantics.

This closes both outer component affinity and the first inner Linux lifecycle:
lazy boot, ready, bounded command, clean stop, and restart are executable. Exact
RV64 step counts remain separate principal-local records for the current Store
residency. Durable disk remains an independent block-overlay problem rather than
a promise made by resident RAM. The pinned Buildroot 2026.05.1 userland and the
invocation-scoped workspace are now behind this lifecycle; attaching durable
principal home storage is the next storage increment.

## Build and install

From the `aos-ce` repository:

```sh
cargo test -p aos-realm-abi -p aos-realm-core -p aos-realm-machine -p aos-realm-runtime \
  -p aos-realm-vfs -p aos-linux-realm \
  --target "$(rustc -vV | sed -n 's/^host: //p')"
cargo clippy -p aos-realm-abi -p aos-realm-core -p aos-realm-machine -p aos-realm-runtime \
  -p aos-realm-vfs -p aos-linux-realm \
  --target "$(rustc -vV | sed -n 's/^host: //p')" -- -D warnings
cargo check -p aos-linux-realm --target wasm32-unknown-unknown
aos capsule build capsules/capsule-linux-realm
astrid --principal default capsule install \
  dist/aos-linux-realm.capsule
```

The installed realm appears as two MCP tools when an Astrid MCP broker such as
`sage-mcp` is present and `astrid --principal default mcp serve` is connected to
an MCP client. `astrid mcp serve` is the stdio transport shim, not the broker.
The first mutating call may elicit session-ingress consent and a capsule grant.

Example tool arguments:

```json
{"command":"pwd"}
{"command":"echo","args":["hello", "realm"]}
{"command":"pipe-echo","args":["hello through two processes"]}
{"command":"guest-pipe-echo","args":["the guest built this pipeline"]}
{"command":"realm-sh","args":["echo","the shell built this job"]}
{"command":"realm-sh","args":["echo","the shell built this pipe","|","cat"]}
{"command":"realm-sh","args":["echo","persisted by the guest shell",">","/home/agent/note.txt"]}
{"command":"realm-sh","args":["env","ASTRID_REALM=ready"]}
{"command":"rv64-smoke"}
{"command":"rv64-supervisor"}
{"command":"linux-boot"}
{"command":"linux-console","args":["ping"]}
{"command":"linux-console","args":["counter"]}
{"command":"linux-console","args":["echo","hello from Linux"]}
{"command":"linux-sh","args":["id -u; uname -m; pwd"]}
{"command":"linux-sh","args":["printf persisted > proof && cat proof"]}
{"command":"linux-sh","args":["pwd; ls -la"],"cwd":"/workspace"}
{"command":"linux-shutdown"}
{"command":"write-file","args":["notes.txt","durable\n"],"cwd":"/home/agent"}
{"command":"cat","args":["notes.txt"],"cwd":"/home/agent"}
{"command":"write-file","args":["candidate.rs","..."],"cwd":"/workspace"}
```

The outer `command` field is never a command line. For example,
`{"command":"pwd && whoami"}` is rejected as an unknown program. `realm-sh`
interprets only its separate structured `args` tokens according to the grammar
above; a single argument containing `"echo hello | cat"` exits with usage status
64 instead of being reparsed as text.

## Current boundary

The private `aos_realm_v0` ABI supplies bounded argument, environment, and CWD
reads, open/read/write/close, pipe creation, signed record-based child creation,
direct-child wait, direct-child signal, monotonic time, and exit. Paths are normalized within
`/home/agent`, `/workspace`, or `/tmp`; unmounted absolute paths and upward escape
fail closed. Writes are buffered at the capsule edge and committed only when the
nested descriptor closes, so a trapped command does not leave a partial guest
file. Durable-home closes select a content-addressed generation with a KV CAS;
workspace and temporary files retain their outer mount semantics.

The embedded kernel executes general RV64 Linux syscalls, including PID 1's
console, mount, credential, process, and reboot paths; the earlier nested
core-WASM process lane still uses the private Realm ABI. Static musl, BusyBox
`execve`, and `ash` are live. This slice does not yet provide shared or inherited
open-file descriptions, sequential POSIX file actions, Bash, package management,
networking, PTYs, surviving background jobs, or an in-guest compiler. Those
belong behind the same realm boundary; they must not be simulated by granting a
host process.

The Linux workspace driver is intentionally split at authority boundaries. The
GPL-2.0-only in-kernel `trans=aos` module turns Linux 9P calls into one synchronous
SBI exchange. The MIT/Apache Rust machine validates and copies bounded guest RAM,
the 9P server implements filesystem semantics without ambient authority, and the
Astrid adapter resolves operations against the invocation's `cwd://` capability.
Docker is used only to reproduce the image; QEMU is neither linked nor used at
runtime.

## Distribution direction

The eventual distribution is AOS Realm, not a renamed Debian image. Its signed
base, packages, compiler target, update policy, durable overlay generations, and
build receipts belong to AOS Community Edition. Familiar Linux interfaces are a
compatibility surface. Guest root is never Astrid authority.
