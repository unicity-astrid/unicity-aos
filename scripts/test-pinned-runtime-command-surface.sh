#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
contract="$repo_root/release/runtime-command-surface.toml"

read -r runtime_version runtime_tag runtime_repository runtime_identity < <(
  python3 - "$repo_root/release/runtime-compatibility.toml" <<'PY'
import pathlib
import sys
import tomllib

with pathlib.Path(sys.argv[1]).open("rb") as file:
    runtime = tomllib.load(file)["runtime"]
print(
    runtime["version"],
    runtime["tag"],
    runtime["repository"],
    runtime["release-workflow-identity"],
)
PY
)

if [[ -n "${AOS_TEST_RUNTIME_BINARY:-}" ]]; then
  runtime_binary=$AOS_TEST_RUNTIME_BINARY
  [[ -x "$runtime_binary" ]] || {
    echo "test runtime binary is not executable: $runtime_binary" >&2
    exit 1
  }
  python3 "$repo_root/scripts/validate-runtime-command-surface.py" \
    "$runtime_binary" "$contract" --runtime-version "$runtime_version"
  exit
fi

for command in curl b3sum cosign tar; do
  command -v "$command" >/dev/null || {
    echo "$command is required to validate the pinned runtime command surface" >&2
    exit 1
  }
done

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
case "$(uname -s):$(uname -m)" in
  Linux:x86_64) target=x86_64-unknown-linux-gnu ;;
  Linux:aarch64 | Linux:arm64) target=aarch64-unknown-linux-gnu ;;
  Darwin:x86_64) target=x86_64-apple-darwin ;;
  Darwin:aarch64 | Darwin:arm64) target=aarch64-apple-darwin ;;
  *)
    echo "unsupported host for runtime command validation: $(uname -s) $(uname -m)" >&2
    exit 1
    ;;
esac
asset="astrid-${runtime_version}-${target}.tar.gz"
base="https://github.com/${runtime_repository}/releases/download/${runtime_tag}"

curl --proto '=https' --tlsv1.2 -fsSLo "$work/$asset" "$base/$asset"
curl --proto '=https' --tlsv1.2 -fsSLo "$work/BLAKE3SUMS.txt" "$base/BLAKE3SUMS.txt"
curl --proto '=https' --tlsv1.2 -fsSLo "$work/$asset.sigstore.json" \
  "$base/$asset.sigstore.json"
expected=$(awk -v asset="$asset" '$2 == asset { print $1; exit }' "$work/BLAKE3SUMS.txt")
[[ "$expected" =~ ^[0-9a-f]{64}$ ]]
[[ "$(b3sum -- "$work/$asset" | awk '{print $1}')" == "$expected" ]]
cosign verify-blob \
  --bundle "$work/$asset.sigstore.json" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-identity "$runtime_identity" \
  "$work/$asset" >/dev/null

root="astrid-${runtime_version}-${target}"
python3 "$repo_root/scripts/validate-runtime-archive.py" "$work/$asset" "$root" astrid
tar -xzf "$work/$asset" -C "$work"
python3 "$repo_root/scripts/validate-runtime-command-surface.py" \
  "$work/$root/astrid" "$contract" --runtime-version "$runtime_version"
