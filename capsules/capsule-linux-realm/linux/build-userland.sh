#!/bin/sh
set -eu

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
    echo "usage: $0 BUILDROOT_SOURCE BUILD_DIR [OUTPUT_CPIO]" >&2
    exit 64
fi

caller_dir=$(pwd)
buildroot_source=$(CDPATH='' cd -- "$1" && pwd)
case $2 in
    /*) build_dir=$2 ;;
    *) build_dir=$caller_dir/$2 ;;
esac
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
if [ "$#" -eq 3 ]; then
    case $3 in
        /*) output_cpio=$3 ;;
        *) output_cpio=$caller_dir/$3 ;;
    esac
else
    output_cpio=$script_dir/rootfs.cpio
fi
expected_cpio=0877c008ec43627d9aeefc30c4e09412ceeb6aefb25b799b2365d002261b891b
record_userland=${AOS_RECORD_USERLAND:-0}
downloads_dir=${BR2_DL_DIR:-"$build_dir.downloads"}

if [ "$record_userland" != 0 ] && [ "$record_userland" != 1 ]; then
    echo "AOS_RECORD_USERLAND must be 0 or 1" >&2
    exit 64
fi
if [ -e "$build_dir" ]; then
    echo "BUILD_DIR must not already exist: $build_dir" >&2
    exit 65
fi
if [ ! -f "$buildroot_source/Makefile" ]; then
    echo "not an extracted Buildroot source tree: $buildroot_source" >&2
    exit 66
fi
version=$(sed -n 's/^export BR2_VERSION := //p' "$buildroot_source/Makefile")
if [ "$version" != 2026.05.1 ]; then
    echo "build-userland.sh requires exact Buildroot 2026.05.1 sources" >&2
    exit 66
fi
mkdir -p "$downloads_dir"

make -C "$buildroot_source" \
    BR2_EXTERNAL="$script_dir/buildroot" \
    BR2_DL_DIR="$downloads_dir" \
    O="$build_dir" aos_rv64_defconfig

config=$build_dir/.config
for required in \
    'BR2_riscv=y' \
    'BR2_RISCV_ISA_RVI=y' \
    'BR2_RISCV_ISA_RVM=y' \
    'BR2_RISCV_ISA_RVA=y' \
    '# BR2_RISCV_ISA_RVF is not set' \
    '# BR2_RISCV_ISA_RVC is not set' \
    '# BR2_RISCV_ISA_RVV is not set' \
    'BR2_RISCV_64=y' \
    'BR2_RISCV_ABI_LP64=y' \
    'BR2_KERNEL_HEADERS_6_18=y' \
    'BR2_DEFAULT_KERNEL_HEADERS="6.18.34"' \
    'BR2_TOOLCHAIN_BUILDROOT_MUSL=y' \
    'BR2_STATIC_LIBS=y' \
    'BR2_REPRODUCIBLE=y' \
    'BR2_DOWNLOAD_FORCE_CHECK_HASHES=y' \
    'BR2_PRIMARY_SITE="https://sources.buildroot.net"' \
    'BR2_PACKAGE_BUSYBOX=y' \
    'BR2_ROOTFS_DEVICE_CREATION_STATIC=y' \
    "BR2_ROOTFS_STATIC_DEVICE_TABLE=\"\$(BR2_EXTERNAL_AOS_PATH)/board/aos/device-table.txt\"" \
    '# BR2_TARGET_ENABLE_ROOT_LOGIN is not set' \
    'BR2_TARGET_ROOTFS_CPIO=y'
do
    if ! grep -qxF "$required" "$config"; then
        echo "generated Buildroot config is missing: $required" >&2
        exit 70
    fi
done
make -j8 -C "$buildroot_source" \
    BR2_EXTERNAL="$script_dir/buildroot" \
    BR2_DL_DIR="$downloads_dir" \
    O="$build_dir"

cpio="$build_dir/images/rootfs.cpio"
if [ ! -f "$cpio" ]; then
    echo "Buildroot did not produce rootfs.cpio" >&2
    exit 70
fi
actual_cpio=$(sha256sum "$cpio" | cut -d ' ' -f 1)
if [ "$record_userland" = 0 ] && [ "$actual_cpio" != "$expected_cpio" ]; then
    echo "rootfs digest mismatch: expected $expected_cpio, got $actual_cpio" >&2
    exit 70
fi
cp "$cpio" "$output_cpio"
if [ "$record_userland" = 1 ]; then
    printf 'rootfs_cpio_sha256=%s\n' "$actual_cpio"
fi
printf '%s  %s\n' "$actual_cpio" "$output_cpio"
