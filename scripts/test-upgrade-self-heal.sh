#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "required command not found: $1" >&2
    exit 1
  }
}
for command in b3sum cargo find python3 sort stat tar; do
  need "$command"
done

mode_of() {
  stat -c '%a' "$1" 2>/dev/null || stat -f '%Lp' "$1"
}

durable_paths() {
  local root=$1
  (
    cd "$root"
    find etc home keys log secrets var wit -type f -print
    find bin -type f -name '*.wasm' -print
  ) | LC_ALL=C sort
}

snapshot_durable_files() {
  local root=$1
  local output=$2
  : > "$output"
  while IFS= read -r relative; do
    printf '%s|%s|%s\n' \
      "$(b3sum -- "$root/$relative" | awk '{print $1}')" \
      "$(mode_of "$root/$relative")" \
      "$relative" >> "$output"
  done < <(durable_paths "$root")
}

snapshot_durable_directories() {
  local root=$1
  local output=$2
  : > "$output"
  for top_level in bin etc home keys log secrets var wit; do
    while IFS= read -r directory; do
      printf '%s|%s\n' \
        "$(mode_of "$directory")" \
        "${directory#"$root/"}" >> "$output"
    done < <(find "$root/$top_level" -type d -print | LC_ALL=C sort)
  done
}

expected_imported_path() {
  local root=$1
  local relative=$2
  case "$relative" in
    etc/profiles/default.toml)
      printf '%s\n' "$root/$relative"
      ;;
    etc/profiles/*)
      printf '%s\n' "$root/imported/astrid-home-v1/$relative"
      ;;
    home/default/.local/capsules | home/default/.local/capsules/*)
      printf '%s\n' "$root/$relative"
      ;;
    home/*/.local/capsules | home/*/.local/capsules/*)
      printf '%s\n' "$root/imported/astrid-home-v1/$relative"
      ;;
    *)
      printf '%s\n' "$root/$relative"
      ;;
  esac
}

assert_no_alternate_activation_path() {
  local root=$1
  local relative=$2
  local expected=$3
  local alternate
  case "$relative" in
    etc/profiles/*.toml | home/*/.local/capsules | home/*/.local/capsules/*)
      case "$expected" in
        "$root/imported/astrid-home-v1/"*) alternate=$root/$relative ;;
        *) alternate=$root/imported/astrid-home-v1/$relative ;;
      esac
      test ! -e "$alternate" || {
        echo "imported activation path exists in both locations: $relative" >&2
        exit 1
      }
      ;;
  esac
}

snapshot_imported_files() {
  local root=$1
  local source_snapshot=$2
  local output=$3
  : > "$output"
  while IFS='|' read -r _ _ relative; do
    local path
    path=$(expected_imported_path "$root" "$relative")
    test -f "$path"
    assert_no_alternate_activation_path "$root" "$relative" "$path"
    printf '%s|%s|%s\n' \
      "$(b3sum -- "$path" | awk '{print $1}')" \
      "$(mode_of "$path")" \
      "$relative" >> "$output"
  done < "$source_snapshot"
}

snapshot_imported_directories() {
  local root=$1
  local source_snapshot=$2
  local output=$3
  : > "$output"
  while IFS='|' read -r _ relative; do
    local path
    path=$(expected_imported_path "$root" "$relative")
    test -d "$path"
    assert_no_alternate_activation_path "$root" "$relative" "$path"
    printf '%s|%s\n' "$(mode_of "$path")" "$relative" >> "$output"
  done < "$source_snapshot"
}

snapshot_shipped_assets() {
  local home=$1
  local output=$2
  local release=$home/releases/2026.1.0
  : > "$output"
  printf '%s|bin/aos\n' "$(b3sum -- "$home/bin/aos" | awk '{print $1}')" >> "$output"
  for name in astrid astrid-daemon astrid-build astrid-emit; do
    printf '%s|runtime/bin/%s\n' \
      "$(b3sum -- "$home/runtime/bin/$name" | awk '{print $1}')" \
      "$name" >> "$output"
  done
  while IFS= read -r capsule; do
    printf '%s|releases/2026.1.0/capsules/%s\n' \
      "$(b3sum -- "$release/capsules/$capsule" | awk '{print $1}')" \
      "$capsule" >> "$output"
  done < "$release/capsule-assets.txt"
}

assert_imported_activation_layout() {
  local runtime=$1
  test "$(find "$runtime/etc/profiles" -type f 2>/dev/null | wc -l | tr -d ' ')" -eq 0
  test "$(find "$runtime/imported/astrid-home-v1/etc/profiles" -type f | wc -l | tr -d ' ')" -eq 7
  test ! -e "$runtime/home/alice/.local/capsules"
  test "$(find "$runtime/imported/astrid-home-v1/home/alice/.local/capsules" -type f | wc -l | tr -d ' ')" -eq 140
}

home=$work/home
legacy=$home/.astrid
aos_home=$home/.aos
fixture=$work/downloads
fake_bin=$work/fake-bin
capsules=$work/capsules
mkdir -p "$home" "$fixture" "$fake_bin" "$capsules"
python3 "$repo_root/scripts/create-astrid-094-fixture.py" "$legacy"

cargo build --locked -p unicity-aos-bootstrap --bin aos
product_binary=$repo_root/target/debug/aos
test "$($product_binary --version)" = 'Unicity AOS 2026.1.0'

PYTHONPATH="$repo_root/scripts" python3 - "$capsules" <<'PY'
import pathlib
import sys

from capsule_release import source_contract
from test_capsule_release import write_fixture

output = pathlib.Path(sys.argv[1])
for spec in source_contract():
    write_fixture(output / spec.asset, spec)
PY

target=x86_64-unknown-linux-gnu
read -r \
  runtime_version \
  runtime_tag \
  runtime_identity \
  runtime_metadata_available \
  runtime_source_commit \
  runtime_metadata_asset \
  runtime_metadata_blake3 < <(
  python3 - "$repo_root/release/runtime-compatibility.toml" <<'PY'
import pathlib
import sys
import tomllib

with pathlib.Path(sys.argv[1]).open("rb") as file:
    runtime = tomllib.load(file)["runtime"]
print(
    runtime["version"],
    runtime["tag"],
    runtime["release-workflow-identity"],
    str(runtime["release-metadata-available"]).lower(),
    runtime["source-commit"],
    runtime["release-metadata-asset"],
    runtime["release-metadata-blake3"],
)
PY
)
if [[ -z "$runtime_version" || -z "$runtime_tag" || -z "$runtime_identity" || \
      -z "$runtime_source_commit" || -z "$runtime_metadata_asset" || \
      -z "$runtime_metadata_blake3" ]]; then
  echo "runtime compatibility fixture provenance is incomplete" >&2
  exit 1
fi
if [[ "$runtime_metadata_available" != true ]]; then
  echo "upgrade/self-heal fixture must exercise available runtime release metadata" >&2
  exit 1
fi
runtime_root=$work/astrid-$runtime_version-$target
mkdir "$runtime_root"
for name in astrid astrid-daemon astrid-build astrid-emit; do
  printf '#!/bin/sh\necho packaged-%s\n' "$name" > "$runtime_root/$name"
  chmod 755 "$runtime_root/$name"
done
COPYFILE_DISABLE=1 tar -czf "$work/runtime.tar.gz" -C "$work" "$(basename "$runtime_root")"
bash "$repo_root/scripts/package-release.sh" \
  "$target" \
  "$product_binary" \
  "$work/runtime.tar.gz" \
  0000000000000000000000000000000000000000000000000000000000000000 \
  "$capsules" \
  "$fixture" >/dev/null

asset=$fixture/unicity-aos-2026.1.0-$target.tar.gz
bundle=$asset.sigstore.json
cp "$asset" "$fixture/signed-asset.tar.gz"
printf 'valid Sigstore fixture\n' > "$fixture/valid.sigstore.json"
cp "$fixture/valid.sigstore.json" "$bundle"
asset_sha256=$(shasum -a 256 "$asset" | awk '{print $1}')
asset_blake3=$(b3sum "$asset" | awk '{print $1}')
asset_size=$(wc -c < "$asset" | tr -d ' ')
release_metadata=$fixture/unicity-aos-2026.1.0-release.toml
cat > "$release_metadata" <<EOF
schema-version = 1
kind = "aos-release"
product = "unicity-aos-ce"
version = "2026.1.0"
tag = "2026.1.0"
source-commit = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
published-at = "2026-07-16T10:00:00Z"
release-workflow-identity = "https://github.com/unicity-aos/aos-ce/.github/workflows/release.yml@refs/tags/2026.1.0"

[runtime]
repository = "astrid-runtime/astrid"
version = "${runtime_version}"
tag = "${runtime_tag}"
release-workflow-identity = "${runtime_identity}"
release-metadata-available = ${runtime_metadata_available}
source-commit = "${runtime_source_commit}"
release-metadata-asset = "${runtime_metadata_asset}"
release-metadata-blake3 = "${runtime_metadata_blake3}"

[contracts]
repository = "astrid-runtime/wit"
commit = "278dbca3e32f327d0f2358644fc86559779ba0fd"
sdk-rust-version = "0.7.1"
sdk-rust-commit = "bbbc61c8821d6c536fb25d2068b6b646e759ad35"

[gates]
release-ready = false
upgrade-self-heal-ready = false
EOF
for metadata_target in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
  metadata_asset="unicity-aos-2026.1.0-${metadata_target}.tar.gz"
  cat >> "$release_metadata" <<EOF

[targets.${metadata_target}]
asset = "${metadata_asset}"
sha256 = "${asset_sha256}"
blake3 = "${asset_blake3}"
sigstore-bundle = "${metadata_asset}.sigstore.json"
size = ${asset_size}
EOF
done
cp "$fixture/valid.sigstore.json" "$release_metadata.sigstore.json"

cat > "$fake_bin/uname" <<'EOF'
#!/bin/sh
case "${1:-}" in
  -s) echo Linux ;;
  -m) echo x86_64 ;;
  *) exit 2 ;;
esac
EOF
cat > "$fake_bin/curl" <<'EOF'
#!/bin/sh
set -eu
output=
url=
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) output=$2; shift ;;
    http*) url=$1 ;;
  esac
  shift
done
[ -n "$output" ]
[ -n "$url" ]
cp "$AOS_TEST_FIXTURE/$(basename "$url")" "$output"
EOF
cat > "$fixture/cosign-linux-amd64" <<'EOF'
#!/bin/sh
set -eu
[ "${1:-}" = verify-blob ]
bundle=
artifact=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --bundle) bundle=$2; shift ;;
    -*) ;;
    *) artifact=$1 ;;
  esac
  shift
done
cmp "$AOS_TEST_FIXTURE/valid.sigstore.json" "$bundle"
[ -f "$artifact" ]
EOF
cat > "$fake_bin/sha256sum" <<'EOF'
#!/bin/sh
set -eu
case "$1" in
  */cosign)
    printf '%s  %s\n' ae1ecd212663f3693ad9edf8b1a183900c9a52d3155ba6e354237f9a0f6463fc "$1"
    ;;
  *) exec /usr/bin/shasum -a 256 "$1" ;;
esac
EOF
chmod 755 "$fake_bin/uname" "$fake_bin/curl" "$fake_bin/sha256sum" \
  "$fixture/cosign-linux-amd64"

install_candidate() {
  PATH="$fake_bin:$PATH" \
  HOME="$home" \
  AOS_TEST_FIXTURE="$fixture" \
  AOS_VERSION=2026.1.0 \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null
}

source_files_before=$work/source-files-before
source_dirs_before=$work/source-dirs-before
snapshot_durable_files "$legacy" "$source_files_before"
snapshot_durable_directories "$legacy" "$source_dirs_before"
test "$(wc -l < "$source_files_before" | tr -d ' ')" -eq 478
test "$(find "$legacy/run" -type f | wc -l | tr -d ' ')" -eq 5

install_candidate
test -x "$aos_home/bin/aos"
test -x "$aos_home/runtime/bin/astrid-daemon"
HOME="$home" "$aos_home/bin/aos" migrate runtime --from "$legacy" > "$work/migrate.log"
grep -F 'imported the standalone runtime; the source was left unchanged' "$work/migrate.log" >/dev/null
assert_imported_activation_layout "$aos_home/runtime"

source_files_after=$work/source-files-after
source_dirs_after=$work/source-dirs-after
target_files=$work/target-files
target_dirs=$work/target-dirs
snapshot_durable_files "$legacy" "$source_files_after"
snapshot_durable_directories "$legacy" "$source_dirs_after"
snapshot_imported_files "$aos_home/runtime" "$source_files_before" "$target_files"
snapshot_imported_directories "$aos_home/runtime" "$source_dirs_before" "$target_dirs"
diff -u "$source_files_before" "$source_files_after"
diff -u "$source_dirs_before" "$source_dirs_after"
diff -u \
  <(cut -d '|' -f 2 "$source_dirs_before") \
  <(cut -d '|' -f 2 "$target_dirs")
diff -u \
  <(cut -d '|' -f 1,3 "$source_files_before") \
  <(cut -d '|' -f 1,3 "$target_files")

while IFS='|' read -r _ source_mode relative; do
  target_path=$(expected_imported_path "$aos_home/runtime" "$relative")
  target_mode=$(mode_of "$target_path")
  if (( (8#$source_mode & 8#111) != 0 )); then
    expected_mode=700
  else
    expected_mode=600
  fi
  test "$target_mode" = "$expected_mode"
done < "$source_files_before"
while IFS='|' read -r target_mode relative; do
  test "$target_mode" = 700 || {
    echo "target directory is not private: $relative ($target_mode)" >&2
    exit 1
  }
done < "$target_dirs"

for transient in .hud-health session.principal system.lock system.pid system.token; do
  test -f "$legacy/run/$transient"
  test ! -e "$aos_home/runtime/run/$transient"
done
test ! -e "$aos_home/runtime/run"

receipt=$aos_home/migrations/astrid-home-v1.json
test -f "$receipt"
receipt_before=$(b3sum -- "$receipt" | awk '{print $1}')
HOME="$home" "$aos_home/bin/aos" migrate runtime --from "$legacy" > "$work/idempotent.log"
grep -F 'this runtime migration is already complete' "$work/idempotent.log" >/dev/null
test "$(b3sum -- "$receipt" | awk '{print $1}')" = "$receipt_before"

shipped_before=$work/shipped-before
shipped_after=$work/shipped-after
snapshot_shipped_assets "$aos_home" "$shipped_before"
for name in aos astrid astrid-daemon astrid-build astrid-emit; do
  case "$name" in
    aos) destination=$aos_home/bin/aos ;;
    *) destination=$aos_home/runtime/bin/$name ;;
  esac
  printf '#!/bin/sh\nexit 0\n' > "$destination"
  chmod 755 "$destination"
done
while IFS= read -r capsule; do
  printf 'tampered capsule\n' > "$aos_home/releases/2026.1.0/capsules/$capsule"
done < "$aos_home/releases/2026.1.0/capsule-assets.txt"
chmod 755 \
  "$aos_home" \
  "$aos_home/bin" \
  "$aos_home/runtime" \
  "$aos_home/runtime/bin" \
  "$aos_home/releases" \
  "$aos_home/releases/2026.1.0" \
  "$aos_home/releases/2026.1.0/capsules"

install_candidate
assert_imported_activation_layout "$aos_home/runtime"
snapshot_shipped_assets "$aos_home" "$shipped_after"
diff -u "$shipped_before" "$shipped_after"
for directory in \
  "$aos_home" \
  "$aos_home/bin" \
  "$aos_home/runtime" \
  "$aos_home/runtime/bin" \
  "$aos_home/releases" \
  "$aos_home/releases/2026.1.0" \
  "$aos_home/releases/2026.1.0/capsules"; do
  test "$(mode_of "$directory")" = 700
done

snapshot_durable_files "$legacy" "$source_files_after"
snapshot_durable_directories "$legacy" "$source_dirs_after"
snapshot_imported_files "$aos_home/runtime" "$source_files_before" "$target_files"
snapshot_imported_directories "$aos_home/runtime" "$source_dirs_before" "$target_dirs"
diff -u "$source_files_before" "$source_files_after"
diff -u "$source_dirs_before" "$source_dirs_after"
diff -u \
  <(cut -d '|' -f 1,3 "$source_files_before") \
  <(cut -d '|' -f 1,3 "$target_files")
diff -u \
  <(cut -d '|' -f 2 "$source_dirs_before") \
  <(cut -d '|' -f 2 "$target_dirs")
while IFS='|' read -r _ source_mode relative; do
  target_path=$(expected_imported_path "$aos_home/runtime" "$relative")
  target_mode=$(mode_of "$target_path")
  if (( (8#$source_mode & 8#111) != 0 )); then
    expected_mode=700
  else
    expected_mode=600
  fi
  test "$target_mode" = "$expected_mode"
done < "$source_files_before"
while IFS='|' read -r target_mode relative; do
  test "$target_mode" = 700 || {
    echo "target directory is not private after reinstall: $relative ($target_mode)" >&2
    exit 1
  }
done < "$target_dirs"
test "$(b3sum -- "$receipt" | awk '{print $1}')" = "$receipt_before"
HOME="$home" "$aos_home/bin/aos" migrate runtime --from "$legacy" > "$work/reinstall-idempotent.log"
grep -F 'this runtime migration is already complete' "$work/reinstall-idempotent.log" >/dev/null
test "$(b3sum -- "$receipt" | awk '{print $1}')" = "$receipt_before"
test ! -e "$aos_home/runtime/run"

echo "sanitized packaged migration, reinstall, and self-heal checks passed"
