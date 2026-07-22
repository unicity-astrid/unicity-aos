#!/bin/sh
set -eu

if [ "$#" -lt 5 ] || [ "$#" -gt 6 ]; then
    echo "usage: $0 HOST_RUST_ROOT BUILDROOT_OUTPUT ASTRID_BUILD_SOURCE RUSTUP_SOURCE BUILD_DIR [OUTPUT_DIR]" >&2
    exit 64
fi

caller_dir=$(pwd)
host_rust=$(CDPATH='' cd -- "$1" && pwd)
buildroot_output=$(CDPATH='' cd -- "$2" && pwd)
astrid_build_source=$(CDPATH='' cd -- "$3" && pwd)
rustup_source=$(CDPATH='' cd -- "$4" && pwd)
case $5 in
    /*) build_dir=$5 ;;
    *) build_dir=$caller_dir/$5 ;;
esac
if [ "$#" -eq 6 ]; then
    case $6 in
        /*) output_dir=$6 ;;
        *) output_dir=$caller_dir/$6 ;;
    esac
else
    output_dir=$caller_dir/guest-tools
fi

target=riscv64gc-unknown-linux-gnu
buildroot_tuple=riscv64-buildroot-linux-gnu
cargo="$host_rust/bin/cargo"
rustc="$host_rust/bin/rustc"
linker="$buildroot_output/host/bin/$buildroot_tuple-gcc"
strip="$buildroot_output/host/bin/$buildroot_tuple-strip"

if [ -e "$build_dir" ]; then
    echo "BUILD_DIR must not already exist: $build_dir" >&2
    exit 65
fi
if [ ! -x "$cargo" ] || [ ! -x "$rustc" ] || \
    [ ! -x "$linker" ] || [ ! -x "$strip" ]; then
    echo "guest-tools build requires Rust plus the completed Buildroot GNU toolchain" >&2
    exit 69
fi
if [ "$($rustc --version)" != 'rustc 1.97.1 (8bab26f4f 2026-07-14)' ]; then
    echo "guest-tools build requires exact Rust 1.97.1" >&2
    exit 69
fi
if ! grep -qxF 'version = "0.10.4"' "$astrid_build_source/Cargo.toml" || \
    [ ! -f "$astrid_build_source/Cargo.lock" ]; then
    echo "guest-tools build requires published astrid-build 0.10.4 source and lockfile" >&2
    exit 66
fi
if ! grep -qxF 'version = "1.29.0"' "$rustup_source/Cargo.toml" || \
    [ ! -f "$rustup_source/Cargo.lock" ]; then
    echo "guest-tools build requires rustup 1.29.0 source and lockfile" >&2
    exit 66
fi

mkdir -p "$build_dir" "$output_dir"
export PATH="$host_rust/bin:$buildroot_output/host/bin:$PATH"
export CARGO_TARGET_RISCV64GC_UNKNOWN_LINUX_GNU_LINKER="$linker"
export CC_riscv64gc_unknown_linux_gnu="$linker"
export CXX_riscv64gc_unknown_linux_gnu="$buildroot_output/host/bin/$buildroot_tuple-g++"
export AR_riscv64gc_unknown_linux_gnu="$buildroot_output/host/bin/$buildroot_tuple-ar"
export SOURCE_DATE_EPOCH=0
export RUSTFLAGS="--remap-path-prefix=$astrid_build_source=/usr/src/astrid-build --remap-path-prefix=$rustup_source=/usr/src/rustup --remap-path-prefix=$build_dir=/usr/src/aos-guest-tools"

CARGO_TARGET_DIR="$build_dir/astrid-build" "$cargo" build \
    --manifest-path "$astrid_build_source/Cargo.toml" \
    --locked --release --target "$target"
CARGO_TARGET_DIR="$build_dir/rustup" "$cargo" build \
    --manifest-path "$rustup_source/Cargo.toml" \
    --locked --release --target "$target" \
    --no-default-features --features reqwest-rustls-tls

install -m 0755 \
    "$build_dir/astrid-build/$target/release/astrid-build" \
    "$output_dir/astrid-build"
install -m 0755 \
    "$build_dir/rustup/$target/release/rustup-init" \
    "$output_dir/rustup"
"$strip" --strip-all "$output_dir/astrid-build" "$output_dir/rustup"

for binary in astrid-build rustup; do
    "$buildroot_output/host/bin/$buildroot_tuple-readelf" -h \
        "$output_dir/$binary" | grep -q 'Machine:.*RISC-V'
    if LC_ALL=C grep -aF -q "$build_dir" "$output_dir/$binary" || \
        LC_ALL=C grep -aF -q "$astrid_build_source" "$output_dir/$binary" || \
        LC_ALL=C grep -aF -q "$rustup_source" "$output_dir/$binary"; then
        echo "$binary retains a builder path" >&2
        exit 70
    fi
    sha256sum "$output_dir/$binary"
done
