#!/usr/bin/env bash
set -euo pipefail

realm_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
artifact="$realm_root/dist/aos-linux-realm.capsule"
mode=${1:-package}

if [[ "$mode" != "package" && "$mode" != "--check" ]]; then
  echo "usage: $0 [--check]" >&2
  exit 2
fi
if [[ ! -f "$artifact" ]]; then
  echo "missing capsule artifact: $artifact" >&2
  echo "run 'aos capsule build' from $realm_root first" >&2
  exit 1
fi

required=(
  Capsule.toml
  aos_linux_realm.wasm
  assets/linux-kernel.img
  assets/linux-prewarm-1g-1h.aos-machine
  assets/linux-system.squashfs
  assets/linux-vcpu.lock
  assets/linux-vcpu.wasm
  wit/capsule.wit
)

stage=$(mktemp -d "${TMPDIR:-/tmp}/aos-linux-realm-package.XXXXXX")
next="${artifact}.next"
trap 'rm -rf "$stage"; rm -f "$next"' EXIT

tar -xzf "$artifact" -C "$stage"
if [[ "$mode" == "package" ]]; then
  install -d "$stage/assets"
  for path in "${required[@]}"; do
    if [[ "$path" == assets/* ]]; then
      install -m 0644 "$realm_root/$path" "$stage/$path"
    fi
  done
  tar -czf "$next" -C "$stage" Capsule.toml aos_linux_realm.wasm assets wit
  mv "$next" "$artifact"
fi

listing=$(tar -tf "$artifact")
for path in "${required[@]}"; do
  if [[ ! -f "$stage/$path" ]]; then
    echo "capsule archive is missing $path" >&2
    exit 1
  fi
  case $'\n'"$listing"$'\n' in
    *$'\n'"$path"$'\n'*) ;;
    *)
      echo "capsule archive listing is missing $path" >&2
      exit 1
      ;;
  esac
  if [[ "$path" == assets/* ]] && ! cmp -s "$stage/$path" "$realm_root/$path"; then
    echo "capsule archive contains stale bytes for $path" >&2
    exit 1
  fi
done

bytes=$(wc -c <"$artifact" | tr -d ' ')
digest=$(b3sum "$artifact" | awk '{ print $1 }')
if [[ "$mode" == "--check" ]]; then
  echo "Linux Realm capsule assets verified: ${bytes} bytes, blake3:${digest}"
else
  echo "Packaged Linux Realm capsule assets: ${bytes} bytes, blake3:${digest}"
fi
