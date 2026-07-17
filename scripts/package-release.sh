#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 6 ]]; then
  echo "usage: $0 <target> <aos-binary> <runtime-archive> <runtime-blake3> <capsule-artifacts> <output-dir>" >&2
  exit 2
fi

target=$1
aos_binary=$2
runtime_archive=$3
runtime_blake3=$4
capsule_artifacts=$5
output_dir=$6

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
toml_value() {
  python3 - "$1" "$2" "$3" <<'PY'
import pathlib
import sys
try:
    import tomllib
except ModuleNotFoundError:  # Python 3.10 and older
    import tomli as tomllib

path, section, key = sys.argv[1:]
value = tomllib.loads(pathlib.Path(path).read_text(encoding="utf-8"))[section][key]
if not isinstance(value, str) or not value:
    raise SystemExit(f"{path}: [{section}] {key} must be a non-empty string")
print(value)
PY
}

product_version=$(toml_value "$repo_root/crates/unicity-aos-bootstrap/Cargo.toml" package version)
runtime_version=$(toml_value "$repo_root/release/runtime-compatibility.toml" runtime version)
runtime_tag=$(toml_value "$repo_root/release/runtime-compatibility.toml" runtime tag)
runtime_repository=$(toml_value "$repo_root/release/runtime-compatibility.toml" runtime repository)
runtime_identity=$(toml_value "$repo_root/release/runtime-compatibility.toml" runtime release-workflow-identity)
wit_repository=$(toml_value "$repo_root/release/runtime-compatibility.toml" contracts repository)
wit_commit=$(toml_value "$repo_root/release/runtime-compatibility.toml" contracts commit)
sdk_rust_version=$(toml_value "$repo_root/release/runtime-compatibility.toml" contracts sdk-rust-version)
sdk_rust_commit=$(toml_value "$repo_root/release/runtime-compatibility.toml" contracts sdk-rust-commit)
asset="unicity-aos-${product_version}-${target}.tar.gz"
root="unicity-aos-${product_version}-${target}"

if [[ -z "$product_version" || -z "$runtime_version" || -z "$runtime_tag" || -z "$runtime_repository" || -z "$runtime_identity" || -z "$wit_repository" || -z "$wit_commit" || -z "$sdk_rust_version" || -z "$sdk_rust_commit" ]]; then
  echo "release compatibility metadata is incomplete" >&2
  exit 1
fi
if [[ ! -x "$aos_binary" ]]; then
  echo "AOS binary is missing or not executable: $aos_binary" >&2
  exit 1
fi
if [[ ! -f "$runtime_archive" ]]; then
  echo "runtime archive is missing: $runtime_archive" >&2
  exit 1
fi
if [[ ! "$runtime_blake3" =~ ^[0-9a-f]{64}$ ]]; then
  echo "runtime BLAKE3 digest is malformed" >&2
  exit 1
fi
python3 "$repo_root/scripts/capsule_release.py" --artifacts "$capsule_artifacts"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p \
  "$work/runtime-extract" \
  "$work/$root/bin" \
  "$work/$root/libexec" \
  "$work/$root/runtime/bin" \
  "$work/$root/capsules" \
  "$output_dir"

python3 "$repo_root/scripts/validate-runtime-archive.py" \
  "$runtime_archive" \
  "astrid-${runtime_version}-${target}" \
  astrid astrid-daemon astrid-build astrid-emit
tar -xzf "$runtime_archive" -C "$work/runtime-extract"

runtime_root="$work/runtime-extract/astrid-${runtime_version}-${target}"
if [[ ! -d "$runtime_root" ]]; then
  echo "runtime archive has no expected root astrid-${runtime_version}-${target}" >&2
  exit 1
fi

install -m 0755 "$aos_binary" "$work/$root/bin/aos"
install -m 0644 "$repo_root/install.sh" "$work/$root/libexec/install.sh"
for binary in astrid astrid-daemon astrid-build astrid-emit; do
  if [[ ! -x "$runtime_root/$binary" ]]; then
    echo "runtime archive is missing $binary" >&2
    exit 1
  fi
  install -m 0755 "$runtime_root/$binary" "$work/$root/runtime/bin/$binary"
done

python3 "$repo_root/scripts/capsule_release.py" --print-assets > "$work/$root/capsule-assets.txt"
while IFS= read -r capsule; do
  [[ "$capsule" =~ ^aos-[a-z0-9-]+\.capsule$ ]]
  install -m 0644 "$capsule_artifacts/$capsule" "$work/$root/capsules/$capsule"
done < "$work/$root/capsule-assets.txt"
python3 "$repo_root/scripts/capsule_release.py" --artifacts "$work/$root/capsules"

install -m 0644 "$repo_root/release/runtime-compatibility.toml" "$work/$root/runtime-compatibility.toml"
install -m 0644 "$repo_root/distros/community/unicity-ce/Distro.toml" "$work/$root/Distro.toml"
install -m 0644 "$repo_root/README.md" "$work/$root/README.md"

python3 - "$work/$root/release-manifest.json" "$work/$root/capsule-assets.txt" "$product_version" "$target" "$runtime_repository" "$runtime_version" "$runtime_tag" "$runtime_blake3" "$runtime_identity" "$wit_repository" "$wit_commit" "$sdk_rust_version" "$sdk_rust_commit" <<'PY'
import json
import pathlib
import sys

path, capsule_list, product, target, runtime_repo, runtime, tag, digest, runtime_identity, wit_repo, wit_commit, sdk_version, sdk_commit = sys.argv[1:]
capsules = pathlib.Path(capsule_list).read_text(encoding="utf-8").splitlines()
manifest = {
    "schema_version": 2,
    "product": {"name": "Unicity AOS Community Edition", "version": product},
    "target": target,
    "runtime": {
        "repository": runtime_repo,
        "version": runtime,
        "tag": tag,
        "asset": f"astrid-{runtime}-{target}.tar.gz",
        "digest": f"blake3:{digest}",
        "release_workflow_identity": runtime_identity,
    },
    "contracts": {
        "repository": wit_repo,
        "commit": wit_commit,
        "sdk_rust_version": sdk_version,
        "sdk_rust_commit": sdk_commit,
    },
    "capsules": {"count": len(capsules), "assets": capsules},
}
pathlib.Path(path).write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
PY

tar -czf "$output_dir/$asset" -C "$work" "$root"
echo "$output_dir/$asset"
