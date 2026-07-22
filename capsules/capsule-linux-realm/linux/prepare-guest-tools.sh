#!/bin/sh
set -eu

if [ "$#" -ne 6 ]; then
    echo "usage: $0 HOST_RUST_ARCHIVE RISCV_STD_ARCHIVE ASTRID_BUILD_CRATE RUSTUP_ARCHIVE WORK_DIR OUTPUT_DIR" >&2
    exit 64
fi

host_archive=$(CDPATH='' cd -- "$(dirname -- "$1")" && pwd)/$(basename -- "$1")
std_archive=$(CDPATH='' cd -- "$(dirname -- "$2")" && pwd)/$(basename -- "$2")
astrid_archive=$(CDPATH='' cd -- "$(dirname -- "$3")" && pwd)/$(basename -- "$3")
rustup_archive=$(CDPATH='' cd -- "$(dirname -- "$4")" && pwd)/$(basename -- "$4")
case $5 in
    /*) work_dir=$5 ;;
    *) work_dir=$(pwd)/$5 ;;
esac
case $6 in
    /*) output_dir=$6 ;;
    *) output_dir=$(pwd)/$6 ;;
esac

if [ -e "$work_dir" ] || [ -e "$output_dir" ]; then
    echo "WORK_DIR and OUTPUT_DIR must not already exist" >&2
    exit 65
fi
for archive in "$host_archive" "$std_archive" "$astrid_archive" "$rustup_archive"; do
    if [ ! -f "$archive" ]; then
        echo "missing guest-tools input: $archive" >&2
        exit 66
    fi
done

verify_sha256() {
    expected=$1
    archive=$2
    actual=$(sha256sum "$archive" | cut -d ' ' -f 1)
    if [ "$actual" != "$expected" ]; then
        echo "digest mismatch for $archive: expected $expected, got $actual" >&2
        exit 66
    fi
}

verify_sha256 9a7a2c336b4787f1b72f6bab7c35d5b7af2fd03cbd39b4fc721466a70d402a7d "$host_archive"
verify_sha256 5cec88477a70fe83c10a12efd6bdad1b0f8dd4c1ebba1e427f00c69666c5b6f9 "$std_archive"
verify_sha256 969760ea346229dccdae9e2824b3a81a23c106dd09b2927243982a0be134bb89 "$astrid_archive"
verify_sha256 de73d1a62f4d5409a2f6bdb1c523d8dc08aa6d9d63588db62493c19ca8f8bf55 "$rustup_archive"

mkdir -p "$work_dir/host" "$work_dir/std" "$output_dir/sources"
tar -xf "$host_archive" -C "$work_dir/host"
tar -xf "$std_archive" -C "$work_dir/std"
tar -xf "$astrid_archive" -C "$output_dir/sources"
tar -xf "$rustup_archive" -C "$output_dir/sources"

host_source=$work_dir/host/rust-1.97.1-aarch64-unknown-linux-gnu
std_source=$work_dir/std/rust-std-1.97.1-riscv64gc-unknown-linux-gnu
host_rust=$output_dir/host-rust
if [ ! -x "$host_source/install.sh" ] || [ ! -x "$std_source/install.sh" ]; then
    echo "official Rust archives have an unexpected layout" >&2
    exit 66
fi

(cd "$host_source" && ./install.sh \
    --prefix="$host_rust" \
    --disable-ldconfig \
    --components=rustc,cargo,rust-std-aarch64-unknown-linux-gnu)
(cd "$std_source" && ./install.sh \
    --prefix="$host_rust" \
    --disable-ldconfig \
    --components=rust-std-riscv64gc-unknown-linux-gnu)

if [ "$("$host_rust/bin/rustc" --version)" != \
    'rustc 1.97.1 (8bab26f4f 2026-07-14)' ]; then
    echo "prepared host Rust has the wrong version" >&2
    exit 70
fi
if [ ! -d "$host_rust/lib/rustlib/riscv64gc-unknown-linux-gnu/lib" ]; then
    echo "prepared host Rust is missing the RISC-V standard library" >&2
    exit 70
fi
if [ ! -f "$output_dir/sources/astrid-build-0.10.4/Cargo.lock" ] || \
    [ ! -f "$output_dir/sources/rustup-1.29.0/Cargo.lock" ]; then
    echo "prepared guest-tool sources are incomplete" >&2
    exit 70
fi

printf 'host_rust=%s\n' "$host_rust"
printf 'astrid_build_source=%s\n' "$output_dir/sources/astrid-build-0.10.4"
printf 'rustup_source=%s\n' "$output_dir/sources/rustup-1.29.0"
