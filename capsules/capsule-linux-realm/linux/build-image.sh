#!/bin/sh
set -eu

if [ "$#" -lt 3 ] || [ "$#" -gt 4 ]; then
    echo "usage: $0 LINUX_SOURCE ROOTFS_CPIO BUILD_DIR [OUTPUT_IMAGE]" >&2
    exit 64
fi

caller_dir=$(pwd)
kernel_source=$(CDPATH='' cd -- "$1" && pwd)
rootfs_dir=$(CDPATH='' cd -- "$(dirname -- "$2")" && pwd)
rootfs_cpio=$rootfs_dir/$(basename -- "$2")
case $3 in
    /*) build_dir=$3 ;;
    *) build_dir=$caller_dir/$3 ;;
esac
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
if [ "$#" -eq 4 ]; then
    case $4 in
        /*) output_image=$4 ;;
        *) output_image=$caller_dir/$4 ;;
    esac
else
    output_image=$script_dir/Image
fi
expected_rootfs=e31a684bb547676654bf79ef36753174108d9a96d003a0b5e0aa0022d6c46e96
expected_image=6b888939b27c813eb9bc7c4d52ba23cd4f451658ef197777757e2b7c859d226a
record_image=${AOS_RECORD_IMAGE:-0}

if [ "$record_image" != 0 ] && [ "$record_image" != 1 ]; then
    echo "AOS_RECORD_IMAGE must be 0 or 1" >&2
    exit 64
fi

if [ -e "$build_dir" ]; then
    echo "BUILD_DIR must not already exist: $build_dir" >&2
    exit 65
fi
if [ ! -x "$kernel_source/scripts/config" ]; then
    echo "not an extracted Linux source tree: $kernel_source" >&2
    exit 66
fi
if [ ! -f "$rootfs_cpio" ]; then
    echo "rootfs cpio does not exist: $rootfs_cpio" >&2
    exit 66
fi
if [ "$(make -s -C "$kernel_source" kernelversion)" != "6.18.39" ]; then
    echo "build-image.sh requires exact Linux 6.18.39 sources" >&2
    exit 66
fi
if ! clang --version | head -n 1 | grep -q '18\.1\.3'; then
    echo "build-image.sh requires Clang 18.1.3 for the recorded image" >&2
    exit 69
fi
if ! ld.lld --version | head -n 1 | grep -q '18\.1\.3'; then
    echo "build-image.sh requires LLD 18.1.3 for the recorded image" >&2
    exit 69
fi

transport_patch=$script_dir/kernel/net-9p-aos.patch
transport_source=$script_dir/kernel/trans_aos.c
if [ ! -f "$transport_patch" ] || [ ! -f "$transport_source" ]; then
    echo "AOS 9P kernel transport sources are missing" >&2
    exit 66
fi
if grep -qxF 'config NET_9P_AOS' "$kernel_source/net/9p/Kconfig" \
    && grep -qF 'obj-$(CONFIG_NET_9P_AOS) += 9pnet_aos.o' "$kernel_source/net/9p/Makefile" \
    && grep -qxF '9pnet_aos-objs := \' "$kernel_source/net/9p/Makefile"; then
    : # The exact source tree was already prepared by an earlier build.
elif patch --batch --forward --dry-run -d "$kernel_source" -p1 \
    < "$transport_patch" >/dev/null 2>&1; then
    patch --batch --forward -d "$kernel_source" -p1 < "$transport_patch"
else
    echo "AOS 9P kernel transport patch does not apply cleanly" >&2
    exit 66
fi
cp "$transport_source" "$kernel_source/net/9p/trans_aos.c"

actual_rootfs=$(sha256sum "$rootfs_cpio" | cut -d ' ' -f 1)
if [ "$record_image" = 0 ] && [ "$actual_rootfs" != "$expected_rootfs" ]; then
    echo "rootfs digest mismatch: expected $expected_rootfs, got $actual_rootfs" >&2
    exit 70
fi

mkdir -p "$build_dir"

make -C "$kernel_source" O="$build_dir/kernel" \
    ARCH=riscv LLVM=1 LLVM_IAS=1 tinyconfig
"$kernel_source/scripts/config" --file "$build_dir/kernel/.config" \
    --enable 64BIT \
    --enable MMU \
    --enable RISCV_SBI \
    --enable NONPORTABLE \
    --disable SMP \
    --disable RISCV_ISA_C \
    --disable FPU \
    --disable RISCV_ISA_V \
    --disable MODULES \
    --disable EFI \
    --disable HIBERNATION \
    --disable CPU_IDLE \
    --enable PRINTK \
    --enable MULTIUSER \
    --enable POSIX_TIMERS \
    --enable TTY \
    --disable VT \
    --disable LEGACY_TIOCSTI \
    --disable UNIX98_PTYS \
    --disable LEGACY_PTYS \
    --enable HVC_DRIVER \
    --enable HVC_RISCV_SBI \
    --enable SERIAL_EARLYCON_RISCV_SBI \
    --disable DEVMEM \
    --disable DEVPORT \
    --disable INPUT \
    --disable MEDIA_SUPPORT \
    --enable NET \
    --disable PACKET \
    --disable UNIX \
    --disable INET \
    --disable NETDEVICES \
    --disable ETHTOOL_NETLINK \
    --enable NET_9P \
    --enable NET_9P_AOS \
    --disable NET_9P_FD \
    --disable NET_9P_VIRTIO \
    --disable NET_9P_XEN \
    --disable NET_9P_USBG \
    --disable NET_9P_RDMA \
    --disable NET_9P_DEBUG \
    --enable 9P_FS \
    --disable 9P_FSCACHE \
    --disable 9P_FS_POSIX_ACL \
    --disable 9P_FS_SECURITY \
    --enable BINFMT_ELF \
    --enable BINFMT_SCRIPT \
    --enable PROC_FS \
    --enable SYSFS \
    --enable DEVTMPFS \
    --enable DEVTMPFS_MOUNT \
    --enable BLK_DEV_INITRD \
    --set-str INITRAMFS_SOURCE "$rootfs_cpio" \
    --enable INITRAMFS_COMPRESSION_NONE \
    --disable INITRAMFS_COMPRESSION_GZIP \
    --disable DEBUG_INFO \
    --disable DEBUG_INFO_BTF \
    --disable WERROR

export KBUILD_BUILD_USER=aos
export KBUILD_BUILD_HOST=builder
export KBUILD_BUILD_VERSION=1
export KBUILD_BUILD_TIMESTAMP='Thu Jan  1 00:00:00 UTC 1970'
export SOURCE_DATE_EPOCH=0

make -C "$kernel_source" O="$build_dir/kernel" \
    ARCH=riscv LLVM=1 LLVM_IAS=1 olddefconfig
make -j8 -C "$kernel_source" O="$build_dir/kernel" \
    ARCH=riscv LLVM=1 LLVM_IAS=1 Image

image="$build_dir/kernel/arch/riscv/boot/Image"
actual_image=$(sha256sum "$image" | cut -d ' ' -f 1)
if [ "$record_image" = 0 ] && [ "$actual_image" != "$expected_image" ]; then
    echo "image digest mismatch: expected $expected_image, got $actual_image" >&2
    exit 70
fi
cp "$image" "$output_image"
if [ "$record_image" = 1 ]; then
    printf 'rootfs_cpio_sha256=%s\nimage_sha256=%s\n' "$actual_rootfs" "$actual_image"
fi
printf '%s  %s\n' "$actual_image" "$output_image"
