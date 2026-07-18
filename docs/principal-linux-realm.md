# AOS Principal Linux Realm Capsule

Status: active implementation programme; persistent principal Realm actor live

Last reviewed: 2026-07-18

## 1. Decision

AOS Community Edition should provide each authorized agent principal with a
persistent, Linux-compatible workbench implemented as a WebAssembly capsule. The
agent should be able to use a shell, processes, files, pipes, compilers, package
tooling, and long-running development services without giving a capsule ambient
access to the host operating system.

The product identity is `AOS Realm` and the capsule package is
`aos-linux-realm`. A realm is not the Astrid kernel, a host process escape hatch,
or a physical virtual machine. It is a principal-owned software system contained
by an ordinary Astrid component boundary:

```text
agent principal
  -> Linux realm tool/service interface
  -> Linux-compatible syscall kernel inside the capsule
  -> sandboxed WASM processes
  -> Astrid host imports for explicitly granted effects
  -> principal-scoped storage, network, clocks, entropy, and audit
```

This moves ahead of the native-kernel, Tensor Logic backend, graphics, and native
driver implementations because it immediately supplies the environment in which
an agent can inspect source, run familiar tools, compile capsules, test software,
and retain a durable working state. It also provides an executable workload against
which later Astrid hosts can be tested.

The first proof must be a real installable capsule. It must not claim Debian
binary compatibility, Bash compatibility, or arbitrary package installation until
those programs actually run under the measured implementation.

## 2. What WebVM, BrowserPod, and BrowserCode establish

Two related architectures demonstrate different compatibility points.

### 2.1 WebVM and CheerpX

WebVM runs an existing Linux userland by translating x86 applications and
implementing a Linux-compatible syscall environment. Its filesystem uses a shared
read-only image with a persistent writable overlay. This establishes that a user
can experience a durable Debian-like computer while the entire authority boundary
remains a WebAssembly sandbox.

References:

- [CheerpX architecture](https://cheerpx.io/docs/overview)
- [CheerpX filesystem and overlay devices](https://cheerpx.io/docs/guides/File-System-support)
- [WebVM persistence design](https://labs.leaningtech.com/blog/webvm-20)

CheerpX is currently proprietary for commercial use, browser-oriented, and
documented as supporting 32-bit x86 binaries. It is evidence and a possible
research dependency, not an assumed Astrid implementation.

### 2.2 BrowserPod and BrowserCode

BrowserPod takes a more Astrid-like path. Programs such as Bash, Git, Node, and
Python are compiled into WebAssembly modules. Separate module instances act as
processes, each with its own imported memory. A WebAssembly kernel supplies a
coherent Linux-compatible syscall interface, filesystem, process coordination, and
network environment. BrowserCode demonstrates an unmodified agent CLI application
running on that compiled runtime and tool set.

References:

- [BrowserPod kernel architecture](https://browserpod.io/blog/browserpod-deep-dive/)
- [BrowserCode](https://browsercode.io/)
- [BrowserCode reference application](https://github.com/leaningtech/browsercode)
- [BrowserPod project and licensing](https://github.com/leaningtech/browserpod-meta)

BrowserPod's engine is also proprietary. Its process model, syscall boundary,
block-streamed persistent filesystem, controlled network proxy, and
`wasm32-browserpod-linux` compiler target are the relevant architectural evidence.

### 2.3 Claim boundary

These systems prove that the product experience is possible. They do not prove
that their browser packages can be placed unchanged inside an Astrid
`wasm32-unknown-unknown` component. Browser APIs, JavaScript workers, dynamic module
instantiation, filesystem backends, JIT treatment, and licensing must all be
replaced or explicitly integrated.

## 3. The system seen from each side

To the agent, the realm should look unsurprising:

```text
$ pwd
/home/agent/work/project

$ python3 --version
Python ...

$ gcc --version
...

$ astrid capsule build
...
```

To Astrid, the same activity is:

```text
principal P
  owns realm R
  invokes process Q
  reads volume V
  requests network endpoint N
  consumes CPU/memory/output budgets B
  exports candidate artifact A
```

The agent does not need to know whether a process is a native host process, a
nested WASM instance, an interpreted binary, or a translated binary. That is an
implementation detail behind the realm contract. Astrid must always know which
implementation is active because its performance, compatibility, and residual-risk
claims differ.

## 4. Two boundaries, not direct host syscalls

Guest programs never issue host syscalls directly.

```text
guest program
  -> versioned Linux-compatible guest ABI
  -> realm syscall implementation
  -> capability and namespace checks
  -> astrid:* host interfaces
  -> host mechanism
```

The guest ABI is internal to the realm implementation. It may initially be a core
WASM import module with named functions or a bounded syscall dispatcher over guest
memory. It is not automatically a public Astrid WIT contract.

The outer boundary remains the audited Astrid Component Model surface. For example:

| Guest operation | Realm mechanism | Astrid effect |
|---|---|---|
| `openat` under `/home/agent` | Resolve beneath the realm mount without escape | Principal-scoped file or block operation |
| `write` to stdout | Append to a bounded process stream | Tool/session output stream |
| `socket` and `connect` | Create virtual descriptor and evaluate realm policy | Capability-checked network proxy |
| `getrandom` | Fill guest buffer | `astrid:sys` entropy import |
| `clock_gettime` | Convert the granted clock | `astrid:time` import |
| `execve` | Resolve an executable artifact and create a child instance | No host-process spawn |
| `kill` | Update realm process state | No host signal |

The initial `Capsule.toml` must not request `host_process`. That negative property is
part of the acceptance test.

## 5. Principal ownership

The host, not an untrusted request payload, selects the principal. The realm must use
the kernel-stamped invocation identity and principal-scoped host services. A caller
cannot name another principal to select its storage.

```text
RealmId = owner principal + realm name + base image digest
```

A principal may own multiple realms, for example:

- `default`: persistent personal workbench;
- `release`: pinned, reproducible release environment;
- `experiment`: disposable or resettable environment;
- per-task clones with attenuated network and secret grants.

Access to a realm is itself authority. Delegation uses a realm handle or explicit
realm grant; sharing an agent prompt or source path does not transfer it.

The security invariant is:

> No mutable byte, process handle, descriptor, secret, or network flow belonging to
> one principal is observable by another principal unless an explicit, audited
> capability transfers that exact resource.

Shared executable artifacts and base-image blocks are immutable and
content-addressed. Sharing their storage does not share guest process state.

## 6. Persistent filesystem model

The first durable layout is:

```text
/                    immutable signed base + principal-private COW overlay
/home/agent          principal-private durable realm home
/workspace           explicit projection of a granted Astrid workspace
/mnt/astrid/<name>   explicitly granted resource mounts
/tmp                 ephemeral, quota-bound
/run                 ephemeral realm state
/proc                synthetic process view
/dev                 synthetic bounded devices
```

Do not mount Astrid's entire internal home or credential directories into the
realm. `/home/agent` is a dedicated namespace backed by principal-scoped storage.
A workspace mount is separately granted so a destructive command in the Linux home
cannot silently reach unrelated Astrid state.

The executable seed uses the same shape with a deliberately narrower guarantee:

- `/home/agent` is a principal-scoped versioned filesystem: KV atomically selects
  one immutable content-addressed manifest, while file and manifest blobs live in
  the principal's private realm file store. It has been verified across daemon
  restart;
- `/workspace` is the invocation's Astrid `cwd://` copy-on-write view. Its writes
  remain staged until the outer Astrid workflow promotes them, and an unpromoted
  view is discarded on daemon restart;
- `/tmp` maps to the invoking principal's `.local/tmp` namespace and is not part of
  the durable realm contract.

That distinction is intentional. A command running inside the realm cannot silently
commit source-tree changes merely because it can write its projected workspace.
Promotion is an outer authority decision and must produce an audit record. The
current seed does not yet expose a realm-side commit tool.

The storage identity should be:

```text
base image digest
+ overlay generation
+ owner principal
+ realm id
+ filesystem format version
-> mounted realm filesystem
```

The host should hold encryption keys and derive or unwrap them only for the bound
principal and realm. Guest root never receives an Astrid storage key.

### 6.1 Selected seed representation

The seed deliberately uses both Astrid storage mechanisms, each for the property
it is good at:

```text
KV realm/default/fs/head
  = { format, generation, manifest_digest }

file store .../store/blobs/<blake3>
  = immutable file content or immutable JSON manifest
```

KV is not the file store. It contains only the exact raw head bytes required for
an atomic compare-and-swap. The principal file store is not the transaction log.
It contains immutable, read-after-write-verified blobs identified by BLAKE3. Both
outer stores are already scoped by the kernel-stamped principal and capsule.

One create-or-truncate commit is:

1. normalize and bound the realm-relative path and bytes;
2. materialize and verify the immutable file blob;
3. load the currently selected head and manifest;
4. build, materialize, and verify a new manifest containing the replacement and
   the prior manifest digest as its parent;
5. compare-and-swap the exact old head bytes to the new head;
6. if another writer won, reload its generation, merge, and retry up to eight
   attempts.

A crash before step 5 may leave an orphan content or manifest blob, but it cannot
make a partial generation visible. A crash after step 5 leaves a complete selected
generation. Missing, tampered, malformed, or newer selected metadata fails closed.
Garbage collection can later remove objects unreachable from retained heads and
named checkpoints.

Format-0 direct-home files are lazily imported on first read. The direct path is
then maintained as a best-effort compatibility projection, never as the selected
truth. This preserves the currently deployed seed without requiring a stop-the-
world migration.

The present semantic boundary is intentionally narrow: regular-file read and
create/truncate, 64 KiB per file, a 1 MiB manifest, no delete or rename, no
directory metadata, no links, no guest flush instruction, no named checkpoint,
and no garbage collection. These omissions are reported as remaining work rather
than implied POSIX behavior.

### 6.2 Base and overlay

- The base image is immutable, signed, content-addressed, and globally cacheable.
- The overlay contains only blocks or files changed by the principal.
- The durable home can be a separate volume so it can migrate between base images.
- `/tmp`, pipes, process tables, and transient logs do not enter durable storage.
- Guest `fsync`, atomic rename, directory ordering, and full-overlay crash recovery
  require explicit tests. The seed currently proves only bounded file replacement
  and atomic selected-home generations over real Astrid storage.

### 6.3 Persistence levels

The first guarantee is durable filesystem state across realm restart. It does not
include a live RAM or process checkpoint.

```text
suspend
  -> stop admission
  -> signal processes
  -> drain bounded output
  -> flush filesystem barrier
  -> commit overlay generation
  -> destroy instances

resume
  -> verify base and overlay identities
  -> mount storage
  -> start realm init
  -> restore working-directory and shell metadata
```

Full process/memory checkpoints may be added later. Such checkpoints must bind to
the exact realm engine, guest ABI, module hashes, and filesystem generation. They
must never be treated as portable durable state by default.

## 7. Processes and scheduling

The realm owns a process table independent of host processes:

```text
ProcessId
ParentProcessId
principal-bound RealmId
executable digest
guest memory
guest globals/tables
file-descriptor table
environment
working directory
signal state
resource counters
exit status
```

Each guest module receives a distinct memory unless it is an explicitly created
thread. Pipes, sockets, files, and terminals are realm resources referenced through
descriptor numbers local to the process.

The supervisor owns scheduling. A guest cannot execute an unbounded internal loop
without returning control. The interpreter or host runtime must support fuel,
instruction slices, interruption, and bounded host calls.

The first process lifecycle is:

```text
Created -> Runnable -> Running -> Waiting -> Runnable
                      |             |
                      v             v
                    Exited        Signaled
```

Admission limits include:

- processes and threads per realm;
- guest memory per process and in aggregate;
- open descriptors;
- pipe/socket buffer bytes;
- stored output and diagnostic bytes;
- CPU fuel and wall time;
- child creation rate;
- filesystem and network quotas.

### 7.1 Process-table invariants

The first `realm-core` model is deliberately independent of Wasmi. It is the
semantic oracle that an interpreter scheduler or a faster backend must drive.
Its rules are:

- process identifiers are realm-local, monotonic, non-zero, and never reused;
- `Created -> Runnable -> Running` is explicit; only a running process may issue
  ordinary process and descriptor operations;
- a running process may yield to `Runnable`, block in `Waiting`, exit with a
  status, or be terminated by a typed signal;
- a terminated child remains in the table as a waitable zombie until its direct
  parent reaps it;
- a parent may wait only for its direct child. Waiting for a live child parks the
  parent; child termination wakes it to the runnable queue;
- parent termination reparents its children to the realm supervisor rather than
  killing them implicitly or leaving a dangling parent identifier;
- process quotas include created, live, and zombie records. Reaping releases the
  slot; identifier exhaustion fails without wrapping;
- runnable selection uses a deterministic FIFO queue. This is a reference trace,
  not yet a fairness or real-time scheduling claim.

### 7.2 Descriptor and bounded-pipe invariants

Descriptors are process-local typed references. Pipe endpoints may be inherited
into exact child descriptor numbers during spawn, including standard input,
output, and error. Spawn validates the complete mapping before allocating a PID
or incrementing an endpoint reference, so an invalid mapping cannot create a
partial child.

Each pipe has an immutable positive capacity and contributes that capacity to an
aggregate realm quota. Writes may be partial up to available capacity. A full pipe
returns `WouldBlock`; a read frees capacity and wakes parked writers. An empty pipe
returns `WouldBlock` while any writer exists, and `EOF` only after the final writer
closes. Closing the final reader wakes writers, whose next write returns
`BrokenPipe`. Termination closes every descriptor owned by that process. A pipe is
removed and its reserved capacity released only after its last read and write
endpoint close.

The model does not yet claim live `spawn`, `wait`, `pipe`, signal, or PTY guest
imports. The required owner now exists: the capsule is a long-lived run-loop actor
with one `RealmMachine` and semantic kernel per kernel-verified principal. It keeps
process identifiers monotonic across tool calls within one capsule boot, reaps
completed foreground jobs, and exposes live process, pipe, reserved-byte, command,
and next-PID accounting. Constructing a new process table inside each one-shot
tool call is no longer the live path.

The reference runtime still wires a deliberately smaller executable proof: one
foreground request creates two signed processes, maps a four-byte pipe from the
producer's stdout to the consumer's stdin, starts the consumer first, and drives
both independent Wasmi stores through repeated read and write suspension. The
producer handles partial writes; the consumer observes EOF only after producer
exit closes the last writer. The caller's fuel and captured-output budgets are
split across the two processes, while each retains the declared per-process memory
ceiling. Suspension counts are returned in process accounting. This proves the
scheduler/backend junction, bounded backpressure, resumable call stacks, and actor
ownership. It does not create guest-facing process syscalls or persistent
background jobs. Calls are currently serialized by one capsule run loop; fair
cross-principal scheduling becomes mandatory before background jobs are admitted.

Astrid's subscription host returns at most one routed message per envelope and
installs that message's principal as the invocation context before Realm KV, file,
and publish calls. The actor additionally requires `verified` attribution rather
than trusting a principal string in the payload.

Process identity is the tuple `(realm boot sequence, process id)`. The process ID
returns to 1 after a capsule restart, while a principal-scoped boot sequence is
advanced atomically with KV compare-and-swap. Read-only status does not allocate a
machine or advance that sequence. The actor admits at most 32 principal machines
per capsule boot and currently has no eviction policy; reaching the bound fails
before initializing durable state for the rejected machine.

## 8. Executable compatibility lanes

There are two compatible long-term execution lanes behind the same realm API.

### 8.1 WASM-native Linux process lane

Programs are compiled for a realm-specific WASM Linux target. They import the
realm's Linux-compatible syscall ABI and run as nested core-WASM instances.

Advantages:

- no CPU instruction emulation;
- portable across Astrid hosts;
- fast validation and deterministic fuel metering;
- explicit process memories;
- compiler and executable identities remain content-addressed.

Costs:

- packages must be rebuilt from source;
- build systems, libc, processes, signals, and threads require a toolchain port;
- JIT runtimes need an interpreter, an approved code-generation service, or a
  purpose-built integration;
- ordinary Debian binary repositories cannot be installed directly.

The prototype should embed a `no_std`-capable interpreter such as Wasmi to prove
nested module execution without adding a public dynamic-code host interface.
[Wasmi](https://docs.rs/wasmi/) supplies a deterministic interpreter, fuel metering,
and `no_std` support. This is a correctness and isolation proof, not the final
performance architecture.

### 8.2 Linux binary compatibility lane

An x86-64, AArch64, or RV64 user-mode interpreter/translator loads ordinary Linux
ELF binaries and maps their syscall ABI into the same realm kernel.

Advantages:

- existing binaries and distribution packages;
- fewer source ports;
- the closest match to a conventional Debian environment.

Costs:

- substantially larger decoder and execution surface;
- translation/JIT cache design;
- lower performance than WASM-native programs;
- difficult signals, threads, atomics, self-modifying code, and debugger behavior;
- architecture-specific compatibility testing.

If Astrid owns this engine, RV64 deserves evaluation before x86-64 because agents do
not observe the guest ISA and Debian 13 provides an official `riscv64` port.
[Debian riscv64](https://www.debian.org/releases/trixie/riscv64/ch02s01.en.html)
offers a modern package ecosystem over a much smaller instruction-set surface.

### 8.3 Dispatch

The realm may eventually support both:

```text
exec file
  -> inspect signed executable metadata and magic
  -> WASM-Linux module: instantiate directly/interpreted
  -> supported Linux ELF: run through compatibility engine
  -> unsupported: ENOEXEC
```

The selected lane is recorded in audit and accounting. It cannot change the
program's realm authority.

## 9. Fork, exec, threads, and the hard compatibility cases

The design must not hide Unix's difficult semantics.

- `execve` can replace a process image while retaining the permitted descriptor and
  signal state.
- `posix_spawn` can be supported early as direct child creation.
- `fork` requires a resumable execution state or a compiler transformation; a
  normal nested WASM call stack cannot simply be copied.
- `vfork` adds shared-state and suspension hazards and should not be claimed early.
- threads require shared memory, atomics, thread-local storage, futexes, and bounded
  scheduler integration.
- job control requires sessions, process groups, signals, and a controlling PTY.
- JIT engines require special treatment because generated code is not automatically
  executable inside an existing WASM instance.

The first guest suite should prefer programs using `posix_spawn` or a narrow spawn
API. Real Bash becomes an acceptance milestone only after pipelines, process
groups, signals, waits, and PTY behavior exist. A smaller shell may precede it.

## 10. Linux root is not Astrid authority

Guest UID 0 may administer the realm. It cannot administer Astrid.

Guest root may:

- modify the realm overlay and home;
- install packages compatible with the realm;
- create users and change guest permissions;
- trace or kill processes in the same realm subject to its internal policy;
- bind guest ports and request external network mappings.

Guest root may not:

- select a different Astrid principal;
- read host paths not projected into the realm;
- access another realm's mutable blocks;
- open raw host sockets;
- receive physical device or kernel handles;
- persist leased secrets unless policy explicitly permits it;
- sign or install an Astrid capsule;
- widen the realm's manifest grants.

The outer capsule's host imports are the maximum authority of every program inside
the realm. Child processes receive an equal or attenuated subset. There is no guest
operation that adds a new outer import.

## 11. Networking

The realm implements sockets internally but maps external connections through
Astrid's network services.

```text
guest connect(host, port)
  -> realm DNS and socket policy
  -> outer capsule capability check
  -> Astrid network proxy
  -> audited connection resource
```

Network policy is enforced outside guest root. Changing guest firewall rules cannot
override it. The default realm has no external network. Development profiles may
grant selected registries, source hosts, or model providers.

Inbound services require an explicit portal/ingress grant with a bound realm,
process, port, protocol, lifetime, and public/private scope. Listening on guest
`0.0.0.0` alone creates no host listener.

Dependency resolution should prefer a host-mediated, content-addressed fetch service
over unrestricted guest egress. The resulting package identity and bytes enter the
build record.

## 12. Secrets and identity

Secrets remain in Astrid key custody. A process receives a leased descriptor,
read-once stream, or ephemeral file injection scoped to an exact command and
lifetime. Persistent environment variables and files are not the default.

The realm must distinguish:

- guest usernames and UIDs, which are local compatibility data;
- the owner principal, stamped and enforced by Astrid;
- delegated callers authorized to use a realm;
- external service identities obtained through explicit Astrid providers.

No guest-provided UID, username, environment variable, or path substitutes for the
outer principal identity.

## 13. Agent and human interfaces

The capsule uses existing tool-bus conventions without creating a public WIT
package. The live seed exports:

- `linux_realm_exec`: run one exact signed program with structured arguments, CWD,
  and caller-reducible fuel/output limits;
- `linux_realm_status`: report the guest-visible mounts, supported programs, owner
  principal, and workspace commit policy without physical host paths.

The longer contract may add:

- `linux_realm_shell`: open or resume an interactive PTY session;
- `linux_realm_read`: read bounded output;
- `linux_realm_signal`: signal a process or job;
- `linux_realm_snapshot`: commit a named filesystem generation;
- `linux_realm_clone`: create an attenuated clone;
- `linux_realm_reset`: return a realm to its selected base;
- `linux_realm_export`: export an artifact through an explicit boundary.

The canonical execution input is structured `argv`. `bash -lc` is an explicit
compatibility operation, not the only command interface.

A human terminal is a PTY client of the same realm. No GUI, login manager, or local
display is required. An agent can operate entirely through structured calls and
streams.

Once another capsule needs to hold realm/process/stream resources directly, the
contract becomes a public cross-capsule surface and requires an Astrid RFC plus
canonical WIT changes.

### 13.1 Who uses the realm

The realm is the default execution workbench for an agent, not a runtime silently
linked into every capsule.

- The react loop and human clients invoke realm tools for interactive shell work,
  compilation, Git, tests, and development services.
- A capsule that needs a Linux program submits an explicit bounded job through a
  realm service contract. Its manifest must authorize the exact request and result
  topics; the dependency is visible in composition and audit.
- Capsules that already have narrow native interfaces—HTTP, filesystem, identity,
  sessions, model providers—continue to use those interfaces. Routing an HTTP GET
  through Bash and `curl` would add authority and failure modes without benefit.
- Build-oriented capsules such as Forge may eventually delegate compilation to a
  disposable realm clone and receive only a verified artifact plus build receipt.
- The current `aos-shell` capsule remains the explicitly privileged host-process
  compatibility path until the realm has real shell/process/toolchain parity. It
  can then leave the default CE set without deleting the emergency compatibility
  option.

There is normally one durable realm per principal and profile, with many bounded
process jobs. There is not one Linux image per calling capsule. Jobs that execute
untrusted package hooks or builds can run in disposable clones derived from the
principal's selected snapshot.

Delegation must not union authority. The effective authority of a submitted job is
the intersection of:

```text
calling principal's rights
∩ calling capsule's declared realm-service rights
∩ selected realm's outer grants
∩ per-job limits and explicit resource portals
```

The realm cannot use its own broader network, workspace, or secret access on behalf
of a narrower caller. Results return on a call-bound response channel, and process,
stream, snapshot, and artifact handles are unforgeable, principal-bound, scoped to
the caller, and generation-checked.

## 14. Capsule construction

The implementation belongs to the `unicity-aos/aos-ce` product monorepo. It is a
first-party system capsule, not another standalone capsule repository. Keep its
private ABI, runtime, guest programs, and image recipe together so they can change
atomically:

```text
capsules/capsule-linux-realm/
  Cargo.toml             outer Astrid capsule and tool adapter
  Capsule.toml
  src/
    lib.rs               tool surface, command admission, result accounting
    actor.rs             run loop and principal-isolated Realm machine ownership
    host.rs              mount normalization and Astrid VFS adapter
  crates/
    realm-abi/          guest syscall numbers, records, errno, executable metadata
    realm-core/         process, descriptor, scheduler, signal, namespace model
    realm-vfs/          mounts, paths, overlay, persistence, crash semantics
    realm-runtime/      nested WASM interpreter and process instances
  guests/
    smoke-write/        one write + exit guest
    pwd/                explicit CWD inspection
    echo/               argument-vector proof
    write-file/         truncate/create through a guest descriptor
    cat/                bounded streaming read through a guest descriptor
  images/
    minimal/            reproducible root image recipe and manifest
  tests/
    traces/             syscall, crash, isolation, and persistence traces
```

Do not split the syscall kernel, filesystem, runtime, and capsule adapters into
separate repositories until their interfaces and release cadence are independently
stable.

The internal crates are explicit members of the root `aos-ce` Cargo workspace and
are host-testable. The capsule remains `wasm32-unknown-unknown` and calls only
audited `astrid:*` imports. Host-target tests exercise the state machines and
filesystems; capsule builds prove portability; an Astrid E2E harness proves
installation and principal enforcement.

## 15. Executable seed

The proof deliberately avoids Bash, Debian, networking, and a large image.

### Inputs

- an installable `aos-linux-realm.capsule`;
- six embedded core-WASM command guests, including the stdin consumer used by the
  two-process pipe proof;
- a private ABI containing bounded argument and CWD reads, file descriptors,
  open/read/write/close, monotonic time, and exit;
- Wasmi configured with fuel and memory limits;
- structured `linux_realm_exec` and read-only `linux_realm_status` tools;
- one long-lived run-loop actor with a separate Realm machine per verified
  principal, CAS-allocated boot identity, and bounded aggregate admission;
- `/home/agent`, `/workspace`, and `/tmp` mount projections;
- no `host_process` grant.

### Behavior

```text
agent invokes linux_realm_exec(write-file, ["notes.txt", "hello"], /home/agent)
  -> outer capsule resolves kernel-stamped principal
  -> realm initializes only that principal's layout
  -> realm validates the guest CWD and exact command name
  -> realm creates a process argument vector and descriptor table
  -> interpreter validates and instantiates guest module
  -> guest opens and writes a realm-local descriptor
  -> realm resolves the normalized path through the admitted mount
  -> close materializes immutable blobs and CAS-selects a new home generation
  -> guest exits with status 0
  -> capsule returns bounded stdout/stderr/status/accounting
```

### Required negative cases

- malformed guest module;
- undeclared guest import;
- out-of-bounds guest pointer;
- output over limit;
- fuel exhaustion;
- guest memory growth over limit;
- unknown descriptor;
- invalid syscall number;
- trap during a host call;
- forged principal in the tool payload;
- path traversal and unmounted absolute paths;
- shell syntax presented as a command name;
- typed Astrid host errors mapped to stable realm I/O faults;
- selected metadata missing, malformed, from a newer format, or content-tampered;
- an interrupted commit leaving unselected blobs;
- concurrent head loss, bounded retry, and persistent contention;
- concurrent invocation state leakage.
- status allocating or mutating a previously idle principal machine;
- overlapping boot allocation selecting the same principal boot identity;
- failed process/pipe admission consuming a PID or retaining kernel resources;

### Exit condition

An installed Astrid capsule runs signed nested WASM commands through an internal
Linux-shaped descriptor boundary and a long-lived principal Realm actor, returns
exact output and accounting, reads and writes principal-scoped storage, survives a
daemon restart where promised, rejects cross-principal home and process-state
reads, traps safely, reaps foreground resources, and has no host-process
capability.

This proves the recursive containment model. It does not prove POSIX or Linux
compatibility.

### Seed evidence recorded on 2026-07-18

- the seed is based on `unicity-aos/aos-ce` main commit
  `dfa1d71c2737a016d8d4dd169d0755ff624f6b50`;
- sixty-three focused host tests pass, including nested argument/CWD delivery,
  file round trips across process instances, path confinement, stable host-error
  mapping, the actual manifest authority test, fuel exhaustion, memory/output
  admission, selected-generation restart reconstruction, competing-writer merge,
  orphan invisibility, corruption failure, process lifecycle and wait/reap,
  deterministic scheduling, descriptor inheritance, pipe EOF/backpressure,
  endpoint wakeups, quota failure atomicity, identifier exhaustion, actor PID
  continuity, principal machine isolation, idle-status non-mutation, CAS boot
  encoding, aggregate actor admission, and foreground cleanup;
- the reference runtime drives a signed `echo | stdin-cat` workload as two
  isolated, resumable Wasmi stores over a four-byte pipe; both processes block and
  resume under measured backpressure and reproduce the exact input plus newline;
- the complete non-WASI capsule workspace checks under
  `wasm32-unknown-unknown`;
- the current stable Wasmi 1.1.0 runs with default `std` and WAT parsing disabled,
  BTree-backed collections, extra runtime checks, fuel, and store limits;
- `astrid-build` 0.10.1 produced a 396,742-byte (387 KiB) final actor artifact
  with SHA-256
  `42e393e7ff19dfd9471587651acce550b2df151a9ad556c00c321e5ced349ad6`;
  Astrid 0.10.1 loads it as a shared component with `host_process=false`;
- the two outer archive digests differed because `astrid-build` copied the
  rebuilt component's modification time into its tar header. Reproducible outer
  `.capsule` bytes therefore remain an identified `astrid-build` defect, not a
  property established by this seed;
- a live isolated daemon exposed both realm tools through the Astrid MCP bridge;
  session ingress and capsule grant-on-use were explicitly elicited;
- live `pwd` returned `/workspace`; `write-file` followed by `cat` returned exact
  bytes from both `/workspace` and `/home/agent`;
- after a full daemon stop/start, `/home/agent/persist.txt` retained its exact
  contents while the unpromoted `/workspace/realm-live.txt` disappeared, matching
  the declared outer-promotion contract;
- a second authenticated principal, `realm_alice`, received its own tool result
  identity and an `io-not-found` fault for the default principal's durable
  `persist.txt`; it wrote and read `alice-only.txt`, while the default principal
  received `io-not-found` for that same name;
- the upgraded live realm reported `migration-required` before first mutation;
  reading each principal's format-0 file lazily selected an independent
  generation-1 manifest, the default principal's next write selected generation 2
  with two files, and both exact heads and contents survived a full daemon
  stop/start;
- the packaged `pipe-echo` workload ran through the live Astrid MCP front door as
  two processes with 128 KiB aggregate linear-memory ceiling, 1,198 consumed
  interpreter fuel, 15 measured read/write suspensions over a four-byte pipe, and
  exact stdout `live resumable realm pipeline\n`;
- a clean actor-only fixture with current `aos-cli`, `sage-mcp`, and
  `aos-linux-realm` exercised the actual tool bus. Default ran PID 1, then a
  two-process pipeline at guest PIDs 3 and 4 (PID 2 was the reaped supervisor),
  and reported next PID 5 with zero retained process records, pipe objects, or
  reserved pipe bytes;
- after a full daemon restart, default returned to PID 1 under boot sequence 2.
  A normal least-privilege `realm_alice` profile, granted only the three test
  capsules, independently started at PID 1 and boot sequence 1. Each status kept
  its own command count and next PID, demonstrating that mutable process state is
  principal-isolated even though the capsule component instance is shared;
- the final 396,742-byte artifact was then installed into every test view and loaded
  as one shared component hash. Immediately after restart, read-only status
  returned `actor_state=idle`, boot sequence 0, command count 0, and next PID 1;
  the first execution atomically selected the principal's next durable boot
  sequence (4 in the reused fixture), ran as PID 1, and left zero process and pipe
  resources. The following status returned `running` with that same boot sequence,
  one completed command, and next PID 2;
- the initial live run discovered two integration constraints rather than hiding
  them: Astrid's component FileHandle methods are not implemented, so the adapter
  uses bounded whole-file I/O and commits on descriptor close; `/tmp` must be
  authorized through the dynamic principal-home scheme so the manifest gate checks
  the resolved principal path.
- the normal `astrid start` path selected an installed 0.10.0 companion daemon even
  though the invoking CLI, builder, and realm requirement were 0.10.1; that daemon
  correctly rejected the `astrid-version >=0.10.1` capsule. Running the locally
  built 0.10.1 daemon proved the realm, but AOS startup must select and verify the
  exact product-pinned runtime companion before the realm enters the default set.

The earlier persistence E2E run used the then-current `astrid-mcp` capsule as its
front door. The actor E2E used the product `aos-cli` proxy and current `sage-mcp`
broker. The Realm still must not enter the default distribution until that broker
and invocation path are part of the supported CE set rather than test-installed
companions.

## 16. Ordered implementation milestones

### Milestone A: nested process proof

- scaffold the new capsule directory in the product monorepo and pin its
  dependencies;
- implement typed realm/process/descriptor identifiers;
- integrate Wasmi without default `std`/WAT features where portability requires;
- implement bounded `write`, `clock`, `exit`, and fuel accounting;
- build, package, install, and invoke the smoke guest;
- record artifact hashes and exact commands.

### Milestone B: files and persistence

- [x] implement normalized, non-escaping path resolution;
- [x] mount a principal-private durable home, projected COW workspace, and
  principal-private temporary namespace;
- [x] implement bounded sequential open/read/write/close using whole-file Astrid
  VFS calls;
- implement seek/stat/rename/unlink and a real flush barrier;
- add immutable base plus COW overlay generations;
- kill the realm during writes and verify declared crash semantics;
- restart and observe the same principal's bytes, but never another's.

### Milestone C: processes and shell substrate

- [x] define the host-testable process lifecycle, direct-child wait/reap,
  deterministic single-runner FIFO scheduling, typed terminal signals, bounded
  pipes, atomic descriptor inheritance, and aggregate process/pipe quotas;
- [x] bind resumable Wasmi process slots to the kernel for a foreground
  two-process stdout-to-stdin pipeline with measured suspension and exact output;
- [x] add a long-lived principal Realm actor with per-boot PID continuity,
  restart-disambiguating boot identity, verified-principal isolation, aggregate
  admission, foreground cleanup, and live accounting;
- [ ] add guest `exec`, `posix_spawn`, wait, pipe, and signal imports;
- [ ] add PTYs, sessions, process groups, and job-control signals;
- run multiple guest modules with isolated memories;
- compile and run a small shell;
- add job control only with explicit conformance tests.

### Milestone D: useful agent workbench

- produce a signed minimal image with shell, Git, and Python;
- add workspace projection and artifact export;
- add a mediated dependency fetch path;
- persist the agent home and tool caches;
- run a real repository inspection/edit/test loop;
- build an Astrid capsule inside the realm and install it only after independent
  verification.

### Milestone E: compatibility breadth

- choose and document the WASM-Linux toolchain target;
- build a reproducible package set;
- run Bash conformance cases;
- add Node or another agent CLI runtime only after its JIT/process assumptions are
  explicit;
- evaluate RV64/x86-64 Linux ELF compatibility;
- define package manager behavior honestly.

### Milestone F: backend substitution

- run the same realm contract over a faster host-managed nested-WASM runtime;
- optionally add a hardware Linux VM backend for native hosts;
- compare behavior with the interpreter oracle;
- retain the same principal storage and capability boundaries.

## 17. Theory and adversarial scenario matrix

The design must be tested against at least these scenarios:

| Scenario | Expected result |
|---|---|
| Principal A and B open `default` | Distinct mutable homes and overlays |
| Both use the same base/tool binary | Immutable bytes may be deduplicated |
| A supplies B's principal id in JSON | Ignored/rejected; host-stamped A is used |
| Guest root reads host path | No mount and no corresponding host import |
| Guest root rewrites network config | Outer egress policy remains unchanged |
| Guest forks without support | Explicit `ENOSYS`, never partial cloning |
| Guest loops forever | Fuel/deadline terminates or suspends it |
| Guest writes infinite stdout | Bounded buffer/backpressure, then failure |
| Realm crashes after data write before flush | Behavior matches declared durability point |
| Base image is upgraded | Existing realm remains bound or migrates explicitly |
| Snapshot uses another engine version | Rejected unless compatibility is established |
| Agent builds malicious capsule | Candidate leaves only through verifier/install policy |
| Process requests more authority than realm | Denied; children cannot widen outer grants |
| Package script accesses a secret | No secret unless exact lease was granted |
| Shared cache is corrupted | Digest mismatch rejects entry without cross-principal mutation |
| Realm capsule traps | Supervisor restarts capsule; durable generation remains consistent |
| Revocation occurs during network I/O | Connection closes and stale descriptor fails |
| Delegated caller uses realm | Only exact delegated realm rights are available |
| Realm is deleted | Keys/handles revoked and durable blocks become collectible |
| Guest writes projected workspace | Change remains in Astrid COW until outer promotion |
| Daemon restarts before promotion | Durable home remains; staged workspace is discarded |
| Actor restarts and PID 1 is reused | Boot sequence advances; the identity tuple remains unique |
| Principal B executes while A is warm | B receives a distinct machine, PID namespace, counters, and boot sequence |
| Actor principal bound is exhausted | New execution fails before Realm state is initialized |

## 18. Measurements

Record rather than assume:

- cold capsule startup;
- warm realm startup;
- guest module validation and instantiation;
- syscall calls per command;
- interpreter instructions per useful instruction;
- process spawn and pipe latency;
- filesystem small-file and sequential throughput;
- overlay amplification and flush latency;
- memory per process and per realm;
- fuel-to-wall-time stability;
- Git, Python, compilation, and capsule-build workloads;
- artifact export and verification time.

The interpreter may be slow and still be the right first reference backend. A
faster backend is admitted only after it reproduces the reference traces and
preserves revocation and accounting.

## 19. AOS Realm distribution and image policy

The project should own the image recipe, package selection, signatures, and update
policy. It should not claim to be Debian merely because it has a familiar directory
layout.

The distribution is `AOS Realm`. It may report a Linux-compatible kernel ABI where
required by a compiled program, but its identity and provenance remain explicit:

```text
NAME="AOS Realm"
ID=aos-realm
ID_LIKE=linux
VARIANT_ID=agent-workbench
```

The distro is designed for agents as operators. Familiar commands remain, but the
system also exposes structured truth that a shell traditionally hides:

- `realm status --json` reports the base digest, overlay generation, budgets, and
  effective external grants;
- `realm why <path-or-program>` explains which immutable package supplied a byte;
- `realm authority --json` reports workspace, network, secret, clock, and export
  portals without exposing Astrid credentials;
- `realm checkpoint <name>` commits a filesystem generation transactionally;
- `realm diff <generation>` reports durable changes before export or reset;
- every capsule build produces a receipt binding sources, compiler, dependencies,
  realm image, output digest, and verifier decision;
- package hooks execute in disposable attenuated child realms rather than gaining
  the authority of the interactive workbench.

Possible profiles are:

- `aos-realm-minimal`: shell, files, pipes, core utilities;
- `aos-realm-python`: Python plus pinned package tooling;
- `aos-realm-rust`: Rust toolchain, linker, and `astrid-build`;
- `aos-realm-agent`: supported agent CLI runtime and exact dependencies.

Every image is immutable and identified by its manifest and block hashes. Package
installation mutates only a realm overlay. Reproducible jobs name an exact base and
overlay snapshot rather than relying on the current contents of `default`.

Full Debian `.deb` compatibility is a binary-emulation milestone. Until then,
`apt` must either use an AOS-built package repository for the guest target or be
absent. It must not silently download incompatible host binaries.

## 20. Relationship to current Astrid

This work preserves the current system:

- current capsules remain Component Model artifacts;
- current manifest, topic, principal, capability, and install semantics remain;
- `aos-shell` remains a host-process compatibility tool during migration;
- the realm becomes the default agent shell/build service only after measured
  parity, while ordinary capsules keep their narrow interfaces;
- the Linux realm does not replace the kernel's capability enforcement;
- a capsule built inside the realm is the same canonical `.capsule` artifact built
  elsewhere;
- the daemon and future native host can run the same realm capsule;
- the native kernel no longer needs hardware virtualization merely to provide an
  initial agent workbench.

The Linux realm is a system capsule with unusually rich internal semantics. It is
not a second authority kernel. Astrid remains responsible for principal identity,
outer grants, installation, revocation, metering, audit, and recovery.

## 21. Open decisions

The first implementation must resolve these with executable evidence:

1. Is the measured Wasmi reference backend fast enough for the first filesystem
   and compiler workloads, or should it remain only the semantic oracle?
2. Should the internal guest ABI use named functions, a syscall dispatcher, WASI
   compatibility, or generated shims?
3. Which shell can prove process semantics before Bash?
4. Which retention and garbage-collection policy should preserve named
   checkpoints over the now-selected KV-head/content-addressed-blob filesystem?
5. Is direct principal VFS projection safe enough, or should the first realm use a
   single private block volume?
6. How are guest continuations represented for `fork`, signals, and blocking calls?
7. Which dynamic-code mechanism is allowed for Node and other JIT runtimes?
8. Can BrowserPod/Cheerp technology be licensed or upstreamed usefully, or should it
   remain comparison evidence only?
9. Should binary compatibility target RV64, x86-64, or neither until the
   WASM-native package set is measured?
10. At what stable boundary does a public realm WIT RFC become necessary?
11. Which existing first-party capsules genuinely need bounded realm jobs, and
    which must retain a narrower service dependency?
12. What fair scheduling and admission policy should replace serialized foreground
    calls before principals may keep background jobs?
13. Should idle principal machines be evicted, and if so which process, descriptor,
    and boot-generation conditions make eviction observable and safe?

## 22. Implementation ledger and immediate task list

- [x] place `capsule-linux-realm` in the authoritative `unicity-aos/aos-ce`
  monorepo;
- [x] freeze the seed guest ABI to `write`, `clock-monotonic-ns`, and `exit`;
- [x] pin the current stable Wasmi 1.1.0, WAT 1.253.0, and Astrid SDK 0.7.1;
- [x] compile Wasmi inside the outer `wasm32-unknown-unknown` capsule;
- [x] run the embedded smoke guest under fuel, memory, descriptor, and output
  limits;
- [x] return exact result, trap, and accounting records through the existing tool
  topic convention;
- [x] produce an installable artifact with no `host_process` capability;
- [x] test malformed modules, undeclared imports, invalid pointers, unknown
  descriptors, fuel exhaustion, output exhaustion, memory admission, and forged
  principal input;
- [x] deliver structured command/argv/CWD execution for `pwd`, `echo`,
  `write-file`, and `cat` without a host shell;
- [x] add normalized `/home/agent`, `/workspace`, and `/tmp` projections;
- [x] verify principal-scoped home persistence across daemon restart and reject a
  second principal's read of those bytes;
- [x] invoke the packaged capsule through a live Astrid 0.10.1 daemon and MCP
  front door, including ingress consent and grant-on-use;
- [x] add crash-consistent principal-home generations with an atomic KV head,
  immutable content-addressed manifests and files, bounded concurrent-writer
  retry, corruption checks, and lazy migration from the format-0 direct home;
- [ ] add explicit guest flush semantics, retained named checkpoints, diff/reset,
  and unreachable-blob garbage collection;
- [x] implement `aos-realm-core` as the backend-independent process/descriptor
  oracle, including monotonic PIDs, zombies, direct-child wait/reap, reparenting,
  deterministic admission, bounded pipes, endpoint inheritance, wakeups, EOF,
  broken-pipe behavior, and failure-atomic quota checks;
- [x] run two signed guest modules with isolated memories through the core
  scheduler, a four-byte bounded pipe, resumable read/write host calls, partial
  producer writes, consumer EOF, and exact combined accounting;
- [x] add a long-lived principal Realm actor with isolated machine state,
  monotonic per-boot PIDs, CAS-allocated boot sequences, read-only idle status,
  a 32-principal aggregate bound, and foreground process/pipe cleanup;
- [ ] expose the tested spawn, wait, pipe, and signal operations through the
  private guest ABI without allowing background jobs to escape actor accounting;
- [ ] add an outer workspace diff/promote workflow; realm code must not silently
  commit its own COW projection;
- [ ] put a supported MCP broker/invocation front door in the CE distribution
  before selecting the realm by default;
- [ ] make product startup verify and launch the exact pinned Astrid daemon rather
  than an older installed companion;
- [ ] add guest-created processes/pipes, waits, signals, and a small shell now
  that persistence and the foreground resumable pipeline are established;
- [ ] define the attenuating capsule-to-realm job contract and migrate Forge as the
  first non-interactive consumer after artifact verification exists;
- [ ] replace `aos-shell` in the default distro only after interactive and
  background-process parity is measured; retain an explicit compatibility path;
- [ ] fix or consume an `astrid-build` release that normalizes archive metadata,
  then add a same-input/same-capsule-digest reproducibility test;
- [ ] keep the seed out of the signed Unicity CE default capsule set until it is a
  useful workbench;
- [ ] defer public WIT, Debian naming, arbitrary package claims, and native-kernel
    coupling until evidence requires them.

The next executable artifact exposes the smallest honest
`posix_spawn`/wait/pipe path from guest code into the now-live principal actor,
including handle generation checks, bounded background admission, cancellation,
and deterministic cleanup. The storage track adds named checkpoint/diff/reset and
outer workspace promotion. A tiny shell follows those live mechanics; Bash and a
compiler remain acceptance workloads.
