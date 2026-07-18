# AOS Realm Capsule

This directory contains the executable seed of AOS Realm: a principal-owned
agent workbench whose guest interface is Linux-shaped and whose outer authority
is an ordinary Astrid capsule.

It is useful now, but it is not Linux yet. The capsule runs signed, embedded core
WebAssembly command modules under Wasmi and now contains the first bounded
`aos-rv64-virt-v0` machine slice. Commands receive structured `argv` and an
explicit current directory; there is no host shell command line and the manifest
requests no `host_process` capability.

```text
agent -> realm tool -> signed nested WASM command -> private realm ABI
                  \-> admitted RV64 machine slice -> virtual hardware
      -> realm policy and accounting -> audited Astrid imports
```

## What works

`linux_realm_exec` currently admits eight signed core-WASM workloads and two
diagnostic RV64 instruction images:

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
- `write-file`
- `cat`
- `smoke-write`, the original interpreter smoke test

`linux_realm_status` reports the guest-visible mount and command surface without
exposing physical host paths. It also reports the caller's actor boot sequence,
completed-command count, next process identifier, and live process/pipe resource
accounting. Every execution result identifies the kernel-stamped owner principal
and exact execution backend, and includes the outcome, exit status, stdout,
stderr, fuel or instruction accounting, memory ceiling, and process identifiers
where the semantic process kernel allocated them.

The first execution call lazily creates this layout and its in-memory Realm
machine for the invoking principal. Status reports `uninitialized` and an idle
actor before that point without allocating a machine or advancing durable boot
state:

```text
/home/agent   durable realm home
/workspace    Astrid COW projection of the invocation workspace
/tmp          principal-private temporary subtree
```

`/home/agent` is a versioned filesystem. One principal-scoped KV value atomically
selects its current generation; immutable manifests and file contents are stored
as BLAKE3-addressed blobs beneath the caller's private realm store. It survives
daemon restart. `/workspace` is deliberately transactional: writes enter
Astrid's copy-on-write view and do not change the source workspace until the
outer Astrid workflow promotes them. An unpromoted workspace overlay is
discarded on daemon restart. `/tmp` is not durable state.

The default realm name is `default`, giving one durable home per principal. The
principal is never accepted from tool input. It comes from the kernel-stamped
invocation, so two principals using the same capsule bytes receive different
KV heads and different blob namespaces.

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

The capsule now runs as a long-lived service actor. It admits at most 32 active
principal machines, derives identity only from each kernel-verified message, and
keeps a separate semantic kernel and monotonic PID namespace for each principal.
Completed foreground jobs are reaped, so their process and pipe resources return
to zero while their identifiers are never reused during that capsule boot. A
principal-scoped boot sequence is advanced with KV compare-and-swap and makes PID
reuse after restart explicit as `(boot sequence, PID)`.

`crates/realm-machine` is the host-testable full-system backend seed. It owns only
admitted guest CPU/CSR state, contiguous RAM, bounded serial input/output, the
standard test finisher, and slice execution. Its current surface is the RV64I
integer subset used by the probes plus Zicsr, typed M/S CSRs, general synchronous
exception delivery/delegation, `mret`/`sret`, Sv39 translation, and `sfence.vma`
under the ratified RISC-V Machine and Supervisor ISA 1.13. The page walker is
bounded and checks canonicality, PTE/superpage form, U/S and R/W/X permissions,
SUM/MXR, MPRV, and A/D updates against admitted RAM. It has
no browser, JavaScript, JIT, host process, host filesystem, or network dependency
and compiles for the capsule's `wasm32-unknown-unknown` target.

Those probes are architectural boundary tests, not Linux claims. The M/A
extensions, counters, compressed instructions, timer/interrupt delivery, PLIC,
SBI/boot handoff, a device tree, and virtio block still have to land before a Linux
kernel can boot. Interrupt CSRs remain hardwired to zero until their corresponding
machine components exist.

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

The actor also retains a bounded 64-entry replay window for mutating tool call
IDs. A transport retry with the same principal, call ID, and arguments returns the
recorded result without running the process tree or filesystem mutation again;
reusing the ID with different arguments fails closed. This is an in-memory
per-actor-boot guarantee, not yet a durable exactly-once receipt across crashes.

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
astrid capsule build capsules/capsule-linux-realm
astrid --principal default capsule install \
  dist/aos-linux-realm.capsule
```

The installed realm appears as two MCP tools when a current Astrid MCP broker is
present and `astrid --principal default mcp serve` is connected to an MCP client.
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

This slice does not provide arbitrary executable resolution, shared or inherited
open-file descriptions, sequential POSIX file actions, `execve`, Bash, a booting
Linux kernel, general Linux syscalls, a libc, package management, networking,
PTYs, background jobs, or a compiler. Those belong behind the same realm boundary;
they must not be simulated by granting a host process.

## Distribution direction

The eventual distribution is AOS Realm, not a renamed Debian image. Its signed
base, packages, compiler target, update policy, durable overlay generations, and
build receipts belong to AOS Community Edition. Familiar Linux interfaces are a
compatibility surface. Guest root is never Astrid authority.
