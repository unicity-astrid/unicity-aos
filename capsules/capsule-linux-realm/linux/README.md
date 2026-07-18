# AOS Linux boot image

This directory contains the first Linux-bearing artifact for
`aos-rv64-virt-v0`. It is deliberately smaller than the eventual AOS Realm
distribution: Linux 6.18.39 boots an embedded `newc` initramfs, executes the
static RV64 `/init`, prints `AOS LINUX /init`, and requests poweroff. There is no
shell, libc, package manager, network, block device, or writable root yet.

`SOURCES.lock` records the verified kernel.org archive digest, exact builder
image and toolchain, and resulting artifact digests. `build-image.sh` performs
no network access and refuses the wrong Linux or compiler version. It starts
from upstream `tinyconfig`, applies the exact AOS machine-profile choices,
fixes all build identity and timestamps, and rejects a non-matching result.

The checked-in `Image` is a raw uncompressed RISC-V Linux image. It contains the
GPL-2.0-only Linux kernel; its complete corresponding upstream source is the
exact archive identified in `SOURCES.lock`, and the configuration and AOS init
source used to build it are generated or present here. Release packaging must
retain Linux's notices and make the exact corresponding source available
alongside the binary.

To reproduce inside the builder recorded in `SOURCES.lock`:

```sh
./build-image.sh /work/linux-6.18.39 /work/aos-linux-build /work/Image
```

To run the retained host-side boot proof through the AOS machine rather than
QEMU:

```sh
cargo run --release -p aos-realm-machine --example boot_linux -- \
  capsules/capsule-linux-realm/linux/Image
```

QEMU may be used separately as a control experiment for the same image. It is
not used by the capsule, linked into AOS, or required by the product path.
