#!/bin/sh
set -eu

if [ "$#" -lt 1 ]; then
    echo "usage: $0 TARGET_DIR" >&2
    exit 64
fi

target_dir=$1
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
cc="$HOST_DIR/bin/riscv64-buildroot-linux-musl-gcc"
strip="$HOST_DIR/bin/riscv64-buildroot-linux-musl-strip"

if [ ! -x "$cc" ] || [ ! -x "$strip" ]; then
    echo "AOS post-build requires the Buildroot RV64 musl toolchain" >&2
    exit 69
fi

"$cc" \
    -std=c11 -Os -Wall -Wextra -Werror \
    -march=rv64ima_zicsr_zifencei -mabi=lp64 \
    -static -fno-pie -no-pie \
    -Wl,--build-id=none \
    -o "$target_dir/init" "$script_dir/init.c"
"$strip" --strip-all "$target_dir/init"
mkdir -p "$target_dir/home/agent" "$target_dir/workspace" "$target_dir/tmp"
chmod 0755 "$target_dir/init"
chmod 0700 "$target_dir/home/agent"
chmod 0700 "$target_dir/workspace"
chmod 1777 "$target_dir/tmp"
