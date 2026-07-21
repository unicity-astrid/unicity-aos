# AOS Principal Linux Realm Capsule

Status: active implementation programme; bounded Linux guest and workspace portal live

Last reviewed: 2026-07-21

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
  -> realm process and resource kernel inside the capsule
  -> signed core-WASM processes, or an admitted RV64 virtual machine
  -> eventually Linux plus AOS Realm userland on the RV64 machine
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

### 1.1 Architectural lineage

Astrid should inherit Plan 9's compositional principle without making its file
protocol the universal native ABI. Plan 9 gives each process a private namespace
assembled from local, remote, and synthetic services. Astrid generalizes that
idea into a principal-scoped graph of typed interfaces, where every edge also
carries authority, provenance, budget, lifecycle, and audit semantics:

```text
Plan 9 process namespace     -> Astrid principal capability graph
file server plus 9P          -> capsule plus typed WIT/IPC/resource interface
mount and bind               -> Dock compatible typed inputs and outputs
path-mediated access         -> explicit capability-mediated access
```

The native rule is therefore “everything is an explicitly governed interface,”
not “everything is a file.” Filesystem, command, graphical, model, stream, and
device views are projections of typed capabilities for particular consumers.
Linux legitimately uses a 9P projection because its in-kernel v9fs client already
turns that protocol into ordinary POSIX operations; native capsules should retain
their richer types rather than being forced through byte streams and pathnames.

Other lineages supply properties Plan 9 did not attempt to provide alone:

- object-capability systems and seL4-style boundary reasoning for narrow,
  possession-based authority and testable isolation claims;
- Erlang/OTP for supervision trees, mailboxes, restart policy, and durable versus
  ephemeral service lifecycle;
- exokernels and unikernels for principal- and workload-specific mechanisms;
- content-addressed systems for reproducible capsules, toolchains, package state,
  and Linux images;
- the WebAssembly Component Model for the typed, portable ABI between capsules.

Tensor Logic is reserved as a later reasoning and composition language over the
typed interface graph. It is not a knowledge graph and is not required for the
current execution path. The present design records enough typed input/output,
authority, resource, and lifecycle information for that algebra to be added
without changing the capsule boundary.

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

### 2.4 Embedded backend decision, 2026-07-18

The embedded backend is an AOS-owned, interpreter-first RV64 virtual machine. AOS
owns the machine model, resource limits, scheduling slices, console, interrupt and
clock behavior, block-overlay semantics, device tree, snapshots, and capability
portals. It may adapt narrowly scoped permissive implementation techniques, but no
third-party browser runtime defines the Realm boundary.

The decision was made against current upstream state rather than project names:

| Candidate pinned during evaluation | Useful property | Why it is not the embedded backend |
|---|---|---|
| CheerpX 1.2.8 and WebVM commit [`007fedb`](https://github.com/leaningtech/webvm/tree/007fedb26946a8c31563dab2fed9d85bcd1f8963) | Strong x86 Linux userland, persistent overlay, excellent product proof | Requires a browser JavaScript environment, `SharedArrayBuffer`, workers, IndexedDB/WebSockets and CheerpX licensing; not a core-WASM library that can be linked into this capsule |
| v86 tag `latest`, commit [`2f1346b`](https://github.com/copy/v86/tree/2f1346b0e7d88d4cbbbcc05fe15b4e369c3de23f) | BSD-licensed full x86 PC with Linux and virtio devices | Browser/JavaScript orchestration and runtime generation of new WebAssembly modules are fundamental to its fast path; Astrid's nested interpreter does not provide that host |
| `rvemu` 0.0.11, current commit [`f55eb5b`](https://github.com/d0iasm/rvemu/tree/f55eb5b376f22a73c0cf2630848c03f8d5c93922) | MIT Rust RV64GC, Sv39, PLIC/CLINT/virtio, and a `wasm32-unknown-unknown` build | The Wasm UART calls the browser DOM, construction runs host `dtc`, RAM eagerly allocates 1 GiB, execution is unbounded, and its Linux recipe remains on 4.19; it is comparison material, not a dependency or source donor |
| `semu` commit [`fd08129`](https://github.com/sysprog21/semu/tree/fd0812970c3c934b46e4897284894e9705355b50) with rolling `prebuilt` tag [`c722c11`](https://github.com/sysprog21/semu/tree/c722c1140770a67e638fd8154446825563ebd738) | Active MIT full-system Linux proof with virtio block, filesystem, network, input, sound, and 2D GPU | It is an RV32IMA C host program with native host integrations, not an embeddable RV64 core-WASM component; its device behavior remains valuable comparison evidence |
| `vpod` release [`v0.4.0`](https://github.com/capsulerun/vpod/tree/v0.4.0), tag commit `3e56b8b`, audited main `08bf0fb` | Apache-2.0 Rust RV64GC, decoded-block cache, copy-on-write RAM, virtio devices, delta snapshots, and a working Alpine 3.23 RV64 snapshot | Its shipped product wraps the machine in a WASI 0.2 component and Wasmtime CLI, and its filesystem/network devices currently own ambient host effects. It is comparison material only: AOS will not vendor, import, or depend on its machine crates |

The first `aos-rv64-virt-v0` slice is intentionally smaller than all four. It has
explicitly admitted contiguous RAM, a slice-driven interpreter for the initial
RV64I integer surface, a bounded 16550 UART subset, and the standard test finisher.
`linux_realm_exec({"command":"rv64-smoke"})` executes 23 real RISC-V
instructions, returns `AOS RV64\n` from virtual UART, and halts with the standard
pass value. The result names `aos-rv64-interpreter` as its backend. Fuel exhaustion,
architectural traps, RAM bounds, and serial-output exhaustion all remain bounded
by the outer Realm rather than escaping into host behavior.

### 2.5 Development backend and WASI boundary, 2026-07-21

The first Linux image proved the machine, policy, lifecycle, and workspace portal,
but it also established a hard workload boundary. Its 32 MiB Buildroot initramfs,
256 MiB machine ceiling, instruction-at-a-time RV64 interpreter, and synchronous
per-file 9P path are not a credible Rust compiler environment. Adding `rustc` to
that initramfs would make an image larger without making compilation useful.

The current direction is therefore two conforming backends, not two operating
systems:

| Profile | Purpose | Required property |
|---|---|---|
| `aos-rv64-interpreter` | Executable specification, recovery image, exact traps, denied-path tests, trace oracle | Small, deterministic, wholly AOS-owned, and allowed to be slow |
| `aos-rv64-block` | Agent development workbench, package tools, compilation, and larger repositories | Engine-free core-WASM implementation with decoded-block execution, snapshot boot, local block storage, and the same outer Realm contract |

The official `vpod` v0.4.0 macOS launcher was used only as comparison evidence.
Its snapshot booted Linux 6.18.0-3-lts and Alpine 3.23.0 as `riscv64`; it did not
contain Rust, and package networking was not usable in that run. Separately, its
unmodified `riscv-core` and `machine` crates both passed `cargo check` for
`wasm32-unknown-unknown`. This is evidence that decoded execution, copy-on-write
memory, virtio devices, and snapshots can be implemented without Wasmtime, a
browser, JavaScript workers, or a WASI engine. It is not a source selection: AOS
will implement applicable techniques within its own machine model. It does not
yet prove compiler performance, Astrid device integration, or semantic
equivalence.

The adaptation boundary is strict:

```text
Linux process
  -> Linux syscall and VFS
  -> AOS-owned virtual CPU and devices
  -> typed backend suspension or portal request
  -> Realm policy and principal-scoped resource admission
  -> existing astrid:* host import
  -> runtime capability check, meter, audit, and physical provider
```

No embedded Wasmtime instance, browser worker, JavaScript shim, sidecar daemon,
ambient host directory, or independent network stack may sit beside that path.
The backend may retire guest instructions and model devices; it may not decide
which principal, file tree, endpoint, clock, secret, GPU, or durable generation is
authorised.

WASI is not itself an execution engine and is not rejected as an interface
lineage. Astrid may expose WASI-derived or WASI-compatible host functions where
their resource semantics are useful. Such a provider must be an Astrid provider:

- every file descriptor or stream originates from an admitted Astrid resource;
- preopens are derived from principal capabilities rather than ambient process
  directories;
- sockets are created only through an admitted network portal;
- wall clock, monotonic clock, entropy, terminal, and poll behavior remain typed,
  metered host effects;
- revocation, attenuation, audit, and principal identity remain Astrid concepts;
- implementing a WASI interface does not select Wasmtime, WASI Preview 2's
  reference host, or any other engine.

For the first accelerated backend, AOS should implement only techniques justified
by measurements and conformance tests, behind its AOS-owned backend interface.
External implementations remain attributed, commit-pinned research evidence;
they are not distribution dependencies or vendored source. Any future proposal
to reuse literal third-party source requires a separate architecture, licence,
provenance, and conformance decision. It is not part of the current plan.

The backend interface is private until two implementations exist. It needs typed
operations for admission, bounded execution, console input/output, device
requests, clean shutdown, checkpoint/restore, and accounting. A backend result
must carry the stable semantic outcome plus backend-specific counters; callers
must never branch on an implementation's Rust type or ambient host feature.

The first measured fast path remains inside the reference machine. A fixed-size,
machine-local Sv39 translation cache keys entries by address space, virtual page,
effective privilege, access type, and permission-relevant status. `sfence.vma`
and accepted address-space changes conservatively flush the whole cache. Every
guest instruction still crosses the existing one-step execution, interrupt,
timer, fuel, trap, and host-suspension path.

The host-side release measurement on 2026-07-21 used the checked-in Linux image
and `cargo run --release -p aos-realm-machine --example boot_linux --
capsules/capsule-linux-realm/linux/Image 250000000`. Both runs reached the first
unwired 9P request after exactly 15,899,016 charged steps at the same PC:

| Machine state | Steps/s | Sv39 walks | PTE reads | Meaning |
|---|---:|---:|---:|---|
| Instrumented interpreter before the cache | 35,355,391 | 21,809,261 | 43,624,126 | Immediate same-session baseline, not a cross-host benchmark |
| AOS translation cache enabled | 51,270,155 | 58,484 | 117,122 | 21,750,777 cache hits, 58,484 misses, and 141 conservative flushes |

That is about 45% more native-host steps per second and roughly 373 times fewer
page walks for this boot. It selects address translation as a proven fast path;
it does not predict core-WASM throughput. The measurement example now emits
these counters so native and Astrid-hosted builds can be compared explicitly.

The next route is also implemented for the default 32 MiB Realm. The AOS machine
boots the exact checked-in image to its first unanswered home-9P request, drains
the boot console into the build receipt, verifies that no console input exists,
and emits a sparse durable checkpoint. The current artifact is 8,495,869 bytes
and represents 15,899,016 precomputed guest steps. It is bound to BLAKE3 digests
of both `Image` and `SOURCES.lock`; its codec checksum, machine model, CPU state,
CSR masks, RAM envelope, sparse-page order, pending request identity, response
buffer, and trailing bytes all fail closed. The signed capsule containing the
image, lock, and checkpoint supplies authenticity.

Restoration does not restore authority. The checkpoint stops before Linux's
first storage response, so each new principal still creates a fresh Astrid home
9P session and a fresh invocation workspace session. Only then can Linux finish
mounting and print `AOS READY`. Under the normal large boot budget that resume
uses 277,798 charged steps and seven cooperative host/slice suspensions, about
57 times fewer guest steps than replaying the cold kernel boot. A 200,000-step
request also reaches the already-emitted ready marker at its final slice
boundary; this is covered separately so fuel-boundary behavior remains explicit.
Non-32-MiB envelopes use the measured full boot rather than accepting a mismatched
checkpoint. Full capsule tests preserve UID-1000 commands, durable home readback,
workspace remounting, shutdown, restart, denied paths, and accounting.

A same-session attempt to prioritize RAM before MMIO dispatch and bypass the
generic instruction-fetch device path measured 50,994,179 steps/s versus the
cached reference's 51,270,155. That is noise, not a win, so the change was
discarded. The next CPU experiment must target decoded dispatch or bounded
blocks and must beat the committed measurement while preserving per-step
interrupt and fuel semantics.

The remaining performance routes are ordered by the boundary they remove:

1. make RAM access and device dispatch cheap after translation;
2. [implemented for 32 MiB] restore a signed-in prewarmed machine checkpoint
   instead of replaying 15.9 million boot steps for every cold principal;
3. add decoded instruction or bounded basic-block caching only if post-TLB
   measurements justify it, preserving an interrupt/fuel boundary per step;
4. serve immutable distribution and toolchain bytes from shared content-addressed
   pages with principal-local copy-on-write overlays;
5. replace chatty file-at-a-time workspace traffic with metered bulk reads,
   staging, diffs, and artifact promotion over Astrid resources;
6. admit native virtualization as an optional host provider only where its state,
   metering, and effects reproduce the Realm contract.

Backend admission requires differential conformance against the reference
machine:

1. boot the same minimal kernel/userland and replay a fixed command corpus;
2. compare exit status, stdout/stderr bytes, filesystem diff, device-request
   sequence, halt reason, and policy denials;
3. prove malformed DMA ranges, forged completions, output exhaustion, step
   exhaustion, revocation, and cancellation fail closed;
4. record where instruction counters differ and retain an outer work unit that is
   stable enough for principal policy;
5. run compiler and repository workloads only after semantic cases pass.

Compiler storage must also change. Immutable distro and toolchain bytes belong in
a content-addressed block image or snapshot, not the initramfs and not thousands
of individual principal KV values. A build gets three distinct stores:

```text
/usr and /nix/store-like package paths   immutable signed system generation
/home/agent                              principal-durable config and caches
/workspace                               invocation-admitted COW work tree
```

The first development implementation may materialize the admitted workspace into
a block-local staging generation, run the build there, and return a typed diff for
outer promotion. `cwd://` remains the authority and source identity; staging does
not grant access to a physical host path or silently commit changes. This avoids
making compiler performance depend on one audited 9P round trip for every small
metadata operation while preserving the user's ability to inspect and promote the
Linux-produced artifact.

The first Rust acceptance proof is deliberately difficult to fake:

1. status names the accelerated backend, exact distro generation, exact Rust and
   Cargo versions, compiler digest, and admitted memory/CPU/storage budgets;
2. the Realm creates `Cargo.toml` and `src/main.rs` under its mounted
   `/workspace`, without a host-side compiler preparing the result;
3. `cargo build --release` executes inside the RV64 Linux guest;
4. `file target/release/<name>` reports a RISC-V 64-bit Linux ELF, not Mach-O and
   not a host cross-build;
5. the same guest executes that exact ELF and returns the expected output;
6. the outer workspace exposes the artifact through its typed diff/promotion
   flow; and
7. the receipt binds principal, source generation, distro, toolchain, dependency
   set, backend, budgets, output digest, exit status, and promotion decision.

Until all seven pass, the accurate claim is “Linux Realm with a shell and mounted
workspace,” not “Rust development environment.”

The second slice is pinned to the January 2026 ratified
[RISC-V privileged release](https://docs.riscv.org/reference/isa/v20260120/priv/priv-index.html):
Machine and Supervisor ISA 1.13 plus
[Zicsr 2.0](https://docs.riscv.org/reference/isa/v20260120/unpriv/zicsr.html).
It adds typed M/S CSR state and access checks, all six CSR read/modify/write
operations with their exact read/write-intent rules, WARL handling for the
implemented fields, M/S/U privilege state, ECALL exception selection and
delegation, direct trap-vector entry, and `mret`/`sret`. The admitted
`rv64-supervisor` program enters S-mode from reset, prints `S`, takes a delegated
S-mode ECALL, prints `T` in the S-mode handler, advances `sepc`, returns to print
`R\n`, and halts from S-mode. It consumes 31 bounded execution steps and retires
30 instructions because ECALL is architecturally non-retiring.

The third slice makes the protection boundary Linux-shaped without claiming a
Linux boot. Instruction, illegal, breakpoint, load, store, alignment, access, and
page faults now enter the architecturally selected M/S vector and remain
non-retiring but slice-charged. `satp` admits Bare and Sv39. Its bounded three-level
walker rejects non-canonical addresses, invalid/reserved PTE encodings and
misaligned superpages; enforces U/S, R/W/X, SUM, and MXR; translates MPRV data
accesses; and updates A/D bits in admitted RAM. `sfence.vma` is privilege checked
and retires as a no-op because the interpreter has no software TLB.

The fourth slice supplies the single-hart execution and time substrate. `misa`
reports RV64IMA plus S/U; all base M multiply/divide operations and W forms, LR/SC,
and the W/D AMO set execute with architectural edge behavior. Reservations are
physical, cleared by overlapping stores and traps, and never weaken the outer
single-threaded machine boundary. Architectural cycle/time/instret counters are
separate from unforgeable Realm fuel accounting, and `mcounteren`/`scounteren`
gate lower-privilege reads.

The deterministic CLINT exposes `msip`, `mtimecmp`, and `mtime`. Guest time advances
once per charged machine step rather than from ambient host wall time. M/S
interrupt CSRs, delegation, global enables, direct/vectored entry, and bounded
`wfi` are live; machine and supervisor timer-interrupt paths have executable tests.

The fifth slice defines the boot firmware contract. `Machine::boot_linux` admits
a raw RV64 `Image` at the standard 2 MiB boundary, page-aligns the initramfs after
the image, writes a deterministic FDT at `0x80001000`, and enters S-mode with
`a0=0`, `a1=FDT`, and address translation disabled. The generated
`aos-rv64-virt-v0` tree describes the exact CPU ISA, Sv39, admitted RAM, timebase,
CPU interrupt controller, UART, boot arguments, and initramfs range without
invoking `dtc` at build or runtime. Its bytes have also been independently parsed
by DTC 1.8.1.

The private firmware implements the SBI 3.0 Base, TIME, DBCN, and SRST subsets,
plus one AOS experimental extension for bounded host requests.
Debug-console buffers are accepted only when their complete physical range lies
in admitted RAM; timer requests map the machine timer into the delegated
Supervisor timer line; a successful reset halts the bounded machine. The
`0x414f5300` implementation value is a private, unregistered profile value, not a
claim to an assigned RISC-V SBI implementation ID.

The first host request uses experimental EID `0x08414f53` for two 9P transports:
channel 1 selects the principal's durable Realm-home generation and channel 2
resolves the current invocation workspace. Linux supplies one complete request
and response buffer;
the machine validates both full physical ranges, copies the request into an owned
Rust value, and returns a typed suspension to the Realm. No more guest steps are
possible while that request is pending. The initiating ECALL is charged as one
non-retiring machine step, the bounded Rust service completes or fails the exact
request ID, and only then may Linux resume.

The pinned Linux longterm 6.18.39 image now reaches the AOS-controlled `/init`
inside a 32 MiB machine and remains alive in a framed console command loop. Its
source archive matches kernel.org SHA-256
`a7a7e3d2ae9d95e74197223a8d4eb5f6be7aac21b6e6de27e9685d001c1f8cb0`;
the Buildroot 2026.05.1 rootfs is
`10d26184e85add731208050fb3da9fed5e1dda7475b6e66e0d9814a221ecf3f4`,
and the deterministic raw `Image` is
`fd394b7e5b09638d52483fe2f417985ae1af6a730eea5bc3e415b97262f863de`.
The checked-in capsule regression asserts the exact kernel and userland
versions, AOS machine model, PID 1 launch and ready markers, diagnostic state
continuity, real UID-1000 BusyBox shell execution, warm file continuity,
nonzero exit propagation, forged-frame rejection, background descendant cleanup,
real Linux create/write/rename/read/readdir through both 9P filesystems, clean
unmount/remount across calls, the exact five-device surface and unreadable PID-1
console descriptors, clean SBI shutdown, RAM release, cold restart, durable-home
readback, and reset boot-local state. Two independent Buildroot trees produced
the same rootfs digest and two independent kernel output trees produced the same
Image digest.

That regression drives the real Linux 9P client, SBI transport, machine boundary,
and Rust server against separate native temporary home/workspace exports so it
can run without inventing an Astrid invocation context. The production home
adapter's CAS filesystem and typed error/metadata mapping are separately tested;
both production adapters are deny-warnings checked and compiled into the
`wasm32-unknown-unknown` capsule. An installed Astrid `>=0.10.2` run is still
required before claiming an end-to-end production `cwd://` or principal-home
mount.

The earlier cold-boot artifact ran through AOS Runtime 0.10.1 as an installed
`wasm32-unknown-unknown` component and established the live tool path. The
resident artifact now requires the principal-affine Store contract in Astrid
`>=0.10.2`; it must not be represented as compatible with an unmodified 0.10.1
runtime. The Realm performs a kernel-recognized cooperative IPC yield between
each 100,000-step RV64 slice. The 32 MiB guest limit is deliberately below
Astrid's independently enforced 64 MiB component linear-memory ceiling, leaving
room for the embedded image, interpreter, stack, and capsule state.

This is now the first useful agent-workbench slice: static musl 1.2.6, BusyBox
1.38.0, `ash`, `/proc`, `/sys`, and a UID/GID-1000 agent home execute inside the
measured RV64 guest. It is not Bash, Debian, or the completed distribution. It
has no Python, in-guest compiler, package manager, network, PTY, or durable disk.
It does mount the current invocation's Astrid COW workspace through synchronous
9P before each shell call. Its static `/dev` contract is only console, null, zero,
random, and urandom: there are no raw-memory, block, network, graphics,
additional TTY, or PTY nodes. The kernel disables legacy `TIOCSTI`, both PTY
families, IP/Unix networking, network devices, raw memory/port devices, input,
and media support. `CONFIG_NET` remains only because Linux's 9P core depends on
it. Linux therefore also selects its internal classic-BPF and RX-busy-poll
helpers, but the `bpf(2)` syscall, packet/IP/Unix families, network devices,
ethtool netlink surface, and every non-AOS 9P transport remain disabled.
A PLIC, compressed instructions, and virtio block also remain absent and will be
added only when an admitted device requires them.

### 2.5 Privileged-machine component sketch and invariant ledger

The implementation is deliberately decomposed by authority rather than by Linux
subsystem names:

```text
Realm adapter (program admission, outer fuel/output ceilings)
  -> Machine::run_slice (bounded scheduling and accounting)
     -> instruction decode/commit (RV64IMA + Zicsr + xRET + sfence.vma + wfi)
        -> Cpu (integer registers, PC, current privilege)
        -> CsrFile (typed implemented CSR set and WARL masks)
        -> exception entry (delegation and xstatus stack update)
        -> Sv39 walk and permission check (virtual to admitted physical address)
        -> SbiFirmware (Base/TIME/DBCN/SRST plus bounded AOS host requests)
        -> Devices (admitted RAM, deterministic CLINT, UART, test finisher)
  -> boot_linux (image/initramfs admission, generated FDT, exact S-mode handoff)
```

Exception entry is a private machine operation rather than a pluggable object:
trap routing is architectural CPU behavior, not a device or agent policy. Sv39
belongs between decoded virtual memory accesses and `Devices`; CLINT/PLIC belong
inside `Devices` but only assert typed pending-interrupt lines into the CPU. The
outer Realm remains responsible for time/fuel admission and must never appear as
an implicit RISC-V device.

| Case | Required invariant | Current executable evidence |
|---|---|---|
| Reset | PC is `0x80000000`, registers/CSRs clear to the profile reset state, privilege is M | image reload/reset tests and both probes |
| CSR address | Bits 9:8 impose minimum privilege; unsupported/reserved addresses fail before mutation | M CSR write attempted from S traps without retirement |
| CSR write intent | CSRRW[I] writes even with zero source; CSRRS/CSRRC[I] with zero source do not write and may read a read-only CSR | all six operations checked against old values; `csrr mhartid` succeeds while a write traps |
| WARL | Only implemented fields persist; reserved MPP is coerced; `satp` admits only Bare/Sv39; interrupt enables/pending remain zero | typed CSR reads, reserved-MPP regression, Sv39 setup tests |
| ECALL | Cause is selected from the originating privilege; EPC points at ECALL; ECALL does not retire | S probe records `scause=9`, exact `sepc`, 31 steps/30 retired |
| Delegation | Only traps originating below M consult `medeleg`; implemented synchronous causes can be delegated | S ECALL and load-page-fault cases enter `stvec`; M ECALL remains in M under repeated bounded delivery |
| S trap entry | `SPP=origin`, `SPIE=SIE`, `SIE=0`; M trap state is untouched | midpoint assertions in the S probe |
| M trap entry | `MPP=origin`, `MPIE=MIE`, `MIE=0`; S trap state is untouched | bounded M ECALL loop assertions |
| xRET | Target alignment is checked before commit; xIE/xPIE and xPP are popped exactly; execution below x privilege fails closed | full MRET/SRET path plus U/S privilege-rejection regressions |
| Trap vector | Direct and vectored encodings are admitted, reserved modes coerce to Direct; synchronous exceptions use BASE | S probe direct-vector address assertion |
| Scheduling | Every interpreted attempt, including a non-retiring architectural trap, spends one slice unit | self-vectoring ECALL and fault cases yield exactly at their budgets |
| Synchronous faults | Faulting instructions do not partially commit; cause/EPC/tval enter the selected architectural vector | illegal CSR, breakpoint, bad jump, misaligned store, and delegated page-fault regressions |
| Sv39 | Walks are bounded to three levels; canonicality, leaf/superpage form, U/S and R/W/X permissions, SUM/MXR, MPRV, and A/D updates are exact | 4 KiB R/W/X, execute-only, 2 MiB superpage, non-canonical, and privilege-matrix regressions |
| Translation fence | `sfence.vma` is illegal in U and retires in S/M; no cached translation survives because this implementation has no TLB | S retirement and U illegal-instruction regression |
| RV64M | Signed/unsigned high products, divide-by-zero, signed overflow, remainder, and W sign extension follow the ISA | decode-driven operation matrix for all M funct3 values and W forms |
| RV64A | LR/SC uses a physical single-hart reservation; overlapping writes and traps invalidate it; W/D AMOs return the old value exactly | successful and failed LR/SC plus AMOSWAP/AMOADD regressions |
| Counters | Architectural counters are deterministic and guest-writable machine counters cannot rewrite outer Realm accounting | cycle/time/instret reads and M/S/U counter-enable matrix |
| Interrupts | Pending/enable/delegation/global-enable selection precedes fetch; entry preserves the interrupted PC and vectors only interrupts | CLINT M-timer vectored entry and delegated S-timer/WFI regressions |
| Linux placement | Kernel, initramfs, and FDT ranges are checked before mutation; Linux receives the standard RV64 register handoff in S-mode | exact RAM-byte, FDT-header, layout, register, CSR, and privilege assertions |
| SBI boundary | Firmware sees only admitted RAM, deterministic time, bounded console/reset devices, and typed host requests; unsupported extensions fail closed | Base 3.0 probe, DBCN write, TIME interrupt, SRST halt, invalid-address regressions, and pending-request exclusion |
| Workspace portal | One complete 9P request is copied from validated guest RAM, served only against the invocation's `cwd://` capability, and completed into the exact admitted response range | Linux creates, writes, renames, lists and rereads a file, then remounts and rereads it in the next invocation |

The table is also the review boundary: a useful static shell and mounted
invocation workspace do not imply a durable distribution. The next storage
increment connects the principal Realm home and a durable root overlay without
weakening the already narrower workspace capability.

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

The executable seed uses named imports under `aos_realm_v0`. Its process subset is
now precise rather than POSIX-shaped by assertion:

| Seed import | Current contract | Deliberate limit |
|---|---|---|
| `arg-count()`, `arg-len/read(index, ...)` | Read a bounded argv vector without a C layout dependency | At most 64 entries and 32 KiB combined UTF-8; no NUL |
| `env-count()`, `env-len/read(index, ...)` | Read canonical, unique `KEY=VALUE` entries | At most 64 entries and 32 KiB combined UTF-8; identifier-shaped keys only |
| `open(path, mode)`, `read/write(fd, ...)`, `close(fd)` | Use one kernel-allocated descriptor table for host-backed files and realm pipes | File descriptions remain process-store-local and cannot yet participate in child descriptor actions |
| `pipe(capacity, ends_ptr)` | Create one quota-charged bounded pipe and write exact read/write descriptors | No flags or socket pairs |
| `spawn-signed(program, arg, source_fd, target_fd, handle_ptr)` | Compatibility scalar form for the first guest-created pipeline | One argument and at most one mapping; new code uses the record form |
| `spawn-signed-record(record_ptr, 44)` | Resolve one absolute build-generated immutable-catalog path; copy bounded argv/environment vectors; apply up to 16 exact descriptor actions; return a generation-checked handle | No module bytes, host PATH lookup, implicit inheritance, file actions, or `fork` |
| `wait(handle, status_ptr)` | Park until one direct child terminates, reap it, and write an explicit exited/signalled record | No `waitpid(-1)`, groups, or nonblocking mode |
| `signal(handle, signal)` | Terminate one direct child with a typed Realm signal | No host signal and no cross-job PID targeting |

The version-1 signed-spawn record is eleven explicit little-endian `i32` fields:
`version`, zero `flags`, executable pointer/length, argv-table pointer/count,
environment-table pointer/count, action-table pointer/count, and output-handle
pointer. Each string-table entry is `{pointer, length}`. Each action is
`{kind, source, target}`. `dup` copies one named parent pipe endpoint into one
child descriptor. `close-parent` requires `target = -1` and releases the named
parent endpoint in the same semantic-kernel transition that retains the child's
copy. All tables, UTF-8, counts, aggregate bytes, duplicate targets, duplicate
closes, catalog paths, environment keys, descriptors, and the handle range are
validated before a child PID is allocated.

The admission cases are intentionally closed rather than inferred:

| Case | Required result |
|---|---|
| Known path, valid vectors, no actions | One prepared child; parent retains its descriptors |
| Known path with several distinct `dup` targets | Child receives only those exact endpoints |
| `dup(source, target)` followed by `close-parent(source)` | Child retains one endpoint reference and the parent loses its copy atomically |
| Unknown catalog path or caller-supplied module bytes | Reject before instantiation and PID allocation |
| Wrong record size/version, nonzero flags, negative/overflowing/out-of-bounds range | Stable host fault; no child and no partial descriptor mutation |
| Empty argv, too many entries/actions, aggregate byte overflow, invalid UTF-8/NUL | Reject before PID allocation |
| Malformed or duplicate environment key | Reject before PID allocation; no last-value-wins ambiguity |
| Missing action source, duplicate child target, duplicate parent close, negative descriptor | Reject the entire action table; retain the parent's table |
| Action selects a host-backed file descriptor | Reject before PID allocation; shared offset and last-close semantics are not guessed |
| Catalog module fails validation or store admission | Reject before PID allocation |
| Child quota exhausted | Reject while preserving every live endpoint and existing process |
| Spawn succeeds but the foreground tree later faults, traps, exhausts fuel, or is cancelled | Terminate descendants, close all endpoints, retain exact accounting, and reap the tree before returning |
| Transport repeats the same principal/call-ID/arguments | Replay the recorded result without constructing another tree |

The current host tests exercise record-version, pointer-range, vector-count,
environment-key, duplicate-target, missing-close-source, child-budget, signal,
fuel/output, and final cleanup paths. Core tests separately check that invalid
inherit/close transactions leave the PID sequence, parent descriptor table, pipe
counts, and endpoint reference counts unchanged.

The immutable executable table is generated from the validated
`guests/catalog.tsv` build manifest. Absolute paths, guest source directories,
output names, and Rust byte symbols must be unique and syntactically bounded;
invalid metadata fails the capsule build rather than changing runtime lookup.

Process handles are explicit little-endian records containing the actor boot
generation and the monotonic Realm PID. Both fields are checked before `wait` or
`signal`; a bare PID is never sufficient across actor restart. File and pipe
descriptors now share one kernel-owned allocation space. The runtime maps the
kernel's typed `FileDescriptionId` to the actual Astrid-backed file object; that
object is still process-store-local, so sharing it across child stores remains a
separate, explicit design step.

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

The executable seed uses that shape through two different consumers. Their
workspace plumbing differs, while the home object is shared:

- nested core-WASM programs see the principal-scoped versioned `/home/agent`, the
  invocation's Astrid `cwd://` copy-on-write `/workspace`, and the principal's
  ephemeral `/tmp` through audited host imports;
- Linux sees the same selected principal-home generation at `/home/agent` and
  the current invocation's Astrid `cwd://` COW workspace at `/workspace` through
  separate bounded 9P channels. Its `/tmp` and root filesystem remain warm-RAM
  state.

That distinction is intentional. A command running inside the realm cannot silently
commit source-tree changes merely because it can write its projected workspace.
Promotion is an outer authority decision and must produce an audit record. The
current seed does not yet expose a realm-side commit tool. Linux accepts
`/workspace` and normalized descendants as CWDs only because the file transport
now mounts that exact capability. `/home/agent` names the same durable Realm home
for both consumers; the execution receipt carries the generation selected before
and after the command.

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

### 6.1 Path identity and translation contract

Path strings are presentation, not authority. The system needs a typed reference
that survives translation between a person's computer, an Astrid resource, and a
Realm program without pretending those namespaces are identical.

The first internal contract is:

```text
PathRef {
  mount: Workspace | AgentHome | Temporary
  mount_id: Optional<String>
  relative_path: String
  guest_path: String
  resource_uri: Optional<String>
  display_path: String
  reference_stability: Invocation | PrincipalGeneration | RealmBoot
  generation_at_admission: Optional<u64>
  realm_boot_sequence_at_admission: Optional<u64>
}

MountContext {
  version: 2
  consumer: NestedCoreWasm | LinuxGuest | BareRv64
  active_cwd: Optional<PathRef>
  mounts: [MountDescriptor]
  physical_host_paths_visible: false
}
```

The execution envelope supplies the kernel-stamped owner principal. The semantic
identity is therefore the owner, Realm, mount ID, relative path, stability scope,
and home generation or Realm boot sequence where applicable. `guest_path`, `resource_uri`, and
`display_path` are derived views of that identity. They are never used to infer
the owner.

The current projection matrix is deliberately candid:

| Role | Person display | Guest spelling | Astrid resource | Nested WASM | Linux now | Stability |
| --- | --- | --- | --- | --- | --- | --- |
| Workspace | `Workspace/src/lib.rs` | `/workspace/src/lib.rs` | `cwd://src/lib.rs` | mounted COW | mounted COW during invocation | invocation |
| Agent home | `Agent Home/.config/aos` | `/home/agent/.config/aos` | principal Realm home URI | mounted durable generation | mounted durable generation | principal generation |
| Temporary | `Temporary Files/job.log` | `/tmp/job.log` | principal Realm temp URI | mounted ephemeral | guest RAM only | Realm boot |

The identical `/home/agent` spelling in the two consumers names the same
principal-scoped storage object. Every response still names its consumer and
projection because access mechanics differ. A Linux workspace path carries its
real `cwd://` resource URI but no durable generation; a Linux home path carries
the Realm-home URI and admitted generation; a temporary path has no Linux Astrid
resource URI. Bare RV64 diagnostic programs have no active filesystem path.

The Realm home ID is stable only inside the verified owner and Realm. The current
`cwd://` import exposes neither a stable workspace attachment ID nor a generation,
so workspace `mount_id` is null and its reference expires with the invocation.
Inventing an ID from the host pathname, display label, or CWD bytes would create
incorrect links and leak implementation details. The runtime should eventually
supply a random, opaque attachment ID and epoch when it admits a workspace.

Translation is a boundary protocol:

```text
person selects a local folder
  -> client/runtime creates an admitted workspace attachment
  -> attachment yields opaque mount identity plus relative path
  -> conversation renders Workspace/<relative>
  -> Realm command receives /workspace/<relative>
  -> consumer adapter verifies that projection is mounted
  -> result/receipt returns typed PathRef plus both audience spellings
```

A physical client path such as `/Users/alice/code/astrid` may be visible to the
person's client. It is never a guest argument, resource URI, mount ID, or status
field. If the client has not attached it, the path is unresolved. The agent must
not guess that a pathname mentioned in prose is the current workspace. Conversely,
when an agent says `/workspace/src/lib.rs`, the person-facing UI should lead with
`Workspace/src/lib.rs` and offer the guest spelling as copyable technical detail.

The ambiguity rules are:

| Case | Required behavior |
| --- | --- |
| Person names an attached local child | Translate through the attachment and retain the typed reference |
| Person names an unattached physical path | Ask the client to attach/select it or fail explicitly |
| Agent names `/workspace/x` | Resolve only against this invocation's workspace attachment |
| Agent names `/home/agent/x` | Resolve against the verified principal's selected Realm-home generation for either mounted consumer |
| Two mounts contain `src/lib.rs` | Display the mount name; never return only the ambiguous relative path |
| An invocation-scoped reference is reused later | Reject it unless the runtime rebinds the same attachment explicitly |
| A durable-home generation changed | Treat the captured generation as admission evidence, not an assertion about the latest head |
| A path escapes every admitted root, uses an unmounted root, or has a physical host prefix | Reject before any storage operation |
| A symlink would escape a mount | Resolve beneath the mount in the transport and reject the escape |
| Principal B replays principal A's reference | Reject at the stamped-principal and mount-ownership boundary |
| Linux requests a workspace descendant that does not exist after mount | Let guest `chdir` fail; do not redirect it to home or create it implicitly |

The tool response remains additive: `requested_cwd` preserves what the caller
asked for, `cwd` reports the effective guest CWD, and `path_context` is the
authoritative interpretation. Status reports both nested-WASM and Linux
projections plus a consumer-indexed CWD table: nested WASM defaults to
`/workspace`, Linux defaults to `/home/agent`, and bare RV64 has no CWD. The old
single `default_cwd` remains the nested-WASM value for compatibility. A future
tool input may accept a typed `PathRef`, but the contract should remain
capsule-private until at least two consumers require a shared WIT surface.

The selected transport is synchronous 9P2000.L over the private bounded SBI host
request. PID 1 unmounts and remounts `/home/agent` and `/workspace` before every
shell call. The home remount observes the currently selected durable head. The
workspace rule is necessary because `cwd://` still exposes neither a stable
attachment ID nor an epoch: all prior workspace FIDs are discarded before a new
invocation can bind its workspace. The remaining identity work is to have the
runtime supply an opaque attachment ID and epoch, carry it through typed receipts
and audit, and reject a stale reference before entering Linux. Transport choice
does not change the path identity or Astrid's outer promotion requirement.

### 6.2 What is the driver?

The Linux storage driver is a protocol stack, not one privileged binary:

```text
Linux VFS and 9P client
  -> GPL-2.0-only trans=aos kernel transport
  -> experimental SBI request in admitted guest RAM
  -> MIT/Apache AOS machine host-request boundary
  -> bounded Rust 9P server
  -> channel 1: CAS-selected principal-home adapter
     channel 2: Astrid cwd:// adapter
  -> invocation-scoped COW workspace capability
```

The in-kernel transport is the guest driver in the conventional Linux sense. It
knows 9P request buffers and SBI, but has no Astrid credentials, physical path,
network socket, shared ring, DMA, or direct host handle. The Rust 9P server is the
device model: it defines bounded filesystem behavior and rejects unsupported or
escaping operations. The final adapter is the authority driver: it can exercise
only the kernel-stamped invocation's `cwd://` capability.

One mount admits at most 64 KiB per message, 1,024 live FIDs, 4,096 materialized
directory entries, and 16,384 retained QID path identities. Exhaustion returns a
guest-visible error instead of growing the capsule without bound.

This is the pattern for later devices. A small guest-facing mechanism speaks a
versioned protocol; a capsule or kernel service implements policy-free device
semantics; Astrid binds the request to an explicit capability and meters it. A
WASM driver may implement a filesystem, network, display, audio, or accelerator
service, but it does not receive arbitrary MMIO or DMA merely by being called a
driver. Physical hardware still needs a protected native mechanism beneath that
service. For a GPU, the safe early portal is a validated command/resource API,
not exposing the physical GPU driver ABI to an agent capsule.

The C transport is GPL-2.0-only because it is compiled into Linux. Linux and
BusyBox remain GPL works with corresponding-source obligations. The Rust machine,
9P server, capsule, and Astrid runtime remain MIT/Apache across the SBI protocol
boundary; none is linked into the kernel.

### 6.3 Selected seed representation

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

Before first format-2 execution, the dedicated format-0/1 direct-home tree is
enumerated in stable order and nodes absent from the selected generation are
imported within explicit entry and per-file bounds. Format-1 manifests
materialize their formerly implicit parent directories and upgrade on the next
mutation. The direct path is maintained as a best-effort compatibility
projection, never as the selected truth. This preserves deployed seed data
without requiring a separate stop-the-world migration window.

The present semantic boundary is intentionally bounded: regular-file create,
read, positional write, truncate and unlink; directory create, stable readdir and
empty removal; atomic file replacement and directory-tree rename; persisted
permission bits; a 64 KiB per-file limit; and a 1 MiB manifest. Successful
mutations select and verify a complete generation before returning, so guest
`fsync` has no deferred dirty state. Links, timestamps, named checkpoints,
diff/reset, garbage collection, and quota-backed `statfs` capacity remain absent
rather than implied POSIX behavior.

### 6.4 Base and overlay

- The base image is immutable, signed, content-addressed, and globally cacheable.
- The overlay contains only blocks or files changed by the principal.
- The durable home can be a separate volume so it can migrate between base images.
- `/tmp`, pipes, process tables, and transient logs do not enter durable storage.
- Guest `fsync`, atomic rename, stable directory ordering, cold-boot home
  readback, and generation crash consistency have executable coverage. Full root
  overlay crash recovery remains separate work.

### 6.5 Persistence levels

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

Files and pipes use the same first-free descriptor allocator in `realm-core`.
Opening a file installs `File(FileDescriptionId)` in the running process table;
the Wasmi store retains only the backing `RealmFile`. Read, write, and close first
resolve the kernel resource kind, so the outer backend cannot independently pick
or collide descriptor numbers. File inheritance and atomic child-close actions
are rejected before PID allocation until a realm-wide open-file-description table
defines shared offsets, reference counts, and final-close commit behavior.

Each pipe has an immutable positive capacity and contributes that capacity to an
aggregate realm quota. Writes may be partial up to available capacity. A full pipe
returns `WouldBlock`; a read frees capacity and wakes parked writers. An empty pipe
returns `WouldBlock` while any writer exists, and `EOF` only after the final writer
closes. Closing the final reader wakes writers, whose next write returns
`BrokenPipe`. Termination closes every descriptor owned by that process. A pipe is
removed and its reserved capacity released only after its last read and write
endpoint close.

The capsule is a principal-affine service. Astrid owns a bounded map of resident
Wasmtime Stores keyed by verified principal; each Store contains one
`RealmMachine` and semantic kernel. It keeps process identifiers monotonic across
tool calls during that Store residency, reaps completed foreground jobs, and
exposes live process, pipe, reserved-byte, command, and next-PID accounting. The
capsule repeats the owner binding internally and rejects a principal change even
if the runtime invariant regresses. Constructing a new process table inside each
one-shot tool call is no longer the live path.

There are now four executable topology proofs. `pipe-echo` has the outer Realm
runtime choose and connect two signed processes. `guest-pipe-echo` instead launches
one signed supervisor guest; that guest calls `pipe`, starts the consumer and
producer through `spawn-signed`, closes its pipe copies, blocks in `wait`, checks
both terminal records, and exits. The scheduler dynamically admits the prepared
child stores when the spawn host call yields. A signalled blocked child is cancelled
without resuming its abandoned Wasmi continuation, but still receives an exact
accounting record and is waitable by its parent.

`realm-sh` is the record-ABI proof. The outer capsule passes separate structured
tokens; the nested shell, not the host, recognizes `echo TEXT`, `env KEY=VALUE`,
`echo TEXT | cat`, or `echo TEXT > PATH`. It maps those names to `/bin/echo`, `/usr/bin/env`, and
`/bin/cat`, encodes argv/environment/action tables in its own memory, spawns the
signed processes, atomically transfers the required pipe endpoints, and waits for
the foreground job. Redirection opens the file in the shell process and pumps a
signed echo child's bounded pipe into it; it does not pretend file descriptors are
shareable yet. A single text argument such as `"echo x | cat"` is not retokenized.
The generated catalog has three exact paths and performs no host PATH search.

The caller's fuel and captured-output budgets are partitioned before execution
across the root and its maximum two descendants. The process quota is reserved
before the root runs; a third spawn fails closed. Every process has an independent
memory ceiling, and the combined result reports aggregate fuel, output, memory
ceilings, suspensions, and all PIDs. Success, invalid generation, child-budget
exhaustion, signal/reap, partial construction, file/pipe descriptor collision, and
final zero-resource cleanup are host-tested. The runtime serializes calls that
target one principal Store while allowing different principals' Stores to run
concurrently. Background jobs still require an explicit job lifecycle rather
than being smuggled through residency.

A shell command is foreground by default. PID 1 waits for its direct child,
kills and reaps every remaining descendant, and closes the command boundary
before returning. Shell syntax such as `command &` therefore cannot create
durable work. An agent that intends continued execution must say so through a
separate Realm job operation with an explicit completion policy, execution
budget, cancellation authority, and output sink. The minimum future record is:

```text
BackgroundJob {
  handle
  owner_principal
  realm_boot_sequence
  command_or_capsule
  lifecycle: UntilComplete | UntilDeadline | UntilCancelled | WakeOnEvent
  cpu_and_io_budget
  started_at
  last_observed_state
  output_stream_or_artifact
}
```

The handle, rather than a guest PID, crosses tool calls. A frozen resident VM
is not a background job: it consumes no guest steps and makes no progress while
no admitted invocation is driving it. Once background jobs exist, the Astrid
scheduler must explicitly wake and meter them to the same principal, and idle
eviction must refuse or deliberately cancel them according to their recorded
lifecycle. The current implementation intentionally supports foreground work
only.

There are presently three independent foreground clocks: the MCP shim's broker
round-trip deadline, the broker capsule's result-drain deadline, and Astrid's
principal invocation timeout. The first two are transport policy; neither turns
a running Realm command into a background job. If either transport clock closes
without publishing cancellation, the original invocation remains foreground in
the runtime until it returns or the outer principal timeout interrupts it. The
safe temporary configuration is therefore:

```text
broker result drain < principal invocation timeout < MCP shim request timeout
```

This ordering bounds any uncancelled tail and preserves the broker's terminal
reply. It is not the final contract. Proper cancellation requires a
correlation-preserving cancel path from MCP request token through broker call ID
to the exact target invocation; proper continued work requires the explicit job
record above.

The earlier manual actor could see the broker call ID and held a bounded replay
window. The SDK's direct tool method currently receives only deserialized tool
arguments, so that cache is not present in the principal-affine path. This is an
explicit regression in delivery semantics, not an excuse to claim duplicate
mutation safety. The correction belongs in SDK/runtime dispatch: expose a
verified invocation context or implement durable mutation receipts before
dispatch. Until then, a lost mutating response is indeterminate and a caller must
inspect durable state before retrying.

Astrid's subscription host returns at most one routed message per envelope and
installs that message's principal as the invocation context before Realm KV, file,
and publish calls. The actor additionally requires `verified` attribution rather
than trusting a principal string in the payload.

Process identity is the tuple `(realm boot sequence, process id)`. The process ID
returns to 1 after Store eviction or daemon restart, while a principal-scoped boot
sequence is advanced atomically with KV compare-and-swap. Read-only status binds
the Store owner but does not allocate a semantic machine or advance that sequence.
Aggregate admission and idle LRU eviction are runtime responsibilities; the guest
does not recreate a hidden multi-principal map.

### 7.1 Linux residency and principal accounting

There are two different concurrency axes and they must not be collapsed:

```text
across principals:  Astrid admits, isolates, meters, schedules, and evicts realms
inside one realm:   the RV64 machine deterministically schedules one or more harts
```

Virtual SMP does not make a shared machine multi-tenant. A principal owns a Realm;
harts are CPUs inside that Realm. The principal boundary remains outside the
machine and every CPU, memory, storage, network, and device charge rolls up to
that owner.

The current Linux adapter is lazy and principal-resident. The affined Store owns
one optional single-hart machine with a proven 32 MiB default and an explicitly
configured 32–256 MiB guest-RAM envelope. `linux-boot`, `linux-console`, or
`linux-sh` creates it when cold, advances Linux in bounded 100,000-step slices
until the controlled `/init` reports ready, and retains its CPU and RAM for later
invocations. A `counter` probe reaches 1 and then 2 across separate calls, and a
file written by UID 1000 under `/home/agent` is readable by a later shell call.
No CPU runs while the Store is idle: every guest step occurs inside an admitted
outer tool invocation and is charged to the verified principal.

Resource admission is a strict intersection rather than an ambient host query:

```text
Astrid administrator's principal profile
∩ capsule hard maximum
∩ invoking principal's per-capsule Realm configuration
∩ optional lower per-command request
```

The outer principal profile remains authoritative for Wasmtime memory, CPU rate,
storage, processes, and timeout. The inner configuration currently selects guest
RAM, interpreted steps, captured output, and an optional per-file ceiling. Zero
step or file ceilings delegate only those dimensions to the outer CPU/timeout or
storage policy. The envelope is read for every invocation and never cached across principals. A
changed envelope destroys only that
principal's warm RAM before the next execution; durable home and workspace
authority are reattached, and another principal's Store is untouched. Status is
read-only and reports configured versus active values plus a pending-change bit.

The capsule cannot yet read the exact enclosing Store ceiling through the frozen
host ABI, so it does not pretend to derive RAM automatically from the physical
machine or consume the maximum. A future versioned host resource-admission query
may turn `auto` into a concrete audited allocation. Until then, the administrator
sets the outer principal quota and the Realm's guest-RAM request separately; an
undersized outer quota fails closed at the existing Wasmtime limiter.

Every non-boot command uses a fresh 128-bit host-CSPRNG token. PID 1 disables
console echo and emits token-bound begin/end frames; the host accepts the final
matching status only before the next ready marker. The shell never receives the
token, reads stdin from `/dev/null`, receives separately reopened write-only
stdout/stderr, cannot reopen the root-only console, runs with `no_new_privs` and
resource limits, and has all surviving descendants killed and reaped before the
result frame. A forged marker and delayed background writer are executable
regressions, not assumptions.

`linux-shutdown` asks PID 1 to power down through SRST and releases the machine;
stopping an already-cold Realm is idempotent. A guest trap, fuel exhaustion,
output-limit failure, or cooperative-host failure also discards uncertain RAM.
Runtime idle eviction, unload, and daemon restart destroy the Store and therefore
the VM. Principal Realm home state is attached to Linux over channel 1 and remains
durable across those boundaries. The invocation workspace is attached
independently on every shell call and retains Astrid's COW/promotion semantics.
`linux_realm_status` exposes home and workspace as mounted, temporary storage as
guest-RAM-only, `linux_home_persistent=true`, and
`linux_rootfs_persistent=false` rather than calling the whole VM durable.

The missing runtime primitive was implemented from freshly fetched Astrid Runtime
0.10.1 upstream `main` commit
`4771bab3c33d1bce53186e40d01cf014e2dce666`, on branch checkpoint
`a7c2358e848cdc041c04f7a34c20ae653143597f`. Runtime 0.10.1 itself still has the
two old Store models below; the experimental branch adds a third:

| Store model | Principal accounting | Residency |
| --- | --- | --- |
| Pooled interceptor | Invoking principal receives the fuel charge, CPU-rate admission, memory ceiling, VFS/KV/secrets, and cancellation context | No principal affinity; a later call may lease another clean Store |
| `run()` singleton | Message principal receives VFS/KV/secrets/profile/cancellation context and stamps follow-up publishes | One persistent Store whose outer CPU and linear memory remain bounded and attributed to its load-time owner |
| Principal-affine service | Invoking principal receives fuel/rate charging plus exact cross-capsule current resident-memory accounting | One evictable Store permanently bound to that principal; same-principal calls serialize, unrelated principals can run concurrently |

The capsule opts into the third model with fail-closed package metadata and
requires Astrid `>=0.10.2` so a 0.10.1 runtime cannot silently ignore the
contract. The implementation rejects `run()`, `host_process`, shared guest
memory, missing stamped principals, typoed metadata, and uplink-daemon semantics.
It destroys over-quota Stores when a memory limit is lowered, evicts only idle
least-recently-used Stores, and releases exact current memory on eviction or
unload. Residency remains an evictable cache; durable state must cross KV/home
before a call returns.

The first Linux Realm lifecycle now runs on that service:

```text
key = (capsule digest, component id, verified principal, realm id)

cold --boot/command--> booting --ready--> running --idle--> warm
  ^                         |                  |          |
  |                         +--failure---------+          |
  +--destroy RAM <----- shutdown/failure/evict/unload-----+

```

Its non-negotiable rules are:

1. The kernel derives the principal from verified invocation provenance; no
   capsule payload may select the lease owner.
2. One Store never changes principal. Its linear-memory limiter is permanently
   bound to that principal and Realm.
3. Every admitted command or machine slice is fuel-seeded, charged, and fed into
   the same per-principal rate limiter used by ordinary capsule invocations.
4. Residency is bounded both per principal and globally. Admission fails or
   explicitly evicts an idle lease; it never grows an unbounded map in one Store.
5. Idle eviction destroys RAM and returns the Realm to `cold`. The first
   guarantee is filesystem persistence, not a memory checkpoint; home mutations
   have already selected complete generations, the invocation workspace has no
   guest state to flush after its 9P session is closed, and Linux still has no
   durable root block device.
6. The current `linux-shutdown` returns to restartable `cold`. A distinct
   operator-disabled `stopped` state that prevents implicit auto-start remains an
   explicit lifecycle-policy increment rather than an invented guarantee.
7. Audit identifies the principal, Realm identity, resident generation, hart,
   charged guest steps, charged outer fuel, peak memory, and lifecycle reason.

Principal-affine residency, idle Store eviction, the persistent Linux supervisor,
token-bound console transport, bounded BusyBox shell, durable principal home,
invocation workspace, and clean shutdown are now implemented. The next storage
order is an immutable base plus durable root overlay; explicit
disabled-vs-evicted lifecycle policy and deterministic virtual SMP remain
separate increments.
Initial SMP can interleave
harts in one host thread with a fixed scheduling quantum. True host-parallel harts
remain optional because they add races to snapshots, metering, devices, and
reproducibility without improving cross-principal isolation.

### 7.2 Generic parallel compute boundary — design only, not active

True host-parallel harts must not become a Linux-specific host function or an
implicit runtime optimization. They require one generic Astrid compute-group
boundary that can also serve tensor kernels, media transforms, local inference,
game simulation, compilation, databases, and other bounded parallel capsule
workloads. This is a proposed public runtime surface; no current manifest field
or WIT name should be treated as implemented.

The stable public contract owns control and authority:

- open one named, signed, package-declared worker group for the verified
  principal;
- resolve requested or automatic parallelism against the principal profile and
  global runtime pool;
- allocate one fixed, principal-charged shared region and a bounded set of
  worker instances;
- dispatch typed work descriptors containing worker identity, an admitted
  shared-region slice, entrypoint, and work budget;
- join, cancel, query status, and return typed terminal reasons and aggregate
  accounting;
- select deterministic interleaving or true parallel execution without changing
  the application protocol;
- tie workers to the foreground invocation unless an independently admitted
  durable job owns them.

The runtime implementation beneath that contract is intentionally hidden:

```text
capsule supervisor
  -> generic compute-group resource
       -> signed worker Store 0 ---\
       -> signed worker Store 1 ----+-> one bounded shared memory
       -> signed worker Store N ---/
       -> principal CPU ledger, deadline, cancellation, audit
```

Wasmtime compilation, native thread pooling, Store construction, shared-memory
linking, wakeups, and scheduling are kernel mechanism. The kernel does not know
about Linux harts, tensor dimensions, compiler jobs, or game entities. Those
meanings remain in the capsule and its signed worker protocol. Worker module
bytes cannot come from a tool argument; they are content-identified package
assets admitted at install/load time.

The worker data plane must not cross a host function per instruction or memory
access. Workers operate directly on the admitted shared region for a bounded
quantum and surface only scheduling boundaries or serialized effects. Device,
filesystem, network, identity, and durable-state effects still return through
the supervisor and their existing Astrid capabilities; joining a compute group
grants no new effect authority.

The resource hierarchy follows the existing Realm zero-means-delegate rule:

```text
effective parallelism = min(
  requested value or automatic allocatable capacity,
  operator principal vCPU/worker allowance,
  runtime global worker-pool capacity,
  implementation hard maximum
)
```

Guest task count, virtual CPU count, and native worker count are separate. Linux
may schedule many tasks onto fewer vCPUs, and Astrid may multiplex those vCPUs
onto its worker pool. “Automatic” means no smaller arbitrary capsule ceiling; it
never means unbounded native thread creation. A changed active envelope requires
a cold Realm restart until CPU hotplug semantics are explicitly implemented.

This public surface requires an Astrid RFC, canonical WIT and manifest-schema
work, runtime implementation, and additive SDK bindings. The WIT should expose
typed resources, region slices, budgets, lifecycle, cancellation, outcomes, and
accounting rather than pretending application payload bytes are a universal
type. SDKs can then layer ergonomic `ComputeGroup`, parallel-map, tensor, and
Realm-hart adapters over the same primitive.

Before activation it must prove aggregate fuel charging under simultaneous
workers, shared memory counted exactly once, principal separation, worker-pool
backpressure, cancellation fan-out, no worker survival after foreground return,
deterministic-mode replay, worker crash containment, and denial of undeclared
worker artifacts or excess parallelism. Wasmtime shared-memory support is not
enough by itself: its current resource-limiter gaps must be closed by Astrid's
own admission and ledger before the feature is safe for managed use.

Current upstream references:

- [Wasmtime shared-memory API](https://docs.wasmtime.dev/api/wasmtime/struct.SharedMemory.html)
- [Wasmtime proposal support and resource-limiter caveats](https://docs.wasmtime.dev/stability-wasm-proposals.html)
- [WebAssembly Component Model roadmap](https://github.com/WebAssembly/component-model)

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
  and caller-reducible fuel/output limits; the temporary `rv64-smoke` diagnostic
  selects the bounded RV64 backend explicitly rather than a signed core-WASM guest;
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
    realm-machine/      bounded RV64 CPU, RAM, console, finisher, scheduling slices
    realm-vfs/          mounts, paths, overlay, persistence, crash semantics
    realm-runtime/      nested WASM interpreter and process instances
  guests/
    smoke-write/        one write + exit guest
    pwd/                explicit CWD inspection
    echo/               argument-vector proof
    write-file/         truncate/create through a guest descriptor
    cat/                bounded streaming read through a guest descriptor
    stdin-cat/          standard-input stream primitive
    env/                bounded environment-vector proof
    guest-pipeline/     guest-created signed pipe/spawn/wait proof
    mini-shell/         structured-token foreground job construction
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
- nine embedded core-WASM guests, including the stdin consumer, environment
  printer, guest-created pipeline supervisor, and record-based mini shell;
- a private ABI containing bounded argument, environment, and CWD reads, file
  descriptors, open/read/write/close, pipe/spawn/wait/signal, monotonic time,
  and exit;
- Wasmi configured with fuel and memory limits;
- structured `linux_realm_exec` and read-only `linux_realm_status` tools;
- one principal-affine Wasmtime Store and Realm machine per verified principal,
  with CAS-allocated boot identity, runtime-bounded aggregate admission, and
  idle LRU eviction;
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
Linux-shaped descriptor boundary and a principal-affine Realm service, returns
exact output and accounting, reads and writes principal-scoped storage, survives a
daemon restart where promised, rejects cross-principal home and process-state
reads, traps safely, reaps foreground resources, and has no host-process
capability.

This proves the recursive containment model. It does not prove POSIX or Linux
compatibility.

### Seed evidence recorded on 2026-07-18

- the seed is based on `unicity-aos/aos-ce` main commit
  `dfa1d71c2737a016d8d4dd169d0755ff624f6b50`;
- seventy-one focused host tests pass, including nested argument/CWD delivery,
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
- the 0.10.1-built guest-process artifact is 413,048 bytes with outer SHA-256
  `dd3889538267a329b6694a302f857ce44afb8fecdc11f56039208d17af8e95c3` and
  installed component hash
  `5e374fbd8a96888d5fc7e55bf03ab73f2c57412e148f6963e2efca653dab0294`;
  the live `guest-pipe-echo` call created root PID 1, consumer PID 2, and producer
  PID 3 from guest code, consumed 2,517 fuel with 16 resumptions and a 192 KiB
  aggregate memory ceiling, returned exact stdout `idempotent live pipeline\n`,
  and left zero process records, pipes, or reserved bytes;
- that same live call hit the MCP shim's broken-pipe publish retry. Before the
  actor replay guard, one tool call executed twice and consumed six PIDs. With the
  bounded principal/call-ID replay guard installed, the forced retry produced one
  completed command and next PID 4; a regression test also rejects same-ID,
  different-argument reuse;
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
- the final record-spawn shell artifact is 418,699 bytes with outer SHA-256
  `6897f00f4b0e30dcb80fce2e3c257690729b22ea776acab24ab323f0378cc87c`
  and installed component hash
  `98a78de3fce2b123402dd09188a0c1f2dc087a560acc85a077ce1d1e7a5d3175`.
  An isolated Astrid 0.10.1 daemon loaded `aos-cli`, `sage-mcp`, and
  `aos-linux-realm` as ready, then the real MCP 2025-06-18 front door elicited
  ingress consent and ran `realm-sh` twice. The guest-created pipeline used root
  PID 1, consumer PID 2, and producer PID 3, consumed 5,808 fuel with 12
  suspensions and a 192 KiB aggregate memory ceiling, and returned exact stdout
  `final byte pipeline\n`. The environment job used PIDs 4 and 5, returned exact
  stdout `ASTRID_REALM=final\n`, and advanced next PID to 6. Final status reported
  two completed commands and zero retained process records, pipe objects, or
  reserved pipe bytes;
- the final generated-catalog/redirection artifact is 415,603 bytes with outer
  SHA-256
  `3947b2890a992c7b0e0051f68b295df8ca0bc469b7e528f841cbac9e3f1482d4`
  and installed component hash
  `91709be6ae12947788ca021af42a32e05479c36e7598aebff42ad36a69b42dab`.
  An isolated Astrid 0.10.1 daemon exposed the capsule through the real MCP
  2025-06-18 front door to a least-privilege `realm-redir` principal. Guest
  `realm-sh echo ... > final-component.txt` ran as PIDs 1 and 2 under boot
  sequence 3, consumed 5,577 fuel with one suspension and a 128 KiB aggregate
  memory ceiling, and returned empty stdout. A separate `cat` at PID 3 returned
  exact stdout `final component redirection\n`; status reported durable home
  generation 2, two files, two completed commands, next PID 4, and zero retained
  process records, pipe objects, or reserved pipe bytes. The same installed
  component had first selected generation 1 for `live-redirection.txt`; after a
  full daemon restart, `cat` returned exact stdout `live Astrid redirection\n`
  under boot sequence 2/PID 1 while the home stayed at generation 1 with manifest
  `bcbb9e4366ebe6f870457e9e033fdd5d5644a08c3e0e8447a9370af501764796`;
  final status again reported zero live resources. Host tests separately pump a
  message larger than pipe capacity and then read it from a distinct process.
  Kernel tests prove file descriptor 3 followed by pipe endpoints 4 and 5,
  failure-atomic rejection of file child actions, and descriptor-quota rejection
  before any outer filesystem open;
- the reviewed RV64-machine artifact is 419,387 bytes with outer SHA-256
  `5194f486ece4e8e13280656058ac1a8b114fad5f4ff6e8a8a277754905496e64`,
  raw built-Wasm SHA-256
  `af6eae6e503250baec97f4918c3066d5ead70b1dce52c93385ad6b48028e8f4c`,
  and installed component hash
  `e210d0200b7f6a4e75cae47910b536f72b089bd92e2eacdc4ff76b020a4ca662`.
  Ninety-five focused host tests pass, deny-warnings clippy is clean, and the
  linked capsule checks under `wasm32-unknown-unknown`. The live Astrid 0.10.1
  MCP 2025-11-25 front door ran `rv64-smoke`; the installed component reported
  backend `aos-rv64-interpreter`, retired exactly 23 instructions in 64 KiB of
  admitted RAM, returned exact virtual-UART stdout `AOS RV64\n`, and halted with
  exit status 0. Final status reported boot sequence 4, one completed command,
  `host_process=false`, next PID 1, and zero retained process, pipe, or
  reserved-byte resources. An immediately preceding packaged run deliberately
  encountered the MCP publish path's broken-pipe retry and still reported one
  completed command, proving replay rather than double execution. The probe does
  not allocate a semantic Realm PID yet; replacing that diagnostic adapter with
  a normal job record is an explicit pre-workbench task;
- the privileged-spine artifact is 420,501 bytes with outer SHA-256
  `12d2d52d855b6f6d445e46dfc761414bed8de1f520cfcec3933331e88ef09e4b`,
  raw built-Wasm SHA-256
  `611271245726eb0de5780c366ea715b719d5e8c95019e8e169b3a24ed145c3a8`,
  and installed component identity
  `b1f229b5fd9355421011dae7547f280c7fa17b80fa500b994ea28367a3e4088a`.
  All 609 workspace tests pass, including 103 focused Realm tests; deny-warnings
  clippy passes across all workspace targets/features; and the linked capsule
  checks under `wasm32-unknown-unknown`. An isolated, locally built Astrid 0.10.1
  daemon negotiated MCP 2025-11-25 and advertised `rv64-supervisor` in the real
  input schema. The installed component returned backend
  `aos-rv64-interpreter`, exact virtual-UART stdout `STR\n`, exit status 0,
  31 charged steps, and 64 KiB admitted RAM. Final status reported boot sequence
  1, one completed command, `host_process=false`, next PID 1, and zero retained
  process records, pipe objects, or reserved bytes. Machine-level tests separately
  assert 30 retired instructions and the midpoint/final CSR and privilege state;
  the tool response deliberately reports the bounded work charge rather than
  mislabeling ECALL as retired;
- the initial live run discovered two integration constraints rather than hiding
  them: Astrid's component FileHandle methods and workspace rename were not yet
  implemented. The resource-backed storage increment recorded below closes both;
  `/tmp` still must be authorized through the dynamic principal-home scheme so
  the manifest gate checks the resolved principal path.
- the normal `astrid start` path selected an installed 0.10.0 companion daemon even
  though the invoking CLI, builder, and realm requirement were 0.10.1; that daemon
  correctly rejected the `astrid-version >=0.10.1` capsule. Running the locally
  built 0.10.1 daemon proved the realm, but AOS startup must select and verify the
  exact product-pinned runtime companion before the realm enters the default set.

### Durable-home evidence recorded on 2026-07-19

- the image uses Linux longterm 6.18.39 and Buildroot stable 2026.05.1, the
  newest patch releases in their selected official series on the recording date;
- two independent completed Buildroot output trees produced init ELF
  `1f0ffeb20cdc708b049fdd3732ef7e1c51f85b8f13d2195dee31f4e1b37cf3ef`
  and byte-identical rootfs CPIO
  `e31a684bb547676654bf79ef36753174108d9a96d003a0b5e0aa0022d6c46e96`;
- two independent kernel output trees produced checked-in Image
  `6b888939b27c813eb9bc7c4d52ba23cd4f451658ef197777757e2b7c859d226a`;
- the real interpreter regression mounted distinct home and workspace 9P
  channels, created directories, wrote and renamed a home file, powered Linux
  down, released RAM and workspace state, cold-booted a new machine, read the
  exact home bytes, and observed reset boot-local counter state;
- all 140 focused host tests pass, deny-warnings clippy is clean, the production
  component checks for `wasm32-unknown-unknown`, and static capsule wiring reports
  two valid tools;
- the final release component is 6.1 MiB with SHA-256
  `b62f24a7158771e63d8f25fd22237ca1d209c1b98dbf204718ebab73b59de1c8`;
  the candidate `.capsule` contains those exact bytes, requires Astrid
  `>=0.10.2`, requests principal component residency, and has no
  `host_process` capability;
- a production installed-runtime proof remains deliberately unclaimed. The
  available released/local runtime identity is older than 0.10.2; weakening the
  manifest gate or relabeling experimental runtime bytes would invalidate the
  principal-affinity guarantee this capsule depends on.

### Installed Linux evidence recorded on 2026-07-20

- a locally built Astrid 0.10.4 daemon loaded the packaged capsule with an
  unlimited operator interceptor-fuel ceiling while retaining the Realm's own
  50,000,000-step, 32 MiB, 64 KiB-output, and one-vCPU envelope;
- Linux 6.18.39 cold-booted as RV64, mounted the invocation's `cwd://` projection
  at `/workspace`, read the repository's `Cargo.toml`, then created, read, and
  removed a previously absent workspace file through the audited 9P bridge;
- the live integration exposed three host-boundary mismatches and fixed each at
  its owning edge: component `lstat` was still stubbed, zero host mode bits made
  the guest projection unusable, and a missing 9P walk surfaced as `EIO` instead
  of `ENOENT` before create;
- `/home/agent` selected generation 4 after writing `durable-live`; after an
  explicit guest power-down and a fresh Linux kernel boot, the new guest read
  the exact bytes from generation 4. The proof marker was then removed and the
  home advanced to generation 6;
- this proof used the former compatibility workspace adapter. The later
  resource-backed increment removes its 10 MiB whole-file ceiling and makes
  workspace rename live; portable mode mutation remains separate because the
  frozen `astrid:fs@1.0.0` contract exposes mode observation but not `chmod`.

### Resource-backed workspace evidence recorded on 2026-07-21

- Astrid Runtime commit `1fc34bc7` implements the frozen
  `astrid:fs@1.0.0` resource lane with exact open modes, bounded positional
  read/write, truncate, data/all sync, mode reporting, same-VFS rename,
  invocation-scoped handle cleanup, principal affinity, and per-call capability,
  audit, and quota checks. Its full workspace tests and deny-warnings clippy
  pass, and the public API comparison against 0.10.4 reports additive VFS
  methods only;
- two independent clean Buildroot 2026.05.1 output trees produced byte-identical
  rootfs CPIO
  `10d26184e85add731208050fb3da9fed5e1dda7475b6e66e0d9814a221ecf3f4`.
  The AOS PID 1 ELF is
  `0a70920cd01dbab74c5b63d614b8a53b07a5bcb18d18a569728a1346c52e262c`,
  and unchanged BusyBox 1.38.0 is
  `ce034f1e35d22be85de0dbe4e63aafa32c284c669568e62cb0d61517e250e56f`;
- two independent clean Linux 6.18.39 output trees embedded that rootfs and
  produced the byte-identical checked-in raw Image
  `fd394b7e5b09638d52483fe2f417985ae1af6a730eea5bc3e415b97262f863de`;
- the real native Linux regression passes in 43.15 seconds, including cold
  boot, home/workspace 9P, exact nonzero status propagation, forged-frame
  rejection, descendant kill/reap, clean remount, and a four-byte
  `RLIMIT_FSIZE` proof whose five-byte truncate exits with status 153;
- `astrid capsule check` reports both tools wired. The installable capsule is
  `ae527591ec0c5e1d32c73aa6fd65fa337a60f98651e910a921c74c84be99c38a`
  and its raw component is
  `63d7d89db87a9c5276d1244380ac8414777b83724f40182b984150da2a2fc1a4`;
- an isolated Astrid 0.10.4 daemon loaded that artifact for `codex-code` with
  `linux_max_steps=0`, `linux_max_file_bytes=0`, 32 MiB guest RAM, 64 KiB
  captured output, and one vCPU. Through the actual MCP front door, Linux
  created an 11 MiB workspace file, renamed it, later used a seek-only tail to
  return exact final byte `Z`, and removed it. This proves the former 10 MiB
  whole-file ceiling and rename stub are gone;
- deliberately reading all 11 MiB with BusyBox `wc` exposed two independent
  production limits rather than a correctness failure in the file-handle lane:
  the current SurrealKV-backed workspace path generates excessive WAL/manifest
  churn for sequential 9P reads, and the released MCP broker returns after 50
  seconds while the target invocation continues. Astrid interrupted the still
  running Realm at the principal's 300-second timeout, after which the next
  status call succeeded and reported the Realm cold. This is explicit evidence
  that a broker timeout is not background execution and does not currently
  cancel the target;
- Oracles commit `1bb637f` makes the broker result-drain window a per-principal
  `tool_execute_timeout_ms` setting (50-second compatibility default, 23-hour
  55-minute ceiling), and Astrid Runtime commit `ed0ea4e2` adds
  `aos mcp serve --request-timeout <duration>` with a 24-hour 5-minute ceiling.
  These knobs enable aligned long foreground calls; cancellation propagation
  and durable job handles remain required before an abandoned call can be
  represented as safely cancelled or intentionally background;
- the isolated runtime then configured `aos-mcp` to 240 seconds and the MCP shim
  to 310 seconds under the principal's 300-second outer timeout. One warm Linux
  foreground call ran `sleep 90` between exact start/finish markers, remained in
  the same MCP request for more than 55 observed wall-clock seconds, returned
  exit status 0 with 901,476,732 charged guest steps and 9,019 cooperative
  suspensions, and reported zero surviving processes. This directly proves the
  configured foreground path crosses the former 50/55-second transport limit
  without converting the command into background work.
- after those commits were signed and pushed, a fresh isolated daemon loaded
  final `aos-mcp` component hash
  `8f530a45183f0eaf3e91a4bdea4d5c5c36dda8b3cff2077842e46a4b752f55e1`
  (raw SHA-256
  `d2d0f5fbfbadda050f4fc0ce81f01899f438d530873a5eb79865879be7d177fb`).
  MCP 2025-11-25 enumerated both Realm tools; a cold Linux 6.18.39 invocation
  returned `final-linux-live`, exit status 0, 17,362,940 charged guest steps,
  181 cooperative suspensions, and zero surviving processes. The following
  status call reported the Linux machine warm with one completed command and
  no process or pipe records.

### Principal-affine live evidence recorded on 2026-07-21

- the final installable Realm archive is 4.4 MiB with SHA-256
  `413ecb61369b6cf1654fd9a141deca589f4b6f88e16c5f40bbd0d9d274c2894e`.
  Astrid installed content hash
  `ffa5ce4c3d2d44a8c23750af0a1fae69833abd9f57a6d7039320f2ad9520e73a`,
  admitted its `astrid-version >=0.10.2` requirement and
  `component-residency = "principal"`, and loaded it without `host_process`;
- AOS 2026.1.1 still bundles Astrid 0.10.1, so the proof deliberately ran the
  exact locally built Astrid 0.10.4 daemon rather than weakening the capsule's
  compatibility gate. Product startup still needs to pin and verify that
  companion identity instead of relying on whichever daemon happens to be
  installed;
- the default principal restored the bound prewarm checkpoint, printed
  `AOS PREWARM RESTORED` and `AOS READY`, and reached ready in 277,798 charged
  guest steps and seven cooperative suspensions. The following shell reported
  UID 1000, Linux 6.18.39, RV64, Buildroot 2026.05.1, and `/home/agent`;
- the default principal wrote `realm-persisted` to `/home/agent/live-proof.txt`.
  A separately provisioned `linux-realm-alice` principal booted its own Realm,
  proved that file absent, and wrote `alice-only` to its own
  `/home/agent/alice-proof.txt`. After the entire daemon restarted, default
  read its original bytes and proved Alice's file absent. This is a live proof
  of durable home recovery and cross-principal isolation, not merely a unit
  model;
- invoking from the AOS repository with Linux CWD `/workspace` returned
  `/workspace` and read the repository's real `[workspace]` Cargo manifest
  through `cwd://`. Neither the tool response nor guest path surface exposed a
  physical host path;
- the fresh principal exposed an Astrid runtime defect: a Store constructed
  after the epoch ticker advanced inherited `u64::MAX` as a relative deadline,
  which wrapped behind the current epoch and trapped during instantiation. The
  signed runtime fix `6fd75db9` gives construction a finite five-minute window
  until the admitted invocation installs its principal deadline. Its regression
  advances the real engine epoch and asynchronously instantiates a real module,
  so the failure cannot return as an arithmetic-only unit-test blind spot;
- the fresh principal also made the supported ecosystem dependency explicit:
  the Realm is a tool provider, not its own ingress server. A principal needs a
  current capsule CLI and MCP broker such as `sage-mcp` before an MCP client can
  enumerate and call Realm tools. That route worked live, but those companions
  are still test-installed rather than a coherent CE default set.

The earlier persistence E2E run used the then-current `astrid-mcp` capsule as its
front door. The actor E2E used the product `aos-cli` proxy and current `sage-mcp`
broker. The privileged proof used the test-installed Codex broker; its legacy
blank manifest edges had to be normalized to explicit `wit = "opaque"` metadata
in the isolated test artifact before Astrid 0.10.1 would admit it. No broker source
was changed by the Realm increment. The Realm still must not enter the default
distribution until a current broker and invocation path are part of the supported
CE set rather than test-installed companions.

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
  principal-private temporary namespace for nested core-WASM programs;
- [x] define a versioned typed path context that distinguishes guest, Astrid, and
  person-facing paths and reports Linux's mounted home/workspace and boot-local
  temporary projection honestly;
- [x] mount the invocation's admitted `cwd://` workspace into Linux through a
  bounded synchronous 9P/SBI file transport, remount it for every call, and prove
  real Linux create/write/rename/read/readdir across two calls;
- [ ] obtain a stable workspace attachment ID/epoch from the runtime and carry it
  through admission, receipts, audit, and the Linux transport;
- [x] attach the durable principal home to Linux without conflating it with the
  invocation workspace or warm guest RAM;
- [x] implement bounded sequential open/read/write/close using whole-file Astrid
  VFS calls;
- [x] implement bounded positional I/O, stat, create, truncate, unlink, and
  synchronous flush for the invocation workspace through Astrid's whole-file
  imports, including Linux's size-plus-implicit-times truncate request;
- [x] replace the 10 MiB whole-file compatibility adapter with bounded Astrid
  resource-backed file handles and implement workspace rename;
- [ ] add portable mode mutation through a canonical filesystem WIT and SDK
  revision before claiming a complete compiler-cache contract across hosts;
- [x] implement positional write/truncate, directory operations, rename, unlink,
  permission metadata, and synchronous flush semantics for the versioned
  principal-home store before presenting it as Linux storage;
- add immutable base plus COW overlay generations;
- kill the realm during writes and verify declared crash semantics;
- restart and observe the same principal's bytes, but never another's.

### Milestone C: processes and shell substrate

- [x] define the host-testable process lifecycle, direct-child wait/reap,
  deterministic single-runner FIFO scheduling, typed terminal signals, bounded
  pipes, atomic descriptor inheritance, and aggregate process/pipe quotas;
- [x] bind resumable Wasmi process slots to the kernel for a foreground
  two-process stdout-to-stdin pipeline with measured suspension and exact output;
- [x] add principal-affine Realm state with per-boot PID continuity,
  restart-disambiguating boot identity, a redundant verified-owner guard,
  runtime-bounded admission, foreground cleanup, and live accounting;
- [x] add the bounded guest `pipe`, signed-child spawn, direct-child wait, and
  direct-child signal substrate with generation-checked handles;
- [x] add a bounded record spawn with argv, environment, exact absolute catalog
  paths, multiple pipe mappings, and atomic parent close actions;
- [x] move file descriptor allocation into the semantic kernel and reject file
  child actions until open-file-description sharing is defined;
- [x] generate the immutable signed executable catalog from validated image
  metadata and add guest-owned, file-backed `echo TEXT > PATH` redirection;
- [ ] add a realm-wide open-file-description table, then translate the record into
  libc-grade sequential `posix_spawn` file actions and add `execve` without
  host process authority;
- [ ] add PTYs, sessions, process groups, and job-control signals;
- [x] run multiple guest modules with isolated memories;
- [x] compile and run a small structured-token shell for direct, environment,
  and foreground pipeline jobs;
- add job control only with explicit conformance tests.

### Milestone D: useful agent workbench

- [x] establish the AOS-owned `aos-rv64-virt-v0` machine boundary with explicit
  RAM/output admission and bounded execution slices;
- [x] execute a real RV64I probe through the installable capsule command path and
  report exact backend, instruction, memory, output, and halt accounting;
- [x] add the first privileged boot spine: typed M/S CSRs, Zicsr read/write intent,
  M/S/U state, ECALL delegation, trap-vector entry, `mret`/`sret`, non-retirement
  accounting, and an installable Supervisor transition probe;
- [x] complete architectural delivery for instruction, load, and store faults,
  then implement Sv39 translation and `sfence.vma` against adversarial page tables;
- [x] add RV64M, RV64A, architectural counters, deterministic CLINT time, interrupt
  CSR/delegation/global-enable behavior, vectored entry, and bounded `wfi`;
- [x] add exact Linux image/initramfs placement, generated versioned FDT bytes,
  S-mode handoff, and bounded SBI 3.0 Base/TIME/DBCN/SRST firmware;
- add a PLIC when an admitted interrupting device requires it;
- [x] boot pinned Linux longterm 6.18.39 to an AOS-controlled `/init`, retain its
  exact serial evidence, and run it through the capsule command adapter;
- [x] expose the current lazy Linux lifecycle, single-vCPU topology, evictable
  resident RAM, and per-principal Store-residency guest-step totals;
- [x] add a principal-affine resident service Store to Astrid with permanent
  owner binding, per-call charging, exact resident-memory accounting, live quota
  enforcement, cancellation-safe construction, and idle LRU eviction;
- [x] keep one RV64 machine in the affined Store, leave PID 1 alive, and add a
  bounded framed console command transport with cross-invocation userspace-state
  proof, clean shutdown, restart, and fail-closed RAM destruction;
- [x] resolve a bounded inner Linux resource envelope per principal invocation,
  expose configured versus active values, and cold-reconfigure only that
  principal's warm machine while retaining Astrid's principal profile as the
  outer authority;
- [x] replace the proof initramfs with pinned Buildroot 2026.05.1, static musl
  1.2.6 and BusyBox 1.38.0; execute `ash` as UID 1000 through token-bound
  frames, strip privilege bits, close console input, bound descendants and prove
  warm file continuity plus exact exit status;
- [x] implement a GPL Linux `trans=aos` 9P transport over an experimental bounded
  SBI request, an authority-free Rust 9P device model, and an Astrid `cwd://`
  adapter; remount `/workspace` before each shell command and keep all other 9P,
  network, virtio, PLIC, and DMA paths absent;
- [x] add a separate principal-home 9P channel backed by atomic generations,
  remount it before each shell call, and prove bytes survive clean shutdown and
  cold boot while guest RAM state resets;
- add explicit Realm `start`, `stop`, and status transitions plus bounded idle
  eviction after that Store is kernel-metered to its verified principal;
- add deterministic multi-hart scheduling, SBI HSM/IPI support, per-hart CLINT
  state, and an SMP-enabled kernel only after the principal residency boundary;
- add virtio-block behind an immutable base plus principal-private COW block
  overlay before making the root filesystem writable;
- replace the probe-only execution adapter with a Realm process/job record whose
  lifecycle, cancellation, and accounting use the same semantic kernel as other
  foreground work;
- produce a signed minimal image with shell, Git, and Python;
- [x] add the invocation workspace projection;
- add typed workspace diff and artifact export through an outer promotion flow;
- add a mediated dependency fetch path;
- [x] persist the bounded agent home; expand quotas and package/tool-cache policy
  only with measured workloads;
- run a real repository inspection/edit/test loop;
- build an Astrid capsule inside the realm and install it only after independent
  verification.

### Milestone E: compatibility breadth

- choose and document the WASM-Linux toolchain target;
- build a reproducible package set;
- publish an immutable development generation containing exact Rust, Cargo,
  linker, libc, Git, and certificate identities;
- pass the seven-step in-guest Rust acceptance proof and retain its receipt;
- run Bash conformance cases;
- add Node or another agent CLI runtime only after its JIT/process assumptions are
  explicit;
- evaluate RV64/x86-64 Linux ELF compatibility;
- define package manager behavior honestly.

### Milestone F: backend substitution

- define the private backend interface at the principal-machine boundary;
- establish reference traces and measure the interpreter before choosing an
  optimisation;
- implement decoded execution, snapshot restore, or other justified fast paths
  inside the AOS-owned machine without importing another runtime;
- implement Astrid filesystem, block, console, clock, and optional network
  adapters over existing principal-scoped imports;
- run the same Realm contract over the accelerated core-WASM backend;
- optionally add a hardware Linux VM backend for native hosts;
- compare behavior and denied paths with the interpreter oracle;
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
| Guest reuses a process handle after actor restart | Generation mismatch rejects it before process-table mutation |
| Guest exceeds its admitted descendant count | Spawn fails closed; the partial foreground tree is cancelled and reaped |
| Guest supplies a malformed record, vector, environment, or catalog path | Reject before child PID allocation and preserve the parent's descriptors |
| Record maps one pipe endpoint and closes the parent copy | Child retains the endpoint; EOF/broken-pipe accounting observes no leaked parent reference |
| Guest opens a file before creating a pipe | The kernel allocates file descriptor 3 and pipe descriptors 4/5 from one process-local table |
| Spawn action selects a file before shared open-file descriptions exist | Reject before PID allocation and retain the file and parent descriptor table |
| Guest signals a blocked direct child | Child becomes waitable, abandoned continuation is not resumed, and all resources are released |
| MCP reconnect redelivers one mutating call ID | Actor returns the cached result without repeating the process tree or mutation |
| Call ID is reused with different arguments | Request fails closed rather than replaying or executing |

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
- decoded-block cache hit rate and invalidation cost;
- snapshot restore latency, dirty-page count, and checkpoint size;
- workspace staging and diff/promotion latency;
- memory per process and per realm;
- fuel-to-wall-time stability;
- Git, Python, compilation, and capsule-build workloads;
- artifact export and verification time.

The interpreter is now classified as the reference backend rather than the first
compiler backend. A faster AOS-owned core-WASM backend is admitted only after it
reproduces the reference traces and preserves revocation and accounting. A quick
shell boot from a prewarmed snapshot is evidence for snapshot design, not evidence
that `rustc`, Cargo dependency traversal, linking, or large workspace I/O is
usable.

### 18.1 Benchmark protocol

`scripts/benchmark-linux-realm.py` and the host-only `benchmark_linux` machine
example make the first comparison reproducible. They emit versioned JSON Lines
with raw samples and summaries rather than copying one favorable stopwatch value
into this document. The initial matrix uses preloaded immutable artifacts and
separates these boundaries:

| Scenario | Starts at | Ends at | What it proves |
| --- | --- | --- | --- |
| `cold-to-init` | Native reference allocation | PID 1 `AOS LINUX /init` marker | AOS machine cold kernel/userland boot |
| `cold-to-principal-bind` | Native reference allocation | First unfulfilled principal-home 9P request | Cold computation needed before authority attachment |
| `checkpoint-to-bindable` | Encoded checkpoint in memory | Integrity-checked sparse machine at the same request | Principal-free restore cost before fresh authority is supplied |
| `qemu-tcg-cold-to-init` | Fresh QEMU process | Same PID 1 marker from the exact AOS Image | Shared kernel-to-init comparison only |
| `docker-run-to-exit` | Docker CLI request | Existing-image `/bin/true` exits and container is removed | Container creation/start path, not Linux boot |
| `docker-unpause` | Paused resident container | Docker CLI unpause returns | Resident process unfreeze, not restore or boot |

The AOS marker is observed at the next 100,000-step cooperative slice boundary,
so its reported init time has that explicit upward resolution bound. The QEMU
lane pins one RV64 vCPU and single-threaded TCG. It includes QEMU process and
OpenSBI startup, but it cannot reach `AOS READY` because QEMU does not own
Astrid's home/workspace providers. Docker is opt-in, never pulls implicitly, and
records missing engines as skips.

Every committed baseline must use at least 30 measured samples after at least
three discarded warmups and record exact Git, image, checkpoint, host, and engine
identity. Median and p95 are the primary latency statistics. The next matrix must
add the signed outer-Wasm capsule through the real MCP front door, first useful
shell completion, QEMU snapshot restore, Docker/CRIU restore where supported,
peak and retained RSS, concurrent-principal scaling, and—after the development
generation exists—compiler workloads. Native reference results must never be
presented as end-to-end Astrid latency.

### 18.2 First recorded baseline, 2026-07-21

The raw 30-sample baseline after three discarded warmups is
`benchmarks/linux-realm/2026-07-21-m2-ultra-9aa1885.jsonl`. It ran commit
`9aa1885` on an Apple M2 Ultra with Rust 1.95.0 and QEMU 11.0.2. Both cold lanes
used the exact checked-in Linux Image; QEMU was pinned to one vCPU and
single-threaded TCG.

| Engine and boundary | Median | p95 | Interpretation |
| --- | ---: | ---: | --- |
| AOS native reference, cold to PID 1 marker | 276.619 ms | 323.653 ms | Allocation, admission, and 15,899,016 charged steps |
| AOS native reference, cold to principal bind | 276.619 ms | 323.653 ms | The home request occurs in the same observed slice |
| QEMU TCG, process start to PID 1 marker | 263.929 ms | 313.747 ms | Exact Image; includes QEMU process and OpenSBI startup |
| AOS checkpoint to bindable machine | 4.862 ms | 5.027 ms | Full digest/binding validation and sparse 32 MiB materialization; no provider completion |

The AOS reference median is 4.8% behind QEMU TCG at the shared cold marker.
Checkpoint admission is 56.9 times faster than the AOS cold median. This is the
first evidence that the 57-times guest-work reduction also survives as a
similar host-latency reduction inside the native machine boundary. It is not yet
a claim that an MCP-visible shell returns in 4.862 ms: fresh principal provider
completion, the remaining 277,798 guest steps, outer Wasm execution, broker
routing, and client transport are outside this sample.

Docker 29.1.2 was installed but its server was unavailable, so the raw result
contains an explicit skip. No container, CRIU, QEMU snapshot, end-to-end AOS,
RSS, or concurrent-principal comparison is inferred from that absence.

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

There is one supported distribution: `AOS Realm`. Its minimal Linux, musl,
BusyBox, AOS init, verifier, and content-store client form the permanent trusted
bootstrap and recovery system. Bash, Git, Python, Rust, Node, Astrid build tools,
and agent applications become signed packages or selected system generations
inside that distribution, not separately branded distros. The minimal system
must remain capable of verifying and selecting a repaired generation even when a
larger toolchain generation does not boot.

“Latest” is resolved at generation-build time and then frozen. As of this design
audit on 2026-07-21, the initial development-generation target is
[Rust 1.97.1](https://blog.rust-lang.org/releases/latest/), released 2026-07-16,
rather than the older compiler available in a convenient base distro. If that
release cannot yet be reproduced for the selected RV64 libc, the image manifest
must name the newest supported version and the blocking delta; it must not report
the host's Rust version. Distribution updates create a new signed generation and
never mutate a running principal's toolchain in place.

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

## 21. Deferred agent-OS and Tensor Logic seam

Status: design scaffold only. It is deliberately not active in scheduling,
authorization, routing, or metering yet.

The full cross-repository designs are pinned at Astrid commit `d505d46c`:
[AI-native OS workplan](https://github.com/astrid-runtime/astrid/blob/d505d46c671effeee919fd4deb86f6d4a40e2e6d/docs/astrid-ai-native-os-workplan.md),
[driver domain contract](https://github.com/astrid-runtime/astrid/blob/d505d46c671effeee919fd4deb86f6d4a40e2e6d/docs/astrid-driver-domain-contract.md),
[Tensor Logic composition](https://github.com/astrid-runtime/astrid/blob/d505d46c671effeee919fd4deb86f6d4a40e2e6d/docs/astrid-tensor-logic-composition.md),
and [native-kernel scope](https://github.com/astrid-runtime/astrid/blob/d505d46c671effeee919fd4deb86f6d4a40e2e6d/docs/astrid-native-kernel.md).
This section records only the Linux Realm's integration seam with those designs.

The larger agent-OS direction takes Plan 9's useful lesson—small named resources,
composable namespaces, and protocol-shaped services—without forcing every device
or semantic object into an undifferentiated file abstraction. Astrid's existing
principal, capability, capsule, WIT, topic, and audit boundaries remain the real
system. Capsules may dock as services or applications; the kernel continues to
route, isolate, meter, revoke, and record rather than learning business logic.

The future connection fabric is a typed relation over all admitted inputs,
outputs, and interfaces. It is not a knowledge graph. A minimal descriptor needs:

```text
Port {
  owner_capsule_digest
  interface_type
  direction: Input | Output | Duplex
  schema_or_tensor_shape
  scalar_type
  units_or_semantic_kind
  cardinality
  capability_requirements
  confidentiality_and_integrity_label
  latency_and_cost_model
  determinism
  version
}

Connection {
  producer_port
  consumer_port
  adapter_chain
  principal_and_grant
  budget
  lifecycle
}
```

Tensor Logic is reserved as a later AI-language and planning backend over those
typed relations: Datalog-like rule selection, einsum-like tensor composition,
and learned scoring may propose or optimize valid connections. It is not a proof
engine, and it does not replace capability checks. A conventional deterministic
validator must first decide schema, direction, version, authority, information
flow, resource, and lifecycle compatibility; only admitted candidates reach a
Tensor Logic planner. This seam lets the algebra arrive later without putting
tensor dependencies or opaque learned decisions into the current kernel.

The same model explains drivers and graphics. A driver advertises typed device
ports and implements a narrow protocol behind them. A WASM service can transform,
schedule, or validate requests, while a protected native mechanism retains MMIO,
DMA, interrupts, and physical reset authority. A game capsule eventually targets
a versioned surface/command-buffer/input/audio contract; the GPU portal validates
resources and commands before the physical driver sees them. Linux's 9P stack is
the first executable example of that split.

This does not need another repository yet. The Linux Realm belongs in `aos-ce`
while it is a product capsule and design probe. Generic runtime enforcement stays
in Astrid core; stable cross-capsule contracts belong in canonical WIT with an RFC;
SDK projections follow those contracts. A new repository becomes justified only
when the connection algebra or agent distribution has an independently versioned
release, conformance suite, and maintainership boundary. Until then, additive
interfaces preserve every current capsule and let services opt in one at a time.

## 22. Open decisions

The first implementation must resolve these with executable evidence:

1. Which measured bottleneck should the first AOS-owned fast path address, and
   when should the machine implementation graduate to a separately versioned
   crate after conformance is established?
2. Which WASI resource interfaces are useful enough to implement as Astrid host
   providers, without introducing a second engine, scheduler, or authority model?
3. Which image shell should follow the WAT mini-shell once redirection and job
   records are available, before Bash becomes a conformance workload?
4. Which retention and garbage-collection policy should preserve named
   checkpoints over the now-selected KV-head/content-addressed-blob filesystem?
5. Is direct principal VFS projection safe enough, or should the first realm use a
   single private block volume?
6. How are guest continuations represented for `fork`, signals, and blocking calls?
7. Which dynamic-code mechanism is allowed for Node and other JIT runtimes?
8. Which ideas from BrowserPod, vpod, and other systems survive Astrid's exact
   fuel, suspension, MMU, and governed-effect requirements when reimplemented in
   the AOS-owned machine?
9. Should binary compatibility target RV64, x86-64, or neither until the
   WASM-native package set is measured?
10. At what stable boundary does a public realm WIT RFC become necessary?
11. Which existing first-party capsules genuinely need bounded realm jobs, and
    which must retain a narrower service dependency?
12. What fair scheduling and admission policy should replace serialized foreground
    calls before principals may keep background jobs?
13. Should idle principal machines be evicted, and if so which process, descriptor,
    and boot-generation conditions make eviction observable and safe?
14. What outer work unit fairly meters decoded-block execution when exact retired
    instruction counts and wall time differ from the reference interpreter?
15. Should workspace staging become a general Astrid resource snapshot/diff
    primitive, or remain a private Realm implementation until another capsule
    needs the same operation?

Decision 13 for the eventual explicit lifecycle: evict only an idle Realm with
no foreground job, refuse implicit eviction of background work, flush its selected
filesystem generation, destroy RAM, preserve its boot-generation history, and
report the eviction reason. The current proof has no background jobs or
durable Linux root filesystem to flush. Home mutations are synchronous selected
generations, and its invocation-scoped 9P workspace is remounted at each
foreground boundary and cannot make progress while the guest is frozen. It maps
clean shutdown and eviction to restartable `cold`; a future operator-disabled
`stopped` state will require explicit restart.

## 23. Implementation ledger and immediate task list

- [x] place `capsule-linux-realm` in the authoritative `unicity-aos/aos-ce`
  monorepo;
- [x] establish the versioned seed ABI with `write`, `clock-monotonic-ns`, and
  `exit`, then extend that private version additively with bounded process
  operations;
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
- [x] add normalized `/home/agent`, `/workspace`, and `/tmp` projections for the
  nested core-WASM lane;
- [x] return versioned `PathRef`/`MountContext` data, requested and effective CWD,
  audience-specific renderings, and projection state without physical host paths;
- [ ] carry an opaque workspace attachment ID and epoch from runtime admission to
  Realm receipts and the already-live Linux file transport;
- [x] implement the bounded 9P2000.L server, Astrid `cwd://` adapter, private
  SBI host-request boundary, GPL Linux transport, per-call remount, and exact
  Linux read/write/rename/remount regression without adding virtio, PLIC, sockets,
  DMA, or host-process authority;
- [x] move the workspace adapter onto invocation-scoped `astrid:fs` file
  resources with bounded positional I/O, truncate, sync, mode reporting, and
  same-VFS rename, removing the 10 MiB whole-file compatibility ceiling;
- [x] replace PID 1's fixed 8 MiB `RLIMIT_FSIZE` with the principal-resolved
  `linux_max_file_bytes` envelope; zero removes only the inner per-file ceiling
  and leaves Astrid's outer principal storage quota mandatory;
- [x] make `linux_max_steps=0` delegate to Astrid's outer principal CPU and
  timeout policy so local foreground builds have no hidden 50-million-step
  ceiling, while shared services can configure a finite exact instruction cap;
- [x] make the AOS MCP broker result drain principal-configurable and add an
  explicit MCP shim request-timeout flag so long foreground calls can align
  their transport windows with the outer principal profile;
- [ ] propagate correlated MCP cancellation through the broker to the exact
  target invocation, and prove timeout/disconnect cannot leave an unobserved
  Realm consuming the principal's budget;
- [ ] add principal-owned background job handles with status, logs, wait,
  cancel, deadline, output sink, and eviction policy before allowing any guest
  process to survive a foreground result;
- [ ] stream outer workspace COW copy-up and promotion so modifying a lower
  file is no longer subject to the runtime's separate 50 MiB transition limit;
- [ ] specify portable workspace mode mutation in the canonical filesystem WIT
  and SDK rather than inventing a Linux-Realm-only host call;
- [x] attach Linux `/home/agent` over a second 9P channel to the selected
  principal generation; implement file, directory, positional, rename, unlink,
  and flush semantics; and prove cold-boot persistence separately from RAM;
- [x] verify principal-scoped home persistence across daemon restart and reject a
  second principal's read of those bytes;
- [x] invoke the packaged capsule through a live Astrid 0.10.1 daemon and MCP
  front door, including ingress consent and grant-on-use;
- [x] add crash-consistent principal-home generations with an atomic KV head,
  immutable content-addressed manifests and files, bounded concurrent-writer
  retry, corruption checks, and bounded migration from the format-0 direct home;
- [x] make guest file flush a no-op only after every successful mutation has
  selected and verified a complete generation;
- [ ] add retained named checkpoints, diff/reset, and unreachable-blob garbage
  collection;
- [x] implement `aos-realm-core` as the backend-independent process/descriptor
  oracle, including monotonic PIDs, zombies, direct-child wait/reap, reparenting,
  deterministic admission, bounded pipes, endpoint inheritance, wakeups, EOF,
  broken-pipe behavior, and failure-atomic quota checks;
- [x] run two signed guest modules with isolated memories through the core
  scheduler, a four-byte bounded pipe, resumable read/write host calls, partial
  producer writes, consumer EOF, and exact combined accounting;
- [x] add principal-affine Realm state with isolated machine state, monotonic
  per-boot PIDs, CAS-allocated boot sequences, read-only idle status, a redundant
  owner guard, runtime-bounded residency, and foreground process/pipe cleanup;
- [x] verify the actor model against Astrid Runtime 0.10.1 and record that recv
  switches principal data context but not the dedicated Store's outer CPU/RAM
  attributee;
- [x] expose Linux first as cold/on-demand with exact principal-local guest-step
  totals rather than imply persistent RAM;
- [x] implement the principal-affine resident service lease in Astrid Runtime;
- [x] move Linux lifecycle ownership onto it by retaining one RV64 machine and
  defining a bounded command/result channel, clean shutdown, and lazy restart;
- [x] prove two principal-affine Stores through the real MCP front door, retain
  separate warm machines and durable homes, survive a daemon restart, and fix
  the late-epoch Store-construction deadline wrap exposed by the second
  principal;
- [ ] add deterministic virtual SMP within one principal-owned Realm after that
  lifecycle and metering boundary is executable;
- [ ] specify the generic principal-owned compute-group RFC and canonical WIT
  before adding true host-parallel harts; keep lifecycle, budgets, cancellation,
  shared-region ownership, and accounting generic while Linux remains an SDK
  consumer;
- [ ] implement aggregate-metered signed worker groups and prove deterministic
  and parallel modes against the same protocol before allowing
  `linux_vcpus=auto` to allocate multiple native workers;
- [x] expose bounded pipe creation, signed child creation, direct-child wait, and
  direct-child signal through the private guest ABI without allowing jobs to
  escape foreground actor accounting;
- [ ] restore duplicate-delivery safety at the SDK/runtime direct-tool boundary;
  the former manual actor replay cache is not reachable from the affined tool
  method because the call ID is not exposed;
- [ ] add an outer workspace diff/promote workflow; realm code must not silently
  commit its own COW projection;
- [ ] put a supported MCP broker/invocation front door in the CE distribution
  before selecting the realm by default; the live fresh-principal proof required
  separately installing the current capsule CLI and `sage-mcp`;
- [ ] make product startup verify and launch the exact pinned Astrid daemon rather
  than an older installed companion; AOS 2026.1.1 currently selected Astrid
  0.10.1 while this principal-affine proof required the locally built 0.10.4;
- [x] generalize the signed executable catalog and spawn record, then add a small
  shell over the now-live guest-created process/pipe/wait/signal substrate;
- [x] generate that catalog from one validated manifest, move file descriptor
  identity/allocation into `realm-core`, reject undefined file child actions
  atomically, and add guest-owned `echo TEXT > PATH` redirection with exact
  persisted-byte and cleanup tests;
- [x] add the host-testable `aos-realm-machine` crate and route `rv64-smoke`
  through the real capsule adapter: 23 RV64 instructions, bounded virtual UART,
  standard finisher, explicit backend identity, fuel/output traps, no browser or
  host-process dependency, and a successful `wasm32-unknown-unknown` build;
- [x] route `rv64-supervisor` through that adapter with ratified Zicsr and
  privileged-ISA semantics: reset M-mode, `mret` entry to S-mode, delegated
  S-mode ECALL, `sret`, exact `STR\n`, 31 charged steps, and 30 retired
  instructions;
- [x] audit the Apache-2.0 `vpod` v0.4.0/current-main split, prove its engine-free
  `riscv-core` and `machine` crates compile for `wasm32-unknown-unknown`, and run
  its official snapshot only as separately labelled RV64 Linux comparison
  evidence;
- [x] define and implement the private backend interface without changing the
  Realm tool schema, then keep `aos-rv64-interpreter` as its first production
  implementation;
- [x] measure the reference interpreter, then implement the first justified fast
  path in the AOS-owned machine without importing another runtime or authority
  model;
- [x] add a versioned raw-sample benchmark harness and record a 30-sample
  exact-Image cold comparison against QEMU TCG plus integrity-checked checkpoint
  admission on identified hardware;
- [ ] extend that matrix through the signed outer-Wasm/MCP route, QEMU snapshot,
  Docker start/unpause and CRIU where available, RSS, and concurrent principals;
- [ ] pass reference-trace and denied-path conformance before selecting the
  accelerated backend for a principal;
- [x] define a principal-free host-suspension checkpoint, add the sparse bound
  codec and reproducible builder, embed the 32 MiB artifact, and prove each
  restore attaches fresh home/workspace providers before ready;
- [ ] build an immutable AOS Realm development generation with the current
  supported stable Rust/Cargo/linker/libc identities and enough measured memory
  for compiler workloads;
- [ ] complete the seven-step in-guest Rust build/run/artifact receipt proof;
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

The Linux-bearing executable artifact is now a useful static workbench: pinned
Linux 6.18.39 serial-boots the Buildroot 2026.05.1 rootfs, executes token-bound
BusyBox `ash` commands as UID 1000 across separate invocations, preserves warm
userspace RAM, mounts the invocation's COW workspace through a bounded 9P/SBI
portal, mounts the selected principal-home generation through a second channel,
powers down cleanly, restarts, and reads the same home bytes after cold boot.
The next storage artifact is an immutable base plus durable root overlay; the
workspace remains a narrower invocation-scoped capability. The firmware contract
beneath it is executable:
exact image/initramfs placement, generated FDT bytes, S-mode register state,
bounded SBI 3.0 Base/TIME/DBCN/SRST handling, and typed host-request suspension.
Every artifact must run in bounded slices and
fail with an exact architectural trap before virtio block, root persistence,
networking, or broader distribution packages are added. The parallel process track still needs
guest-visible executable lookup, the realm-wide open-file-description table,
sequential spawn actions, and explicit foreground job records. The storage track
adds named checkpoint/diff/reset and outer workspace promotion. Bash and a
compiler remain acceptance workloads, not claims made by this seed.
