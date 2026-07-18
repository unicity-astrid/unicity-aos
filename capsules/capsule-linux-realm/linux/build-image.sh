#!/bin/sh
set -eu

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
    echo "usage: $0 LINUX_SOURCE BUILD_DIR [OUTPUT_IMAGE]" >&2
    exit 64
fi

kernel_source=$1
build_dir=$2
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
output_image=${3:-"$script_dir/Image"}
expected_init=24ba7748ca40285eb5a00e61b8f26e0f0058f7de50a1a203c863a92f74b4e8a5
expected_image=0dd20934e7c1b54484803a9a474a35e931f26dd881b280178d3e4fe937595852

if [ -e "$build_dir" ]; then
    echo "BUILD_DIR must not already exist: $build_dir" >&2
    exit 65
fi
if [ ! -x "$kernel_source/scripts/config" ]; then
    echo "not an extracted Linux source tree: $kernel_source" >&2
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

mkdir -p "$build_dir"
init="$build_dir/aos-init"
initramfs="$build_dir/initramfs.list"

clang --target=riscv64-unknown-linux-gnu \
    -march=rv64ima_zicsr_zifencei -mabi=lp64 \
    -nostdlib -static -fuse-ld=lld \
    -Wl,--build-id=none -Wl,-Ttext=0x10000 \
    -o "$init" "$script_dir/init.S"
actual_init=$(sha256sum "$init" | cut -d ' ' -f 1)
if [ "$actual_init" != "$expected_init" ]; then
    echo "init digest mismatch: expected $expected_init, got $actual_init" >&2
    exit 70
fi
sed "s|@AOS_INIT@|$init|" "$script_dir/initramfs.list" > "$initramfs"

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
    --enable TTY \
    --enable HVC_DRIVER \
    --enable HVC_RISCV_SBI \
    --enable SERIAL_EARLYCON_RISCV_SBI \
    --enable BINFMT_ELF \
    --enable DEVTMPFS \
    --enable DEVTMPFS_MOUNT \
    --enable BLK_DEV_INITRD \
    --set-str INITRAMFS_SOURCE "$initramfs" \
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
if [ "$actual_image" != "$expected_image" ]; then
    echo "image digest mismatch: expected $expected_image, got $actual_image" >&2
    exit 70
fi
cp "$image" "$output_image"
printf '%s  %s\n' "$actual_image" "$output_image"
