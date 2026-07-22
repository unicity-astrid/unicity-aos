#!/usr/bin/env bash
set -euo pipefail

realm_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
repo_root=$(cd "$realm_root/../.." && pwd)
manifest="$realm_root/crates/realm-vcpu-worker/Cargo.toml"
artifact="$repo_root/target/wasm32-unknown-unknown/release/aos_realm_vcpu_worker.wasm"
installed="$realm_root/assets/linux-vcpu.wasm"
expected_rustc=2972b5e59f1c5529b6ba770437812fd83ab4ebd4
expected_blake3=679e3964f38906522de69d72704e16f17abdfa2e986560512040a8b84381088f
toolchain=nightly-2026-04-04
toolchain_root=$(rustc "+$toolchain" --print sysroot)
cargo_home=${CARGO_HOME:-$HOME/.cargo}
rustflags="--remap-path-prefix=$toolchain_root=/rust/toolchain \
--remap-path-prefix=$cargo_home=/cargo \
--remap-path-prefix=$repo_root=/src/aos-ce \
-C target-feature=+atomics,+bulk-memory,+mutable-globals \
-C link-arg=--import-memory=astrid_compute,memory -C link-arg=--shared-memory \
-C link-arg=--no-stack-first -C link-arg=--global-base=65536 \
-C link-arg=--initial-memory=67108864 \
-C link-arg=--max-memory=3758096384"

actual_rustc=$(rustc "+$toolchain" -vV | awk '/^commit-hash:/ { print $2 }')
if [[ "$actual_rustc" != "$expected_rustc" ]]; then
  echo "Linux vCPU worker requires $toolchain rustc $expected_rustc; found $actual_rustc" >&2
  exit 1
fi

RUSTFLAGS="$rustflags" cargo "+$toolchain" -Zbuild-std=std,panic_abort build \
  --release --target wasm32-unknown-unknown --manifest-path "$manifest"

actual_blake3=$(b3sum "$artifact" | awk '{ print $1 }')
if [[ "$actual_blake3" != "$expected_blake3" ]]; then
  echo "Linux vCPU worker drifted: expected $expected_blake3, got $actual_blake3" >&2
  exit 1
fi

if [[ "${1:-}" == "--check" ]]; then
  cmp "$artifact" "$installed"
  echo "Linux vCPU worker is reproducible: blake3:$actual_blake3"
  exit 0
fi

mkdir -p "$(dirname "$installed")"
install -m 0644 "$artifact" "$installed"
echo "Installed Linux vCPU worker: blake3:$actual_blake3"
