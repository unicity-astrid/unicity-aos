#!/bin/sh
set -eu

if [ "$#" -lt 4 ] || [ "$#" -gt 5 ]; then
    echo "usage: $0 BUILDROOT_SOURCE BUILDROOT_OUTPUT GUEST_TOOLS OUTPUT_CPIO [DOWNLOADS_DIR]" >&2
    exit 64
fi

buildroot_source=$(CDPATH='' cd -- "$1" && pwd)
buildroot_output=$(CDPATH='' cd -- "$2" && pwd)
guest_tools=$(CDPATH='' cd -- "$3" && pwd)
case $4 in
    /*) output_cpio=$4 ;;
    *) output_cpio=$(pwd)/$4 ;;
esac
if [ "$#" -eq 5 ]; then
    downloads_dir=$(CDPATH='' cd -- "$5" && pwd)
else
    downloads_dir=$buildroot_output.downloads
fi
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
target=$buildroot_output/target
readelf=$buildroot_output/host/bin/riscv64-buildroot-linux-gnu-readelf
development_lock=$script_dir/DEVELOPMENT.lock

lock_value() {
    value=$(sed -n "s/^$1=//p" "$development_lock")
    if [ -z "$value" ]; then
        echo "development generation lock has no $1" >&2
        exit 66
    fi
    printf '%s\n' "$value"
}

if [ ! -f "$buildroot_source/Makefile" ] || [ ! -d "$target" ]; then
    echo "assemble-userland requires a completed Buildroot source and output tree" >&2
    exit 66
fi
if [ ! -x "$readelf" ]; then
    echo "assemble-userland requires the completed Buildroot RISC-V toolchain" >&2
    exit 69
fi
if [ ! -f "$development_lock" ] || \
    ! grep -qxF 'format=aos-realm-development-image-1' "$development_lock"; then
    echo "missing or invalid development generation lock: $development_lock" >&2
    exit 66
fi
for binary in astrid-build rustup; do
    if [ ! -x "$guest_tools/$binary" ]; then
        echo "guest tools are missing $binary" >&2
        exit 66
    fi
    if ! "$readelf" -h "$guest_tools/$binary" | grep -q 'Machine:.*RISC-V'; then
        echo "guest tool is not a RISC-V executable: $binary" >&2
        exit 66
    fi
done

for binary in astrid-build rustup; do
    case $binary in
        astrid-build) lock_prefix=astrid_build ;;
        rustup) lock_prefix=rustup ;;
    esac
    expected=$(lock_value "${lock_prefix}_input_sha256")
    actual=$(sha256sum "$guest_tools/$binary" | cut -d ' ' -f 1)
    if [ "$actual" != "$expected" ]; then
        echo "guest-tool digest mismatch for $binary: expected $expected, got $actual" >&2
        exit 66
    fi
done

install -m 0755 "$guest_tools/astrid-build" "$target/usr/bin/astrid-build"
install -m 0755 "$guest_tools/rustup" "$target/usr/bin/rustup"

# Buildroot 2026.05 has no `rootfs-cpio-rebuild` convenience target. Remove
# only its generated CPIO outputs, then invoke the normal filesystem target so
# fakeroot metadata, static devices, post-build checks, and reproducible archive
# ordering all stay authoritative after the separately cross-built tools are
# added. Package and toolchain build products remain untouched.
rm -f \
    "$buildroot_output/images/rootfs.cpio" \
    "$buildroot_output/images/rootfs.cpio.gz"
make -C "$buildroot_source" \
    BR2_EXTERNAL="$script_dir/buildroot" \
    BR2_DL_DIR="$downloads_dir" \
    O="$buildroot_output" \
    rootfs-cpio

cpio=$buildroot_output/images/rootfs.cpio.gz
if [ ! -f "$cpio" ]; then
    echo "Buildroot did not regenerate rootfs.cpio.gz" >&2
    exit 70
fi
for executable in usr/bin/astrid-build usr/bin/rustup; do
    if [ ! -x "$target/$executable" ]; then
        echo "assembled rootfs is missing /$executable" >&2
        exit 70
    fi
done
for binary in astrid-build rustup; do
    case $binary in
        astrid-build) lock_prefix=astrid_build ;;
        rustup) lock_prefix=rustup ;;
    esac
    expected=$(lock_value "${lock_prefix}_shipped_sha256")
    actual=$(sha256sum "$target/usr/bin/$binary" | cut -d ' ' -f 1)
    if [ "$actual" != "$expected" ]; then
        echo "shipped guest-tool digest mismatch for $binary: expected $expected, got $actual" >&2
        exit 70
    fi
done
expected=$(lock_value rust_lld_shipped_sha256)
actual=$(sha256sum \
    "$target/usr/lib/rustlib/riscv64gc-unknown-linux-gnu/bin/rust-lld" |
    cut -d ' ' -f 1)
if [ "$actual" != "$expected" ]; then
    echo "shipped rust-lld digest mismatch: expected $expected, got $actual" >&2
    exit 70
fi
expected_cpio=$(lock_value rootfs_cpio_sha256)
actual_cpio=$(sha256sum "$cpio" | cut -d ' ' -f 1)
if [ "$actual_cpio" != "$expected_cpio" ]; then
    echo "final rootfs digest mismatch: expected $expected_cpio, got $actual_cpio" >&2
    exit 70
fi
mkdir -p "$(dirname -- "$output_cpio")"
cp "$cpio" "$output_cpio"
sha256sum "$output_cpio"
