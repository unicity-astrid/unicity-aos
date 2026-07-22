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

if [ ! -f "$buildroot_source/Makefile" ] || [ ! -d "$target" ]; then
    echo "assemble-userland requires a completed Buildroot source and output tree" >&2
    exit 66
fi
if [ ! -x "$readelf" ]; then
    echo "assemble-userland requires the completed Buildroot RISC-V toolchain" >&2
    exit 69
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
mkdir -p "$(dirname -- "$output_cpio")"
cp "$cpio" "$output_cpio"
sha256sum "$output_cpio"
