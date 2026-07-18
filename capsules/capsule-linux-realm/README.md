# AOS Realm Capsule

This directory contains the executable seed of AOS Realm: a principal-owned
agent workbench whose guest interface is Linux-shaped and whose outer authority
is an ordinary Astrid capsule.

It is useful now, but it is not Linux yet. The capsule runs signed, embedded core
WebAssembly command modules under Wasmi. The commands receive structured `argv`
and an explicit current directory; there is no host shell command line and the
manifest requests no `host_process` capability.

```text
agent -> realm tool -> nested WASM command -> private realm ABI
      -> mount and descriptor policy -> audited Astrid filesystem imports
```

## What works

`linux_realm_exec` currently admits exactly six signed workloads:

- `pwd`
- `echo`
- `pipe-echo`, a two-process resumable echo-to-stdin-cat pipeline
- `write-file`
- `cat`
- `smoke-write`, the original interpreter smoke test

`linux_realm_status` reports the guest-visible mount and command surface without
exposing physical host paths. It also reports the caller's actor boot sequence,
completed-command count, next process identifier, and live process/pipe resource
accounting. Every execution result identifies the kernel-stamped owner principal
and includes the nested process outcome, exit status, stdout, stderr, fuel
consumed, memory ceiling, and process identifiers.

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
increment. It provides monotonic typed process and pipe identifiers, explicit
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

This model is intentionally not exposed as guest `spawn`, `wait`, or `pipe`
creation imports yet. `pipe-echo` exercises two isolated, resumable Wasmi
processes and a four-byte bounded pipe during one foreground request. Process
handles and background jobs still do not survive calls because every admitted job
is foreground and reaped; the actor establishes the honest owner in which those
guest operations can be added next.

## Build and install

From the `aos-ce` repository:

```sh
cargo test -p aos-realm-abi -p aos-realm-core -p aos-realm-runtime \
  -p aos-realm-vfs -p aos-linux-realm \
  --target "$(rustc -vV | sed -n 's/^host: //p')"
cargo clippy -p aos-realm-abi -p aos-realm-core -p aos-realm-runtime \
  -p aos-realm-vfs -p aos-linux-realm \
  --target "$(rustc -vV | sed -n 's/^host: //p')" -- -D warnings
cargo check -p aos-linux-realm --target wasm32-unknown-unknown
astrid capsule build capsules/capsule-linux-realm
astrid --principal default capsule install \
  capsules/capsule-linux-realm/dist/aos-linux-realm.capsule
```

The installed realm appears as two MCP tools when a current Astrid MCP broker is
present and `astrid --principal default mcp serve` is connected to an MCP client.
The first mutating call may elicit session-ingress consent and a capsule grant.

Example tool arguments:

```json
{"command":"pwd"}
{"command":"echo","args":["hello", "realm"]}
{"command":"pipe-echo","args":["hello through two processes"]}
{"command":"write-file","args":["notes.txt","durable\n"],"cwd":"/home/agent"}
{"command":"cat","args":["notes.txt"],"cwd":"/home/agent"}
{"command":"write-file","args":["candidate.rs","..."],"cwd":"/workspace"}
```

Shell syntax is data, not authority. For example, `{"command":"pwd && whoami"}`
is rejected as an unknown program rather than evaluated by Bash.

## Current boundary

The private `aos_realm_v0` ABI supplies bounded argument and CWD reads,
open/read/write/close, monotonic time, and exit. Paths are normalized within
`/home/agent`, `/workspace`, or `/tmp`; unmounted absolute paths and upward escape
fail closed. Writes are buffered at the capsule edge and committed only when the
nested descriptor closes, so a trapped command does not leave a partial guest
file. Durable-home closes select a content-addressed generation with a KV CAS;
workspace and temporary files retain their outer mount semantics.

This slice does not provide Bash, guest-created processes or pipes, general Linux
syscalls, a libc, package management, networking, PTYs, or a compiler. Those
belong behind the same realm boundary; they must not be simulated by granting a
host process.

## Distribution direction

The eventual distribution is AOS Realm, not a renamed Debian image. Its signed
base, packages, compiler target, update policy, durable overlay generations, and
build receipts belong to AOS Community Edition. Familiar Linux interfaces are a
compatibility surface. Guest root is never Astrid authority.
