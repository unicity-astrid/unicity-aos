# AOS Realm Capsule

This directory contains the executable seed of AOS Realm: a principal-owned
agent workbench whose guest interface is Linux-shaped and whose outer authority
is an ordinary Astrid capsule.

It keeps Linux resident for each active principal. The capsule runs signed,
embedded core WebAssembly command modules under Wasmi and a signed core-Wasm
RV64 worker with three ordered, hash-bound assets: a pinned Linux 6.18.39
kernel containing only a tiny bootstrap, the read-only AOS Realm development
SquashFS, and a principal-free post-mount checkpoint. Linux reaches an
AOS-controlled PID 1, accepts token-bound command frames, and remains alive
until explicit shutdown or runtime eviction. The immutable system contains
Bash, Git, Python, Clang/C++, Make, CMake, Ninja, Rust/Cargo/rustup, and
`astrid-build`; mutable home and workspace bytes never enter it.
Commands receive structured `argv` and an explicit current directory; there is
no host shell command line and the manifest requests no `host_process`
capability.

```text
agent -> realm tool -> signed nested WASM command -> private realm ABI
                  \-> admitted RV64 machine slice -> virtual hardware
      -> realm policy and accounting -> audited Astrid imports
```

## What works

`realm_shell` is the normal agent-facing shell tool. Its `command` is executed
by Bash as UID/GID 1000 inside the caller's resident Linux Realm, with an
optional guest `cwd`, lower guest-step ceiling, and lower output ceiling. It has
no host execution mode and never falls back to `aos-shell`.

`linux_realm_exec` is the lower-level structured and diagnostic surface. It
currently admits signed core-WASM workloads, two diagnostic RV64 instruction
images, and the first resident Linux boot image:

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
- `linux-boot`, which restores the bound 1 GiB/one-hart principal-free
  checkpoint when the admitted envelope matches, otherwise cold-boots the
  hash-bound Linux 6.18.39 kernel and immutable system in the principal's
  admitted 512 MiB–3 GiB envelope; it returns when `/init` reports `AOS READY`,
  and calling it again while warm is a zero-step readiness check
- `linux-console`, which lazily boots if needed, sends one validated line to the
  resident `/init`, and returns one framed result while preserving Linux RAM;
  the proof commands are `ping`, `counter`, and `echo ...`
- `linux-sh`, which executes one bounded Bash script as UID/GID 1000 in
  `/home/agent` or the invocation's mounted `/workspace`, propagates its exact
  exit status, kills/reaps background descendants, and commits home mutations
  as crash-consistent principal generations
- `linux-shutdown`, which cleanly powers a warm guest down through SBI and
  releases its RAM; stopping an already-cold realm is an idempotent zero-step
  operation
- `write-file`
- `cat`
- `smoke-write`, the original interpreter smoke test

`linux_realm_status` reports the guest-visible mount and command surface without
exposing physical host paths. It also reports the caller's actor boot sequence,
completed-command count, next process identifier, and live process/pipe resource
accounting. Linux-specific fields state whether the admitted virtual-CPU topology
is cold or running, the configured and effective vCPU counts, whether RAM is
currently resident, the number of boots, completed
commands, clean shutdowns, and exact guest-step totals for the current
principal-affine Store. Outer Wasm metering is charged to the verified invoking
principal. The response's versioned path contract reports the semantic mount,
guest path, Astrid resource URI where one really exists, human display path,
reference lifetime, and the projection state for nested WASM and Linux. The
invocation workspace and durable Realm home are mounted into Linux through
separate bounded 9P channels. Every execution result identifies
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
/workspace    host-managed projection of the invocation workspace
/tmp          principal-private temporary subtree
```

The nested core-WASM lane can use all three projections today. Its `/home/agent`
is a versioned filesystem. One principal-scoped KV value atomically selects its
current generation; immutable manifests and file contents are stored as
BLAKE3-addressed blobs beneath the caller's private realm store. It survives
daemon restart. Its `/workspace` follows the policy of the outer Astrid
attachment. Git-managed workspaces are shared directly so edits are immediately
visible to the person and ordinary Git remains the rollback mechanism. A
non-Git workspace may instead be supplied through Astrid's OS-level copy-on-write
backend and require outer promotion. The capsule cannot distinguish or enlarge
that policy, so its receipt reports `host-managed` rather than claiming one
durability model. `/tmp` is not durable state.

The Linux lane mounts the read-only system asset as `/`, the principal's
selected Realm generation at `/home/agent`, and the invocation's `cwd://`
resource at `/workspace`. Immutable system reads cross private SBI channel 3
and are completed inside the compute worker from an exact manifest-selected
asset; Linux supplies only an aligned offset and length. Linux 9P requests
cross channels 1 and 2 into the capsule's Rust 9P server. Home operations select a complete
content-addressed generation; workspace operations resolve through the current
Astrid filesystem imports. Because the runtime does not yet expose a stable
workspace attachment ID or epoch, the workspace mount is torn down and
recreated for every call; no workspace FID or path reference is allowed to
outlive the invocation. The home session is principal-resident, while its
authoritative generation survives guest shutdown, component eviction, and
daemon restart. Linux `/run` and `/tmp` remain guest RAM.

Astrid reports host POSIX mode bits through the workspace projection. On hosts
without a portable POSIX-mode projection, mode `0` falls back to `0755` for
directories and `0644` for regular files inside the already
capability-confined mount. The frozen `astrid:fs@1.0.0` contract does not expose
mode mutation, so portable guest `chmod` remains a future canonical WIT and SDK
addition rather than a private capsule convention.

Workspace data uses resource-backed `FileHandle` operations: each bounded 9P
frame maps to positional read or write, truncate maps to `set-len`, flush maps
to the appropriate host sync operation, and rename is atomic inside the same
workspace VFS. Large files therefore stream without the former 10 MiB
whole-file compatibility ceiling. File handles are scoped to the admitted
invocation and cannot become hidden durable state. When the outer runtime uses
its current copy-on-write backend, its separate 50 MiB copy-up/promotion ceiling
still applies to lower-layer files; direct Git-managed workspaces do not take
that path. Removing the conditional COW ceiling requires a streaming outer
implementation, not a larger capsule buffer.

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
home generation or Realm boot observed when the call was admitted. Execution
also reports the selected home generation before and after the command.
`MountContext` adds the consumer (`nested-core-wasm`, `linux-guest`, or
`bare-rv64`), all declared mount projections, and an explicit
`physical_host_paths_visible=false` invariant.

The enclosing execution response supplies the verified owner principal. A durable
home reference is therefore identified by owner, Realm home ID, relative path,
and admitted generation in both nested-WASM and Linux consumers. A Linux
temporary path instead carries the `linux-rootfs` mount ID and the admitting
Realm boot sequence. Workspace references are
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
  -> immutable 64 KiB chunks, radix nodes, or manifest bytes
```

A file record selects a sparse three-level radix tree over immutable 64 KiB
content chunks. The chunk is a storage unit, not a logical file ceiling:
missing subtrees read as zeroes, positional writes replace only one chunk and
its ancestor path, and the tree addresses up to one TiB. The guest's optional
`linux_max_file_bytes` and Astrid's mandatory aggregate principal storage
ledger remain the policy boundaries.

A mutation writes and verifies its new chunks and tree nodes, writes and
verifies a new manifest whose parent is the prior selected manifest, then swaps
the head with KV compare-and-swap. A crash before the swap can leave unreachable
blobs but cannot select a partial generation. A losing concurrent writer reloads
the winner, merges its own replacement, and retries up to a fixed bound.

Existing format-0, format-1, and format-2 homes are not discarded. Before first
format-3 execution, the dedicated direct-home tree is enumerated in stable order
and any node absent from the selected generation is imported within explicit
entry and file bounds. Format-1 manifests materialize their implicit parent
directories in memory. Format-2 whole-file records remain readable and upgrade
lazily on their first content mutation.

The current seed supports regular-file create/read/positional-write/truncate,
directory create/read/remove, tree rename, unlink, persisted permission bits,
sparse extension, and synchronous flush semantics. There is no 64 KiB logical
file limit; one MiB bounds metadata blobs independently of file content. Every
successful mutation is already durable at its generation head; `fsync` therefore
has no deferred dirty bytes. Links, timestamps, quota-aware garbage collection,
named checkpoints, and diff/reset remain absent. The component does not yet
receive the outer principal storage quota, so 9P `statfs` capacity fields remain
unspecified rather than fabricating free space.
`linux_realm_status` exposes the format, selected generation, file count, and
manifest digest without exposing a physical path.

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

A foreground command keeps its Astrid tool invocation—and therefore the agent
turn awaiting that tool result—open until the command exits, fails, is
cancelled, times out, or exhausts an admitted budget. Shell `&` does not detach:
PID 1 kills and reaps remaining descendants before returning. Durable continued
work will be a separate job API that returns an opaque job handle and exposes
explicit status, logs, wait, and cancel operations; warm Linux residency alone
does not run CPU between invocations.

The currently released AOS MCP path has its own shorter broker/shim deadlines.
Those are transport policy, not Linux background execution: if either expires,
the original Realm invocation can continue until Astrid's outer principal
timeout because cancellation is not yet propagated across the tool bus. Long
foreground builds therefore require explicitly aligned broker, shim, and
principal deadlines. A future background operation must use the job API rather
than relying on a timed-out or disconnected foreground call.

`crates/realm-machine` is the host-testable full-system backend seed. It owns only
admitted guest CPU/CSR state, contiguous RAM, bounded serial input/output, the
standard test finisher, and slice execution. Its current surface is RV64IMA plus
F, D, C, Zicsr, typed M/S CSRs, general synchronous
exception delivery/delegation, `mret`/`sret`, Sv39 translation, and `sfence.vma`
under the ratified RISC-V Machine and Supervisor ISA 1.13. The page walker is
bounded and checks canonicality, PTE/superpage form, U/S and R/W/X permissions,
SUM/MXR, MPRV, and A/D updates against admitted RAM. It also owns independent
architectural counters, per-hart CLINT timer/software interrupts, deterministic
round-robin hart scheduling, interrupt selection and vector entry, and bounded
`wfi`. The Linux boot contract loads a raw RV64
`Image` at the standard 2 MiB boundary, page-aligns an admitted initramfs after
it, generates the versioned `aos-rv64-virt-v1` FDT without a host tool, and
enters the kernel in S-mode with `a0=hartid` and `a1=FDT`. Its private firmware
implements the SBI 3.0 Base, TIME, IPI, RFENCE, HSM, DBCN, and SRST subsets
needed by the SMP profile. The private implementation ID is deliberately unregistered;
it is not presented as an assigned RISC-V SBI implementation ID. The machine has
no browser, JavaScript, JIT, host process, host filesystem, or network dependency
and compiles for the capsule's `wasm32-unknown-unknown` target.

The pinned kernel, Buildroot rootfs, and AOS-controlled `/init` provide a
token-bound serial command channel inside this machine. The development image
contains Bash, Git, Python, Clang/LLVM C and C++, binutils, Make, CMake, Ninja,
pkg-config, patch, CA certificates, and strace. The complete toolchain probe
compiles and executes C and C++, configures and builds with CMake/Ninja, and
creates a real Git commit on the governed workspace mount. This is an agent
workbench, not a claim of Debian compatibility: package management, ambient
networking, durable block storage, and PTYs remain absent. The only guest file
portals are the synchronous 9P home and workspace transports; PLIC and virtio
block remain deferred until the selected device profile requires them.

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
the guest in bounded 10,000,000-step slices until `/init` is ready. Later calls
resume the same kernel and userspace memory, so the `counter` proof advances
across separate tool invocations. There is no background CPU: Linux advances
only inside an admitted, metered invocation. A clean `linux-shutdown`, execution failure, output-limit
failure, runtime eviction, daemon restart, or capsule unload destroys RAM.

RAM residency is therefore an evictable cache, not durable process state.
Linux `/home/agent` is the durable Realm home and status reports that persistent
storage separately from `linux_rootfs_persistent=false`. `/workspace` has
the outer attachment's host-managed semantics—direct for Git-managed workspaces,
conditionally COW-backed otherwise. It is not evidence of a durable Linux root
filesystem.

Resource authority is hierarchical. Astrid's admin-owned principal profile is
the outer CPU, linear-memory, storage, process, and timeout boundary. The capsule
then resolves a smaller inner Linux envelope from the invoking principal's
per-capsule configuration on every operation:

| Config key | Default | Admitted range | Meaning |
| --- | ---: | ---: | --- |
| `linux_memory_bytes` | 0 | 0, or 536870912–3221225472 aligned to 4 KiB | Guest-visible RV64 RAM; zero uses current host/principal admission but caps the interpreter at its measured 1 GiB cold-boot default |
| `linux_max_steps` | 0 | 0–1000000000000 | Guest steps admitted per invocation; zero delegates to Astrid's outer principal CPU and timeout policy |
| `linux_max_output_bytes` | 65536 | 1–65536 | Captured Linux output per invocation |
| `linux_max_file_bytes` | 0 | 0–1099511627776 | Guest `RLIMIT_FSIZE`; zero applies no extra inner cap and leaves Astrid's principal storage quota in force |
| `linux_max_processes` | 0 | 0–65536 | Guest `RLIMIT_NPROC` for the agent UID; zero leaves Linux's inherited limit and Astrid's outer principal envelope authoritative |
| `linux_max_open_files` | 0 | 0–1048576 | Guest `RLIMIT_NOFILE` per process; zero preserves Linux's inherited operator limit |
| `linux_vcpus` | 0 | 0–64 | Logical Linux CPUs; zero selects the one-hart warm checkpoint, while explicit 1–64 selects an exact topology and values above one reserve one parallel compute worker per guest hart |

Operators control the enclosing pool independently from each principal. With
both host keys omitted, Astrid derives the worker count from useful CPU
parallelism and the shared-memory pool from physical RAM after a safety reserve:

```toml
[capsule]
compute_host_max_workers = 16
compute_host_max_shared_memory_bytes = 34359738368
```

Those values are optional ceilings, not recommended fixed defaults. A managed
host can then narrow one principal without changing the machine-wide pool:

```console
astrid quota set --agent alice --memory 8GiB --compute-workers 4 --cpu-fuel-per-sec 0
```

`--memory` is a virtual admission ceiling, not eager allocation;
`--compute-workers 0` delegates worker parallelism to the host; and
`--cpu-fuel-per-sec 0` removes the CPU rate cap. Managed installations can use
finite values for all three. Compute workers are the generic schedulable CPU
boundary. Linux vCPUs are a separate guest topology: today one deterministic
worker time-slices every admitted hart and charges their aggregate steps to the
principal. This provides correct Linux SMP semantics but makes no parallel
speedup claim.

The effective boundary is the intersection of the Astrid principal profile, the
daemon's host-wide compute pool, current aggregate reservations, the signed
worker maximum, the capsule hard maximum, this configured envelope, and any
lower per-command request.
For the optional step, file-size, and process limits, zero omits only that inner
boundary; Astrid's outer CPU, timeout, memory, and storage limits still apply. The guest cannot
raise any boundary. When guest RAM or vCPUs are automatic, Astrid first opens a
short-lived generic-compute admission probe. The capsule reads effective memory
and worker parallelism, releases the probe, and retains one exact deterministic
worker. For RAM it keeps the worker's fixed 64 MiB base, allocator headroom, and
a 128 MiB safety reserve outside guest RAM, uses the remaining
principal-admitted capacity, aligns down to a guest page, and never exceeds 3
GiB when explicitly configured. Auto mode stops at the measured 1 GiB
interpreter default so it can become ready inside the ordinary principal
timeout. The daemon and principal profile have already bounded the process-wide
pool before this calculation. For vCPUs auto mode selects one logical hart while
the deterministic worker is serialized; the recorded one/two/four-hart matrix
showed that additional harts increase Linux SMP work without adding host
execution parallelism. An explicit
`linux_vcpus` value is a logical topology override and does not reserve idle
native workers. Explicit limits remain useful for repeatable tests and managed
tiers; insufficient outer admission fails closed.

Changing the inner envelope never retargets a Store to another principal. On the
next mutating execution the capsule discards that principal's warm Linux RAM and
9P sessions, retains the actor boot identity, and remounts the same durable home
and invocation workspace under the new limits. Read-only status reports both the
configured and active envelopes plus whether that cold reconfiguration is
pending. Auto does not bind to all physical RAM. The daemon reserves at least
one eighth of host memory (with a 1 GiB floor), the compute ledger admits all
principals against the remainder, and the signed worker still imposes its own
finite maximum. Status exposes configured, active, and effective envelopes plus
the currently resident `linux_ram_bytes`.

Astrid Runtime now has the primitive required to make this boundary honest:
compute-worker capsules use one retained component Store per active verified
principal. Same-principal calls serialize on that Store-local machine;
different principals may run concurrently up to the ordinary instance-pool
ceiling. Idle Stores are bounded, least-recently-used, evictable RAM cache—not
durable principal state. Per-call fuel charging, principal quota enforcement,
and generic compute memory accounting remain mandatory below the pool.
`Capsule.toml` requests principal component residency through fail-closed
package metadata and requires Astrid `>=0.10.2`; older runtimes must not silently
run this capsule with free-pool or one-global-Store semantics.

This closes outer component affinity, the first inner Linux lifecycle, and the
durable home attachment: lazy boot, ready, bounded command, clean stop, and
restart are executable. Exact RV64 step counts remain separate principal-local
records for the current Store residency. Durable root storage remains an
independent block-overlay problem rather than a promise made by resident RAM.
The pinned Buildroot 2026.05.1 userland is a separately hash-bound read-only
SquashFS. The kernel and immutable system are restored from a principal-free
post-mount checkpoint when the exact 1 GiB/one-hart envelope matches; durable
principal home and invocation-scoped workspace capabilities are attached only
after restore.

## Build and install

From the `aos-ce` repository:

```sh
rustup toolchain install nightly-2026-04-04 --profile minimal --component rust-src
capsules/capsule-linux-realm/scripts/build-vcpu-worker.sh --check
cargo test -p aos-realm-abi -p aos-realm-core -p aos-realm-machine -p aos-realm-runtime \
  -p aos-realm-vfs -p aos-linux-realm \
  --target "$(rustc -vV | sed -n 's/^host: //p')"
cargo clippy -p aos-realm-abi -p aos-realm-core -p aos-realm-machine -p aos-realm-runtime \
  -p aos-realm-vfs -p aos-linux-realm \
  --target "$(rustc -vV | sed -n 's/^host: //p')" -- -D warnings
cargo check -p aos-linux-realm --target wasm32-unknown-unknown
(
  cd capsules/capsule-linux-realm
  aos capsule build
  ./scripts/package-capsule-assets.sh
  ./scripts/package-capsule-assets.sh --check
)
astrid --principal default capsule install \
  capsules/capsule-linux-realm/dist/aos-linux-realm.capsule
```

The asset-packaging step is currently mandatory. The released `astrid-build`
path packages the executable component and WIT but does not yet copy private
`compute-worker` assets declared by `[[component.asset]]`. Installing that thin
archive fails closed because `assets/linux-vcpu.wasm` is absent. The script
copies only the five manifest-bound assets and verifies their exact bytes in
the final archive; remove it only after the upstream builder covers and tests
this manifest surface.

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

## Benchmarking

The checked-in harness records raw JSON Lines samples plus median, mean, p95,
minimum, maximum, and standard deviation summaries:

```sh
python3 scripts/benchmark-linux-realm.py \
  --samples 30 --warmups 3 \
  --output /tmp/aos-linux-realm-benchmark.jsonl
```

The native reference lane measures the AOS-owned RV64 machine in the same
speed-optimized release profile used to establish interpreter throughput. It
records three current boundaries:

- `cold-to-init`: allocate the admitted envelope, load the image, and execute
  through PID 1's `AOS LINUX /init` marker;
- `cold-to-principal-bind`: continue through the first principal-home 9P request.
- `checkpoint-to-bindable`: validate and restore the principal-free one-hart
  checkpoint at that same pending home request, before any principal authority
  is attached.

When `qemu-system-riscv64` is installed, the harness boots the exact same Linux
`Image` under single-threaded TCG and records process start through the same PID
1 marker. QEMU cannot reach `AOS READY`: it does not implement the Astrid-owned
home and workspace transports. The QEMU sample therefore compares the shared
kernel-to-init boundary only, not complete Realm readiness.

Docker measurements are opt-in with `--docker-image IMAGE`. The harness refuses
implicit pulls and accepts only an existing local image. It labels ordinary
container create/start/exit separately from unpausing an already resident
container; neither is presented as a VM boot. An unavailable daemon or image is
emitted as a machine-readable skip, not a zero or failed timing.

Compilation, artifact file reads, and implicit network downloads stay outside
timed regions. Committed baselines must identify the exact Git revision,
artifact hashes, host architecture, engine versions, sample count, and boundary.
The native reference lane is not the full outer-Wasm/MCP path. Governed
request-to-result latency, QEMU snapshot restore, Docker/CRIU restore, RSS, and
per-principal scaling remain separate required lanes before cross-system product
claims are made.

The recorded 2026-07-23 M2 Ultra acceptance run used Linux 6.18.39, the
333,258,752-byte immutable system, a 1 GiB/two-hart machine, and 30 measured
samples after three warmups. Cold-to-init retired 42,197,024 guest steps with a
813.39 ms native median. Cold-to-principal-bind retired 46,431,093 steps with a
907.93 ms median. The 22,773,044-byte sparse checkpoint restored to the same
pending principal-home request in a 21.57 ms median and zero guest steps:
42.09 times faster than the corresponding cold-to-bind boundary. These are
native machine/checkpoint measurements, not a claim that the outer component,
333 MiB asset admission, IPC, and CLI transport complete in 21.57 ms.

## Current boundary

The private `aos_realm_v0` ABI supplies bounded argument, environment, and CWD
reads, open/read/write/close, pipe creation, signed record-based child creation,
direct-child wait, direct-child signal, monotonic time, and exit. Paths are normalized within
`/home/agent`, `/workspace`, or `/tmp`; unmounted absolute paths and upward escape
fail closed. Writes are buffered at the capsule edge and committed only when the
nested descriptor closes, so a trapped command does not leave a partial guest
file. Durable-home closes select a content-addressed generation with a KV CAS;
workspace and temporary files retain their outer mount semantics.

The packaged, hash-bound kernel asset executes general RV64 Linux syscalls,
including PID 1's console, mount, credential, process, futex, and reboot paths;
the earlier nested core-WASM process lane still uses the private Realm ABI. The
development image includes glibc, BusyBox, Bash, Git, Python, Clang/LLVM, CMake,
Make, Ninja, pkg-config, rustup 1.29.0, Rust/Cargo 1.97.1 with
`wasm32-unknown-unknown`, and `astrid-build` 0.10.4. A live principal has created,
compiled, retained across invocations, inspected, hashed, and executed a native
RISC-V Rust program inside this boundary. The next reproducible image config also
includes `file(1)`, which the live probe found missing. Package management,
networking, PTYs, surviving background jobs, streamed command progress, and a
fast execution backend remain absent. Those belong behind the same realm
boundary; they must not be simulated by granting a host process.

The Linux storage driver is intentionally split at authority boundaries. The
GPL-2.0-only in-kernel `trans=aos` module turns Linux 9P calls into one synchronous
SBI exchange. The MIT/Apache Rust machine validates and copies bounded guest RAM,
the 9P server implements filesystem semantics without ambient authority, and the
Astrid adapters resolve channel 1 against the selected principal-home generation
and channel 2 against the invocation's `cwd://` capability. Docker is used only
to reproduce the image; QEMU is neither linked nor used at runtime.

## Distribution direction

The eventual distribution is AOS Realm, not a renamed Debian image. Its signed
base, packages, compiler target, update policy, durable overlay generations, and
build receipts belong to AOS Community Edition. Familiar Linux interfaces are a
compatibility surface. Guest root is never Astrid authority.
