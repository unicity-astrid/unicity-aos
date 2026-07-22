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
    output_cpio=$script_dir/rootfs.cpio.gz
fi
expected_cpio=10d26184e85add731208050fb3da9fed5e1dda7475b6e66e0d9814a221ecf3f4
record_userland=${AOS_RECORD_USERLAND:-0}
# LLVM translation units can individually consume several GiB. Keep the
# reproducible default safe for an 8 GiB builder; operators with a larger
# envelope may raise this explicitly.
build_jobs=${AOS_BUILD_JOBS:-1}
downloads_dir=${BR2_DL_DIR:-"$build_dir.downloads"}

if [ "$record_userland" != 0 ] && [ "$record_userland" != 1 ]; then
    echo "AOS_RECORD_USERLAND must be 0 or 1" >&2
    exit 64
fi
case $build_jobs in
    ''|*[!0-9]*|0)
        echo "AOS_BUILD_JOBS must be a positive integer" >&2
        exit 64
        ;;
esac
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
"$buildroot_source/utils/config" --file "$config" \
    --set-val BR2_JLEVEL "$build_jobs"
make -C "$buildroot_source" \
    BR2_EXTERNAL="$script_dir/buildroot" \
    BR2_DL_DIR="$downloads_dir" \
    O="$build_dir" olddefconfig

for required in \
    'BR2_riscv=y' \
    'BR2_RISCV_ISA_RVI=y' \
    'BR2_RISCV_ISA_RVM=y' \
    'BR2_RISCV_ISA_RVA=y' \
    'BR2_RISCV_ISA_RVF=y' \
    'BR2_RISCV_ISA_RVD=y' \
    'BR2_RISCV_ISA_RVC=y' \
    '# BR2_RISCV_ISA_RVV is not set' \
    'BR2_RISCV_64=y' \
    'BR2_RISCV_ABI_LP64D=y' \
    'BR2_KERNEL_HEADERS_6_18=y' \
    'BR2_DEFAULT_KERNEL_HEADERS="6.18.34"' \
    'BR2_TOOLCHAIN_BUILDROOT_GLIBC=y' \
    'BR2_TOOLCHAIN_BUILDROOT_CXX=y' \
    'BR2_SHARED_LIBS=y' \
    'BR2_REPRODUCIBLE=y' \
    'BR2_DOWNLOAD_FORCE_CHECK_HASHES=y' \
    "BR2_JLEVEL=$build_jobs" \
    'BR2_PRIMARY_SITE="https://sources.buildroot.net"' \
    'BR2_PACKAGE_BUSYBOX=y' \
    'BR2_PACKAGE_BUSYBOX_SHOW_OTHERS=y' \
    'BR2_PACKAGE_BASH=y' \
    'BR2_PACKAGE_BINUTILS_TARGET=y' \
    'BR2_PACKAGE_CA_CERTIFICATES=y' \
    'BR2_PACKAGE_CLANG=y' \
    'BR2_PACKAGE_CMAKE_CTEST=y' \
    'BR2_PACKAGE_GIT=y' \
    'BR2_PACKAGE_MAKE=y' \
    'BR2_PACKAGE_AOS_NINJA=y' \
    'BR2_PACKAGE_PATCH=y' \
    'BR2_PACKAGE_PKGCONF=y' \
    'BR2_PACKAGE_PYTHON3=y' \
    'BR2_PACKAGE_PYTHON3_PY_PYC=y' \
    'BR2_PACKAGE_STRACE=y' \
    'BR2_PACKAGE_AOS_RUST_TOOLCHAIN=y' \
    'BR2_ROOTFS_DEVICE_CREATION_STATIC=y' \
    "BR2_ROOTFS_STATIC_DEVICE_TABLE=\"\$(BR2_EXTERNAL_AOS_PATH)/board/aos/device-table.txt\"" \
    '# BR2_TARGET_ENABLE_ROOT_LOGIN is not set' \
    'BR2_TARGET_ROOTFS_CPIO=y' \
    'BR2_TARGET_ROOTFS_CPIO_GZIP=y'
do
    if ! grep -qxF "$required" "$config"; then
        echo "generated Buildroot config is missing: $required" >&2
        exit 70
    fi
done
make -j"$build_jobs" -C "$buildroot_source" \
    BR2_EXTERNAL="$script_dir/buildroot" \
    BR2_DL_DIR="$downloads_dir" \
    O="$build_dir"

target=$build_dir/target
for executable in \
    bin/bash \
    usr/bin/ar \
    usr/bin/c++ \
    usr/bin/cc \
    usr/bin/clang++ \
    usr/bin/clang-22 \
    usr/bin/cmake \
    usr/bin/ctest \
    usr/bin/git \
    usr/bin/make \
    usr/bin/ninja \
    usr/bin/patch \
    usr/bin/pkg-config \
    usr/bin/python3 \
    usr/bin/readelf \
    usr/bin/rustc \
    usr/bin/cargo \
    usr/bin/rustfmt \
    usr/bin/strace
do
    if [ ! -x "$target/$executable" ]; then
        echo "development rootfs is missing executable: /$executable" >&2
        exit 70
    fi
done
for required_file in \
    usr/include/stdio.h \
    usr/include/c++/14.4.0/vector \
    usr/lib/crt1.o \
    usr/lib/libc.a \
    lib/libc.so \
    lib/libgcc_s.so \
    lib/libatomic.a \
    usr/lib/libm.a \
    usr/lib/clang/22/include/stddef.h \
    usr/lib/gcc/riscv64-buildroot-linux-gnu/14.4.0/libgcc.a \
    usr/lib/libstdc++.so \
    usr/libexec/aos/clang-22 \
    usr/lib/os-release \
    usr/share/cmake-4.3/Modules/CMakeDetermineSystem.cmake \
    etc/ssl/certs/ca-certificates.crt
do
    if [ ! -f "$target/$required_file" ]; then
        echo "development rootfs is missing file: /$required_file" >&2
        exit 70
    fi
done
if [ ! -f "$target/lib/ld-linux-riscv64-lp64d.so.1" ]; then
    echo "development rootfs is missing the RISC-V LP64D glibc loader" >&2
    exit 70
fi
if [ ! -d "$target/usr/lib/rustlib/wasm32-unknown-unknown/lib" ]; then
    echo "development rootfs is missing the wasm32-unknown-unknown Rust standard library" >&2
    exit 70
fi
if ! grep -qxF 'NAME="AOS Realm"' "$target/usr/lib/os-release"; then
    echo "development rootfs has the wrong operating-system identity" >&2
    exit 70
fi
readelf="$build_dir/host/bin/riscv64-buildroot-linux-gnu-readelf"
if [ ! -x "$readelf" ]; then
    echo "development build is missing the target readelf verifier" >&2
    exit 70
fi
if ! "$readelf" -h "$target/usr/libexec/aos/clang-22" |
    grep -q 'Machine:.*RISC-V'; then
    echo "development Clang is not a RISC-V target executable" >&2
    exit 70
fi
if "$readelf" -d "$target/usr/libexec/aos/clang-22" |
    grep -Eq '/build|rootfs-out|RPATH|RUNPATH'; then
    echo "development Clang retains a build-time dynamic-loader path" >&2
    exit 70
fi
if "$readelf" -d "$target/usr/bin/cmake" |
    grep -Eq '/build|rootfs-out|RPATH|RUNPATH'; then
    echo "development CMake retains a build-time dynamic-loader path" >&2
    exit 70
fi
for compiler_flag in \
    '--target=riscv64-buildroot-linux-gnu' \
    '--sysroot=/' \
    '--gcc-install-dir=/usr/lib/gcc/riscv64-buildroot-linux-gnu/14.4.0' \
    '-resource-dir=/usr/lib/clang/22' \
    '-march=rv64gc_zicsr_zifencei' \
    '-mabi=lp64d'
do
    if ! grep -qF -- "$compiler_flag" "$target/usr/bin/clang"; then
        echo "development compiler wrapper is missing: $compiler_flag" >&2
        exit 70
    fi
done
privileged=$(find "$target" -xdev \( -perm -4000 -o -perm -2000 \) -print -quit)
if [ -n "$privileged" ]; then
    echo "development rootfs contains a set-ID path: $privileged" >&2
    exit 70
fi
leaked_path=$(LC_ALL=C grep -RIl -F "$build_dir" "$target" 2>/dev/null |
    head -n 1 || true)
if [ -n "$leaked_path" ]; then
    echo "development rootfs retains a builder path: $leaked_path" >&2
    exit 70
fi

cpio="$build_dir/images/rootfs.cpio.gz"
if [ ! -f "$cpio" ]; then
    echo "Buildroot did not produce rootfs.cpio.gz" >&2
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
