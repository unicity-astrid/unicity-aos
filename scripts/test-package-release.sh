#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
python3 "$repo_root/scripts/validate-release-contract.py"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
target=x86_64-unknown-linux-gnu
read -r product_version runtime_version runtime_identity < <(
  python3 - "$repo_root" <<'PY'
import pathlib
import sys
import tomllib

root = pathlib.Path(sys.argv[1])
with (root / "crates/unicity-aos-bootstrap/Cargo.toml").open("rb") as file:
    product = tomllib.load(file)["package"]["version"]
with (root / "release/runtime-compatibility.toml").open("rb") as file:
    runtime = tomllib.load(file)["runtime"]
print(product, runtime["version"], runtime["release-workflow-identity"])
PY
)
runtime_root="$work/astrid-$runtime_version-$target"
mkdir -p "$runtime_root" "$work/output"
mkdir -p "$work/capsules"

PYTHONPATH="$repo_root/scripts" python3 - "$work/capsules" <<'PY'
import pathlib
import sys

from capsule_release import source_contract
from test_capsule_release import write_fixture

output = pathlib.Path(sys.argv[1])
for spec in source_contract():
    write_fixture(output / spec.asset, spec)
PY

for binary in astrid astrid-daemon astrid-build astrid-emit; do
  printf '#!/bin/sh\nexit 0\n' > "$runtime_root/$binary"
  chmod 755 "$runtime_root/$binary"
done
COPYFILE_DISABLE=1 tar -czf "$work/runtime.tar.gz" -C "$work" "$(basename "$runtime_root")"
printf '#!/bin/sh\nexit 0\n' > "$work/aos"
chmod 755 "$work/aos"

if bash "$repo_root/scripts/package-release.sh" \
  "$target" \
  "$work/aos" \
  "$work/runtime.tar.gz" \
  AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA \
  "$work/capsules" \
  "$work/output" >/dev/null 2>&1; then
  echo "release composer accepted a non-canonical BLAKE3 digest" >&2
  exit 1
fi

bash "$repo_root/scripts/package-release.sh" \
  "$target" \
  "$work/aos" \
  "$work/runtime.tar.gz" \
  0000000000000000000000000000000000000000000000000000000000000000 \
  "$work/capsules" \
  "$work/output"

archive="$work/output/unicity-aos-$product_version-$target.tar.gz"
test -f "$archive"
tar -tzf "$archive" > "$work/files"
grep -q '/bin/aos$' "$work/files"
grep -q '/libexec/install.sh$' "$work/files"
grep -q '/runtime/bin/astrid-daemon$' "$work/files"
test "$(grep -c '/capsules/aos-.*\.capsule$' "$work/files")" -eq 21
grep -q '/capsule-assets.txt$' "$work/files"
grep -q '/Distro.toml$' "$work/files"
grep -q '/release-manifest.json$' "$work/files"

tar -xzf "$archive" -C "$work"
manifest=$(find "$work" -path '*/release-manifest.json' -print -quit)
bundle_root=$(dirname "$manifest")
test "$(grep -c '^source = "capsules/aos-.*\.capsule"$' "$bundle_root/Distro.toml")" -eq 21
if grep -F '@unicity-aos/capsule-' "$bundle_root/Distro.toml" >/dev/null; then
  echo "release archive retained a legacy capsule repository source" >&2
  exit 1
fi
python3 - "$manifest" "$product_version" "$runtime_version" "$runtime_identity" <<'PY'
import json
import pathlib
import sys

manifest = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
product_version, runtime_version, runtime_identity = sys.argv[2:]
assert manifest["schema_version"] == 2
assert manifest["product"]["version"] == product_version
assert manifest["runtime"]["version"] == runtime_version
assert manifest["runtime"]["digest"] == "blake3:" + "0" * 64
assert "sha256" not in manifest["runtime"]
assert manifest["runtime"]["release_workflow_identity"] == runtime_identity
assert manifest["contracts"]["repository"] == "astrid-runtime/wit"
assert manifest["contracts"]["commit"] == "278dbca3e32f327d0f2358644fc86559779ba0fd"
assert manifest["contracts"]["sdk_rust_version"] == "0.7.1"
assert manifest["contracts"]["sdk_rust_commit"] == "bbbc61c8821d6c536fb25d2068b6b646e759ad35"
assert manifest["capsules"]["count"] == 21
assert len(manifest["capsules"]["assets"]) == 21
assert len(set(manifest["capsules"]["assets"])) == 21
PY

darwin_target=aarch64-apple-darwin
darwin_runtime_root="$work/astrid-$runtime_version-$darwin_target"
mkdir -p "$darwin_runtime_root"
for binary in astrid astrid-daemon astrid-build astrid-emit; do
  printf '#!/bin/sh\nexit 0\n' > "$darwin_runtime_root/$binary"
  chmod 755 "$darwin_runtime_root/$binary"
done
COPYFILE_DISABLE=1 tar -czf "$work/runtime-darwin.tar.gz" \
  -C "$work" "$(basename "$darwin_runtime_root")"
bash "$repo_root/scripts/package-release.sh" \
  "$darwin_target" \
  "$work/aos" \
  "$work/runtime-darwin.tar.gz" \
  0000000000000000000000000000000000000000000000000000000000000000 \
  "$work/capsules" \
  "$work/output" >/dev/null
test -f "$work/output/unicity-aos-$product_version-$darwin_target.tar.gz"

unsafe_root="$work/unsafe-runtime"
mkdir -p "$unsafe_root/astrid-$runtime_version-$target"
ln -s /tmp "$unsafe_root/astrid-$runtime_version-$target/astrid"
for binary in astrid-daemon astrid-build astrid-emit; do
  printf '#!/bin/sh\nexit 0\n' > "$unsafe_root/astrid-$runtime_version-$target/$binary"
  chmod 755 "$unsafe_root/astrid-$runtime_version-$target/$binary"
done
COPYFILE_DISABLE=1 tar -czf "$work/unsafe-runtime.tar.gz" -C "$unsafe_root" "astrid-$runtime_version-$target"
if bash "$repo_root/scripts/package-release.sh" \
  "$target" \
  "$work/aos" \
  "$work/unsafe-runtime.tar.gz" \
  0000000000000000000000000000000000000000000000000000000000000000 \
  "$work/capsules" \
  "$work/output" >/dev/null 2>&1; then
  echo "release composer accepted a symlinked runtime binary" >&2
  exit 1
fi

python3 - "$work/duplicate-runtime.tar.gz" "$target" "$runtime_version" <<'PY'
import io
import sys
import tarfile

archive_path, target, runtime_version = sys.argv[1:]
root = f"astrid-{runtime_version}-{target}"

def add(archive, name, data=b"#!/bin/sh\nexit 0\n"):
    member = tarfile.TarInfo(name)
    member.mode = 0o755
    member.size = len(data)
    archive.addfile(member, io.BytesIO(data))

with tarfile.open(archive_path, "w:gz") as archive:
    directory = tarfile.TarInfo(root)
    directory.type = tarfile.DIRTYPE
    directory.mode = 0o755
    archive.addfile(directory)
    for binary in ("astrid", "astrid-daemon", "astrid-build", "astrid-emit"):
        add(archive, f"{root}/{binary}")
    add(archive, f"{root}/astrid", b"#!/bin/sh\nexit 99\n")
PY

if bash "$repo_root/scripts/package-release.sh" \
  "$target" \
  "$work/aos" \
  "$work/duplicate-runtime.tar.gz" \
  0000000000000000000000000000000000000000000000000000000000000000 \
  "$work/capsules" \
  "$work/output" >/dev/null 2>&1; then
  echo "release composer accepted a duplicate runtime binary" >&2
  exit 1
fi

rm "$work/capsules/aos-cli.capsule"
if bash "$repo_root/scripts/package-release.sh" \
  "$target" \
  "$work/aos" \
  "$work/runtime.tar.gz" \
  0000000000000000000000000000000000000000000000000000000000000000 \
  "$work/capsules" \
  "$work/output" >/dev/null 2>&1; then
  echo "release composer accepted an incomplete capsule set" >&2
  exit 1
fi
