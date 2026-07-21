# AOS Linux image

This directory contains the Linux-bearing artifact for `aos-rv64-virt-v0`.
Linux 6.18.39 boots the checked-in Buildroot 2026.05.1 `newc` root filesystem,
runs an AOS-owned static `/init` as PID 1, and keeps one single-hart guest
resident per admitted principal Store.

The current userland is intentionally small but real:

- static musl 1.2.6;
- BusyBox 1.38.0 and its `ash` shell;
- an unprivileged `agent` account at UID/GID 1000 with `/home/agent`;
- a locked root password and no login service;
- `/proc`, `/sys`, static device nodes, and ordinary Linux process/syscall
  execution; and
- no set-user-ID or set-group-ID executables.

The static device contract contains only `/dev/console`, `/dev/null`,
`/dev/zero`, `/dev/random`, and `/dev/urandom`. In particular there is no raw
memory, block, network, graphics, additional TTY, or PTY device node for the
agent shell. The kernel also disables legacy `TIOCSTI`, both PTY families,
IP/Unix networking and network devices, raw memory/port devices, input, and
media support.

Linux does have one capability device that is not represented as a `/dev` node:
its built-in `trans=aos` 9P transport. PID 1 mounts the principal's durable home
at `/home/agent` and the invocation workspace at `/workspace` with
`version=9p2000.L`, `msize=65536`, `cache=none`, and `access=client`. The kernel
transport sends one complete bounded request through private experimental SBI
extension `0x08414f53`: channel 1 selects the principal-home generation and
channel 2 resolves the invocation's workspace. The outer machine copies the
request out of admitted guest RAM, pauses the guest while the capsule serves it,
copies the bounded response back, and resumes Linux. There is no PLIC, virtio,
socket, network device, shared-memory ring, or host filesystem handle in this
path.

This is not Bash, Debian, or a development distribution yet. It has no Python,
GCC inside the guest, package manager, network device, block device, PTY, or
durable Linux root disk. `/home/agent` is the separate crash-consistent Realm
filesystem: each successful mutation selects an immutable principal generation,
so its bytes survive warm calls, guest shutdown, component eviction, and daemon
restart. `/workspace` is the invocation's Astrid COW resource, not a Linux disk.
PID 1 unmounts and remounts both views before every shell command; the home
reattaches the current durable head, while workspace remounting ensures stale 9P
FIDs cannot cross an invocation boundary. `/tmp` and the initramfs root remain
boot-local RAM.

## Command protocol

PID 1 disables terminal echo and accepts one canonical console line:

```text
AOS/1 <32-lowercase-hex-token> sh <max-file-bytes> <cwd-byte-length> <cwd> <script>\n
```

Lifecycle and diagnostic commands retain their shorter fixed forms. The shell
The per-file limit is a decimal byte count; zero means no additional inner
`RLIMIT_FSIZE`, while Astrid's outer principal storage quota remains mandatory.
The frame's CWD length makes the absolute guest CWD unambiguous; only
`/home/agent`, `/workspace`, and their normalized descendants are admitted.

PID 1 emits token-bound `AOS BEGIN` and `AOS END <token> <status>` frames followed
by `AOS READY`. The capsule generates the 128-bit token from Astrid's host
CSPRNG for every call and accepts only the exact matching terminal frame before
the next ready marker. The shell receives the script, not the token. Its stdin
is `/dev/null`; PID 1 reopens stdout and stderr as write-only console file
descriptions before dropping credentials; and `/dev/console` is root-only. It
runs as UID/GID 1000 with `no_new_privs`, no supplementary groups, no core
dumps, 64 descriptors, at most 32 UID-owned processes, and the principal's
configured per-file limit. A zero per-file limit delegates capacity entirely to
Astrid's outer storage boundary; it does not mean unmetered host storage.

PID 1 kills and reaps every remaining descendant before it emits the result.
Consequently a background process cannot survive a tool call or write into a
later call's output. The outer capsule independently limits guest RAM, output
bytes, and cooperative scheduling slices, and can impose a finite exact RV64
step ceiling. With the default zero inner step ceiling, Astrid's principal CPU
and timeout policy remains authoritative. The guest advances only during a
metered invocation.

The diagnostic `linux-console` surface admits only `ping`, `counter`, and
`echo`. Arbitrary shell text is sent only by the explicit `linux-sh` action.
`shutdown` powers the guest down through the admitted SBI reset extension.

## Reproducible inputs and builds

`SOURCES.lock` records the verified kernel.org and Buildroot archives, the
Buildroot signing-key fingerprint, exact source/toolchain versions, the pinned
OCI builder, and all retained artifact digests. `build-userland.sh` requires an
exact Buildroot 2026.05.1 tree, verifies the generated RV64IMA/static-musl
configuration before compiling, uses Buildroot's forced package-hash checks,
and rejects a rootfs digest mismatch. `build-image.sh` similarly requires Linux
6.18.39, Clang/LLD 18.1.3, the recorded rootfs, and a clean output directory.

`prewarm-32m.aos-machine` is not another operating system or an imported VM. It
is a sparse checkpoint produced by the AOS-owned RV64 machine after the exact
checked-in image reaches its first, still-unanswered home 9P request. No
principal filesystem response or console input has entered the machine at that
point. The codec binds the state to BLAKE3 digests of `Image` and `SOURCES.lock`,
checks its own payload integrity, validates every decoded resource and CPU field,
and restores only for the exact 32 MiB envelope. The signed capsule artifact is
the authenticity boundary. `PREWARM.lock` records the reproducible checkpoint
receipt and outer file digests without creating a circular input binding.

The active Buildroot output directory must be on a Linux filesystem. GNU
package configure probes create path patterns that macOS file-sharing mounts do
not reliably support. A container or Linux CI worker is only the reproducible
build workshop: Docker, QEMU, and host Linux are not present when Astrid runs
the resulting capsule.

Inside the builder recorded in `SOURCES.lock`:

```sh
./build-userland.sh /work/buildroot-2026.05.1 /build/rootfs-out /work/rootfs.cpio
./build-image.sh /work/linux-6.18.39 /work/rootfs.cpio /build/kernel-out /work/Image
```

`build-userland.sh` uses `BR2_DL_DIR` when supplied; otherwise it creates a
sibling cache next to the output directory. It never writes downloads into the
source tree or the container root filesystem.

Both scripts default to fail-closed digest verification. `AOS_RECORD_USERLAND=1`
and `AOS_RECORD_IMAGE=1` only print candidate digests for an intentional source
refresh; recording them still requires review plus an independent clean rebuild.

After reproducing `Image`, regenerate the prewarm artifact through the machine
itself:

```sh
cargo run --release -p aos-realm-machine --example build_prewarm -- \
  capsules/capsule-linux-realm/linux/Image \
  capsules/capsule-linux-realm/linux/SOURCES.lock \
  capsules/capsule-linux-realm/linux/prewarm-32m.aos-machine
```

The builder rejects a halt, trap, unexpected channel, missing init marker,
undrained output, or principal-bearing console input. Default cold Realm startup
then restores this state, attaches fresh principal home and invocation workspace
sessions, and reaches `AOS READY`. A different admitted RAM size takes the full
cold-boot path rather than coercing the checkpoint.

The checked-in `Image` contains the GPL-2.0-only Linux kernel and GPL-2.0-only
BusyBox; the in-kernel `trans_aos.c` transport is GPL-2.0-only because it is
compiled into Linux. That boundary does not change the MIT/Apache licensing of
the Rust RV64 machine, 9P server, capsule, or Astrid runtime: they communicate
with Linux across the SBI protocol rather than linking into it. The statically
linked musl portions retain musl's MIT license. Release packaging must retain all
notices and make the exact corresponding sources from `SOURCES.lock` available.
`legal-info/` retains the exact target manifest, generated Buildroot
configuration, target license texts, and kernel license; its README defines the
larger corresponding-source release gate.

To exercise the image through the AOS interpreter rather than QEMU, including a
durable-home write/rename, clean shutdown, cold boot, and exact readback:

```sh
cargo test --release -p aos-linux-realm \
  linux_boot_and_console_preserve_userspace_across_invocations -- --nocapture
```

QEMU remains useful only as an independent control experiment. It is not linked
into AOS or required by the product path.
