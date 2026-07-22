#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fixture="$work/fixture"
fake_bin="$work/fake-bin"
mkdir -p "$fixture" "$fake_bin" "$work/home" "$work/capsules"
mkdir -p "$work/home/.astrid"
printf 'standalone-runtime-state\n' > "$work/home/.astrid/sentinel"
if ! read -r \
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
); then
  echo "failed to read runtime compatibility fixture provenance" >&2
  exit 1
fi
if [[ -z "$runtime_version" || -z "$runtime_tag" || -z "$runtime_identity" || \
      -z "$runtime_source_commit" || -z "$runtime_metadata_asset" || \
      -z "$runtime_metadata_blake3" ]]; then
  echo "runtime compatibility fixture provenance is incomplete" >&2
  exit 1
fi
if [[ "$runtime_metadata_available" != true ]]; then
  echo "installer fixture must exercise available runtime release metadata" >&2
  exit 1
fi

cat > "$work/aos" <<'EOF'
#!/bin/sh
if [ "${1:-}" = --version ]; then
  echo 'Unicity AOS 2026.1.3'
  exit 0
fi
exit 0
EOF
chmod 755 "$work/aos"

PYTHONPATH="$repo_root/scripts" python3 - "$work/capsules" <<'PY'
import pathlib
import sys

from capsule_release import source_contract
from test_capsule_release import write_fixture

output = pathlib.Path(sys.argv[1])
for spec in source_contract():
    write_fixture(output / spec.asset, spec)
PY

runtime_root="$work/astrid-$runtime_version-x86_64-unknown-linux-gnu"
mkdir -p "$runtime_root"
for binary in astrid astrid-daemon astrid-build astrid-emit; do
  printf '#!/bin/sh\necho %s\n' "$binary" > "$runtime_root/$binary"
  chmod 755 "$runtime_root/$binary"
done
COPYFILE_DISABLE=1 tar -czf "$work/runtime.tar.gz" -C "$work" "$(basename "$runtime_root")"
bash "$repo_root/scripts/package-release.sh" \
  x86_64-unknown-linux-gnu \
  "$work/aos" \
  "$work/runtime.tar.gz" \
  0000000000000000000000000000000000000000000000000000000000000000 \
  "$work/capsules" \
  "$fixture" >/dev/null
asset="$fixture/unicity-aos-2026.1.3-x86_64-unknown-linux-gnu.tar.gz"
bundle="$asset.sigstore.json"
signed_asset="$fixture/signed-asset.tar.gz"
good_bundle="$fixture/valid.sigstore.json"
cp "$asset" "$signed_asset"
printf 'valid Sigstore fixture\n' > "$good_bundle"
cp "$good_bundle" "$bundle"

asset_sha256=$(shasum -a 256 "$asset" | awk '{print $1}')
asset_blake3=$(b3sum "$asset" | awk '{print $1}')
asset_size=$(wc -c < "$asset" | tr -d ' ')
release_metadata="$fixture/unicity-aos-2026.1.3-release.toml"
cat > "$release_metadata" <<EOF
schema-version = 1
kind = "aos-release"
product = "unicity-aos-ce"
version = "2026.1.3"
tag = "2026.1.3"
source-commit = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
published-at = "2026-07-16T10:00:00Z"
release-workflow-identity = "https://github.com/unicity-aos/aos-ce/.github/workflows/release.yml@refs/tags/2026.1.3"

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
release-ready = true
upgrade-self-heal-ready = true
EOF
for metadata_target in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
  metadata_asset="unicity-aos-2026.1.3-${metadata_target}.tar.gz"
  cat >> "$release_metadata" <<EOF

[targets.${metadata_target}]
asset = "${metadata_asset}"
sha256 = "${asset_sha256}"
blake3 = "${asset_blake3}"
sigstore-bundle = "${metadata_asset}.sigstore.json"
size = ${asset_size}
EOF
done
cp "$good_bundle" "$release_metadata.sigstore.json"
cp "$release_metadata" "$fixture/release-good.toml"

cat > "$fake_bin/uname" <<'EOF'
#!/bin/sh
case "${1:-}" in
  -s) echo Linux ;;
  -m) echo x86_64 ;;
  *) exit 2 ;;
esac
EOF
cat > "$fake_bin/date" <<'EOF'
#!/bin/sh
if [ "$#" -eq 2 ] && [ "$1" = -u ]; then
  case "$2" in
    +%Y-%m-%dT%H:%M:%SZ) printf '%s\n' '2026-07-16T10:00:00Z'; exit 0 ;;
    +%s) printf '%s\n' '1784196000'; exit 0 ;;
  esac
fi
exec /bin/date "$@"
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
identity=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --bundle)
      bundle=$2
      shift
      ;;
    --certificate-identity)
      identity=$2
      shift
      ;;
    --certificate-identity-regexp) exit 97 ;;
    -*) ;;
    *) artifact=$1 ;;
  esac
  shift
done
[ -n "$bundle" ]
[ -n "$artifact" ]
[ -n "$identity" ]
cmp "$AOS_TEST_FIXTURE/valid.sigstore.json" "$bundle"
[ -f "$artifact" ]
printf '%s\n' "$identity" >> "$AOS_TEST_FIXTURE/cosign-identities"
: > "$AOS_TEST_FIXTURE/cosign-called"
EOF
cat > "$fake_bin/cosign" <<'EOF'
#!/bin/sh
set -eu
: > "$AOS_TEST_FIXTURE/path-cosign-called"
exit 99
EOF

cat > "$fake_bin/sha256sum" <<'EOF'
#!/bin/sh
set -eu
case "$1" in
  */cosign)
    if [ "${AOS_TEST_BAD_COSIGN_DIGEST:-0}" = 1 ]; then
      printf '%064d  %s\n' 0 "$1"
    else
      printf '%s  %s\n' ae1ecd212663f3693ad9edf8b1a183900c9a52d3155ba6e354237f9a0f6463fc "$1"
    fi
    ;;
  *) exec /usr/bin/shasum -a 256 "$1" ;;
esac
EOF
chmod 755 "$fake_bin/uname" "$fake_bin/date" "$fake_bin/curl" "$fake_bin/cosign" \
  "$fake_bin/sha256sum" "$fixture/cosign-linux-amd64"

if PATH="$fake_bin:$PATH" HOME="$work/impossible-nightly-home" AOS_TEST_FIXTURE="$fixture" \
  sh "$repo_root/install.sh" --version "2026.1.3-nightly.20260230.g$(printf '%040d' 0)" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer accepted a nightly version with an impossible date" >&2
  exit 1
fi
test ! -e "$work/impossible-nightly-home/.aos"

PATH="$fake_bin:$PATH" \
HOME="$work/home" \
AOS_TEST_FIXTURE="$fixture" \
AOS_VERSION=2026.1.3 \
sh "$repo_root/install.sh" --yes --no-migrate-prompt

test -x "$work/home/.aos/bin/aos"
test -x "$work/home/.aos/runtime/bin/astrid-daemon"
release_dir="$work/home/.aos/releases/2026.1.3"
test -f "$release_dir/release-manifest.json"
test -f "$release_dir/Distro.toml"
test -f "$release_dir/capsule-assets.txt"
test "$(find "$release_dir/capsules" -mindepth 1 -maxdepth 1 -type f | wc -l | tr -d ' ')" -eq 21
while IFS= read -r capsule; do
  cmp "$work/capsules/$capsule" "$release_dir/capsules/$capsule"
done < "$release_dir/capsule-assets.txt"
test "$("$work/home/.aos/bin/aos" --version)" = 'Unicity AOS 2026.1.3'
test -f "$work/home/.aos/libexec/install.sh"
test "$(stat -c '%a' "$work/home/.aos/libexec/install.sh" 2>/dev/null || stat -f '%Lp' "$work/home/.aos/libexec/install.sh")" = 600
test "$(cat "$work/home/.astrid/sentinel")" = 'standalone-runtime-state'
test -f "$fixture/cosign-called"
test ! -e "$fixture/path-cosign-called"
test "$(stat -c '%a' "$work/home/.aos" 2>/dev/null || stat -f '%Lp' "$work/home/.aos")" = 700
test "$(stat -c '%a' "$release_dir/release-manifest.json" 2>/dev/null || stat -f '%Lp' "$release_dir/release-manifest.json")" = 600
test "$(stat -c '%a' "$release_dir/capsules" 2>/dev/null || stat -f '%Lp' "$release_dir/capsules")" = 700

python=${PYTHON3:-python3}
"$python" "$repo_root/scripts/release_metadata.py" render-channel \
  --channel stable \
  --generation 2 \
  --published-at 2026-07-16T10:00:00Z \
  --expires-at 2026-08-15T10:00:00Z \
  --release-metadata "$release_metadata" \
  --require-ready \
  --output "$fixture/channel.toml"
cp "$good_bundle" "$fixture/channel.toml.sigstore.json"
cp "$fixture/channel.toml" "$fixture/channel-good.toml"

PATH="$fake_bin:$PATH" HOME="$work/channel-home" AOS_TEST_FIXTURE="$fixture" \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null
accepted_current="$work/channel-home/.aos/update/channels/stable/current"
accepted_channel="$work/channel-home/.aos/update/channels/stable/generations/2/channel.toml"
accepted_bundle="$work/channel-home/.aos/update/channels/stable/generations/2/channel.toml.sigstore.json"
test "$(cat "$accepted_current")" = 2
test -f "$accepted_channel"
test -f "$accepted_bundle"
test "$(awk '$1 == "generation" { print $3 }' "$accepted_channel")" = 2
grep -Fx 'https://github.com/unicity-aos/aos-ce/.github/workflows/promote-channel.yml@refs/heads/main' \
  "$fixture/cosign-identities" >/dev/null
grep -Fx 'https://github.com/unicity-aos/aos-ce/.github/workflows/release.yml@refs/tags/2026.1.3' \
  "$fixture/cosign-identities" >/dev/null

cp "$fixture/channel-good.toml" "$fixture/channel.toml"
nightly_version="2026.1.3-nightly.20260717.gaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
sed -i.bak "s/version = \"2026.1.3\"/version = \"$nightly_version\"/" "$fixture/channel.toml"
rm "$fixture/channel.toml.bak"
if PATH="$fake_bin:$PATH" HOME="$work/nightly-on-stable-home" AOS_TEST_FIXTURE="$fixture" \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer accepted a nightly release through the stable channel" >&2
  exit 1
fi
test ! -e "$work/nightly-on-stable-home/.aos"
cp "$fixture/channel-good.toml" "$fixture/channel.toml"

channel_root="$work/channel-home/.aos/update/channels/stable"
mkdir "$channel_root/generations/3"
cp "$accepted_channel" "$channel_root/generations/3/channel.toml"
cp "$accepted_bundle" "$channel_root/generations/3/channel.toml.sigstore.json"
PATH="$fake_bin:$PATH" HOME="$work/channel-home" AOS_TEST_FIXTURE="$fixture" \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null
test "$(cat "$accepted_current")" = 2

mkdir -p "$work/channel-home/.aos/update/install.lock"
sleep 60 &
live_lock_pid=$!
printf '%s\n' "$live_lock_pid" > "$work/channel-home/.aos/update/install.lock/pid"
if PATH="$fake_bin:$PATH" HOME="$work/channel-home" AOS_TEST_FIXTURE="$fixture" \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer ignored a live installation lock" >&2
  kill "$live_lock_pid" 2>/dev/null || true
  exit 1
fi
kill "$live_lock_pid" 2>/dev/null || true
wait "$live_lock_pid" 2>/dev/null || true
rm -rf "$work/channel-home/.aos/update/install.lock"
mkdir "$work/channel-home/.aos/update/install.lock"
printf '%s\n' 999999999 > "$work/channel-home/.aos/update/install.lock/pid"
PATH="$fake_bin:$PATH" HOME="$work/channel-home" AOS_TEST_FIXTURE="$fixture" \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null
test ! -e "$work/channel-home/.aos/update/install.lock"

for lexical_case in schema generation size; do
  cp "$fixture/channel-good.toml" "$fixture/channel.toml"
  case "$lexical_case" in
    schema) sed -i.bak 's/schema-version = 1/schema-version = "1"/' "$fixture/channel.toml" ;;
    generation) sed -i.bak 's/generation = 2/generation = "2"/' "$fixture/channel.toml" ;;
    size) sed -E -i.bak 's/^size = ([0-9]+)$/size = "\1"/' "$fixture/channel.toml" ;;
  esac
  rm "$fixture/channel.toml.bak"
  if PATH="$fake_bin:$PATH" HOME="$work/quoted-${lexical_case}-home" AOS_TEST_FIXTURE="$fixture" \
    sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
    echo "installer accepted a quoted TOML $lexical_case field" >&2
    exit 1
  fi
  test ! -e "$work/quoted-${lexical_case}-home/.aos"
done

cp "$fixture/release-good.toml" "$release_metadata"
sed -i.bak 's/release-ready = true/release-ready = "true"/' "$release_metadata"
rm "$release_metadata.bak"
if PATH="$fake_bin:$PATH" HOME="$work/quoted-gate-home" AOS_TEST_FIXTURE="$fixture" \
  sh "$repo_root/install.sh" --version 2026.1.3 --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer accepted a quoted TOML readiness gate" >&2
  exit 1
fi
test ! -e "$work/quoted-gate-home/.aos"
cp "$fixture/release-good.toml" "$release_metadata"
cp "$fixture/channel-good.toml" "$fixture/channel.toml"

cp "$fixture/channel-good.toml" "$fixture/channel.toml"
sed -i.bak 's/published-at = "2026-07-16T10:00:00Z"/published-at = "not-a-time"/' "$fixture/channel.toml"
rm "$fixture/channel.toml.bak"
if PATH="$fake_bin:$PATH" HOME="$work/bad-channel-time-home" AOS_TEST_FIXTURE="$fixture" \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer accepted one valid and one malformed channel timestamp" >&2
  exit 1
fi
test ! -e "$work/bad-channel-time-home/.aos"

cp "$fixture/channel-good.toml" "$fixture/channel.toml"
sed -i.bak 's/expires-at = "2026-08-15T10:00:00Z"/expires-at = "2026-08-15T10:00:01Z"/' "$fixture/channel.toml"
rm "$fixture/channel.toml.bak"
if PATH="$fake_bin:$PATH" HOME="$work/excessive-channel-lifetime-home" AOS_TEST_FIXTURE="$fixture" \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer accepted a channel lifetime beyond its channel maximum" >&2
  exit 1
fi
test ! -e "$work/excessive-channel-lifetime-home/.aos"

cp "$fixture/channel-good.toml" "$fixture/channel.toml"
sed -i.bak 's/published-at = "2026-07-16T10:00:00Z"/published-at = "2026-07-16T10:05:01Z"/' "$fixture/channel.toml"
rm "$fixture/channel.toml.bak"
if PATH="$fake_bin:$PATH" HOME="$work/future-channel-home" AOS_TEST_FIXTURE="$fixture" \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer accepted an unreasonably future channel publication" >&2
  exit 1
fi
test ! -e "$work/future-channel-home/.aos"

cp "$fixture/channel-good.toml" "$fixture/channel.toml"
sed -i.bak 's/blake3 = "[0-9a-f]*"/blake3 = "BAD"/' "$fixture/channel.toml"
rm "$fixture/channel.toml.bak"
if PATH="$fake_bin:$PATH" HOME="$work/bad-channel-digest-home" AOS_TEST_FIXTURE="$fixture" \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer accepted one valid and one malformed channel target digest" >&2
  exit 1
fi
test ! -e "$work/bad-channel-digest-home/.aos"

cp "$fixture/channel-good.toml" "$fixture/channel.toml"
sed -i.bak 's/generation = 2/generation = 1000000000000000000/' "$fixture/channel.toml"
rm "$fixture/channel.toml.bak"
if PATH="$fake_bin:$PATH" HOME="$work/oversized-generation-home" AOS_TEST_FIXTURE="$fixture" \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer accepted a channel generation outside its comparison range" >&2
  exit 1
fi
test ! -e "$work/oversized-generation-home/.aos"

cp "$fixture/channel-good.toml" "$fixture/channel.toml"
sed -i.bak 's/generation = 2/generation = 1/' "$fixture/channel.toml"
rm "$fixture/channel.toml.bak"
if PATH="$fake_bin:$PATH" HOME="$work/channel-home" AOS_TEST_FIXTURE="$fixture" \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer accepted a signed channel generation downgrade" >&2
  exit 1
fi
test "$(awk '$1 == "generation" { print $3 }' "$accepted_channel")" = 2

cp "$fixture/channel-good.toml" "$fixture/channel.toml"
sed -i.bak 's/published-at = "2026-07-16T10:00:00Z"/published-at = "2026-07-16T10:00:01Z"/' "$fixture/channel.toml"
rm "$fixture/channel.toml.bak"
if PATH="$fake_bin:$PATH" HOME="$work/channel-home" AOS_TEST_FIXTURE="$fixture" \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer accepted conflicting metadata at an accepted channel generation" >&2
  exit 1
fi
cmp "$fixture/channel-good.toml" "$accepted_channel"
cp "$fixture/channel-good.toml" "$fixture/channel.toml"

unavailable_fixture="$work/unavailable-fixture"
mkdir "$unavailable_fixture"
cp "$fixture/cosign-linux-amd64" "$unavailable_fixture/"
if PATH="$fake_bin:$PATH" HOME="$work/unavailable-channel-home" AOS_TEST_FIXTURE="$unavailable_fixture" \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer accepted an unavailable default stable channel" >&2
  exit 1
fi
test ! -e "$work/unavailable-channel-home/.aos"

if PATH="$fake_bin:$PATH" HOME="$work/mutually-exclusive-home" AOS_TEST_FIXTURE="$fixture" \
  sh "$repo_root/install.sh" --channel dev --version 2026.1.3 --yes --no-migrate-prompt \
  >/dev/null 2>&1; then
  echo "installer accepted mutually exclusive channel and version selectors" >&2
  exit 1
fi
test ! -e "$work/mutually-exclusive-home/.aos"

printf 'tampered capsule\n' > "$release_dir/capsules/aos-cli.capsule"
PATH="$fake_bin:$PATH" HOME="$work/home" AOS_TEST_FIXTURE="$fixture" AOS_VERSION=2026.1.3 \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null
cmp "$work/capsules/aos-cli.capsule" "$release_dir/capsules/aos-cli.capsule"

cat > "$work/home/.aos/bin/aos" <<'EOF'
#!/bin/sh
set -eu
if [ "${1:-}" = stop ]; then
  : > "$AOS_STOP_MARKER"
fi
echo existing-unicity-aos
EOF
chmod 755 "$work/home/.aos/bin/aos"
cp "$work/home/.aos/bin/aos" "$work/aos-before-unattended-upgrade"
if PATH="$fake_bin:$PATH" HOME="$work/home" AOS_TEST_FIXTURE="$fixture" AOS_VERSION=2026.1.3 \
  AOS_STOP_MARKER="$work/unattended-stop-called" \
  sh "$repo_root/install.sh" --no-migrate-prompt </dev/null >"$work/unattended-upgrade.log" 2>&1; then
  echo "installer replaced an existing installation without confirmation" >&2
  exit 1
fi
cmp "$work/aos-before-unattended-upgrade" "$work/home/.aos/bin/aos"
test ! -e "$work/unattended-stop-called"
grep -F 'rerun with --yes to replace it without a prompt' "$work/unattended-upgrade.log" >/dev/null

rm -f "$fixture/cosign-called"
if PATH="$fake_bin:$PATH" HOME="$work/bad-verifier-home" AOS_TEST_FIXTURE="$fixture" \
  AOS_TEST_BAD_COSIGN_DIGEST=1 AOS_VERSION=2026.1.3 \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer accepted a Sigstore verifier with the wrong digest" >&2
  exit 1
fi
test ! -e "$fixture/cosign-called"
test ! -e "$work/bad-verifier-home/.aos"

printf 'invalid Sigstore fixture\n' > "$bundle"
if PATH="$fake_bin:$PATH" HOME="$work/bad-bundle-home" AOS_TEST_FIXTURE="$fixture" AOS_VERSION=2026.1.3 \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer accepted an invalid Sigstore bundle" >&2
  exit 1
fi
test ! -e "$work/bad-bundle-home/.aos"
cp "$good_bundle" "$bundle"

mv "$bundle" "$work/missing-bundle"
if PATH="$fake_bin:$PATH" HOME="$work/missing-bundle-home" AOS_TEST_FIXTURE="$fixture" AOS_VERSION=2026.1.3 \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer accepted a release with no Sigstore bundle" >&2
  exit 1
fi
test ! -e "$work/missing-bundle-home/.aos"
mv "$work/missing-bundle" "$bundle"

printf 'modified after signing\n' >> "$asset"
if PATH="$fake_bin:$PATH" HOME="$work/modified-asset-home" AOS_TEST_FIXTURE="$fixture" AOS_VERSION=2026.1.3 \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer accepted release bytes that did not match the Sigstore bundle" >&2
  exit 1
fi
test ! -e "$work/modified-asset-home/.aos"
cp "$signed_asset" "$asset"

symlink_home="$work/symlink-destination-home"
mkdir -p "$symlink_home/.aos/bin"
cat > "$work/symlink-target" <<'EOF'
#!/bin/sh
set -eu
: > "$AOS_SYMLINK_MARKER"
EOF
chmod 755 "$work/symlink-target"
ln -s "$work/symlink-target" "$symlink_home/.aos/bin/aos"
if PATH="$fake_bin:$PATH" HOME="$symlink_home" AOS_TEST_FIXTURE="$fixture" AOS_VERSION=2026.1.3 \
  AOS_SYMLINK_MARKER="$work/symlink-executed" \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer replaced a symlinked destination" >&2
  exit 1
fi
test -L "$symlink_home/.aos/bin/aos"
test ! -e "$work/symlink-executed"

custom_bin_home="$work/custom-bin-home"
custom_bin_target="$work/custom-bin-target"
custom_bin_link="$work/custom-bin-link"
mkdir -p "$custom_bin_home" "$custom_bin_target"
cp "$work/symlink-target" "$custom_bin_target/aos"
ln -s "$custom_bin_target" "$custom_bin_link"
if PATH="$fake_bin:$PATH" HOME="$custom_bin_home" AOS_BIN_DIR="$custom_bin_link" \
  AOS_TEST_FIXTURE="$fixture" AOS_VERSION=2026.1.3 \
  AOS_SYMLINK_MARKER="$work/custom-bin-symlink-executed" \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >"$work/custom-bin-symlink.log" 2>&1; then
  echo "installer accepted a symlinked custom binary directory" >&2
  exit 1
fi
test ! -e "$work/custom-bin-symlink-executed"
grep -F "refusing symlinked binary directory: $custom_bin_link" "$work/custom-bin-symlink.log" >/dev/null

managed_symlink_home="$work/managed-symlink-home"
mkdir -p "$managed_symlink_home" "$work/managed-symlink-target/bin"
ln -s "$work/managed-symlink-target" "$managed_symlink_home/.aos"
ln -s "$work/symlink-target" "$work/managed-symlink-target/bin/aos"
if PATH="$fake_bin:$PATH" HOME="$managed_symlink_home" AOS_TEST_FIXTURE="$fixture" AOS_VERSION=2026.1.3 \
  AOS_SYMLINK_MARKER="$work/managed-symlink-executed" \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer accepted a symlinked managed installation root" >&2
  exit 1
fi
test ! -e "$work/managed-symlink-executed"

directory_home="$work/directory-destination-home"
mkdir -p "$directory_home/.aos/bin/aos"
if PATH="$fake_bin:$PATH" HOME="$directory_home" AOS_TEST_FIXTURE="$fixture" AOS_VERSION=2026.1.3 \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer replaced a directory destination" >&2
  exit 1
fi
test -d "$directory_home/.aos/bin/aos"

for binary in aos astrid astrid-daemon astrid-build astrid-emit; do
  case "$binary" in
    aos) destination="$work/home/.aos/bin/aos" ;;
    *) destination="$work/home/.aos/runtime/bin/$binary" ;;
  esac
  printf '#!/bin/sh\necho old-%s\n' "$binary" > "$destination"
  chmod 755 "$destination"
done
printf 'old-release-manifest\n' > "$release_dir/release-manifest.json"

fail_bin="$work/fail-bin"
mkdir "$fail_bin"
cat > "$fail_bin/mv" <<'EOF'
#!/bin/sh
set -eu
last=
for argument in "$@"; do last=$argument; done
if [ "$last" = "$MV_FAIL_DESTINATION" ] && [ ! -f "$MV_FAILED" ]; then
  : > "$MV_FAILED"
  exit 1
fi
exec "$REAL_MV" "$@"
EOF
chmod 755 "$fail_bin/mv"
real_mv=$(command -v mv)
if PATH="$fail_bin:$fake_bin:$PATH" \
  HOME="$work/home" \
  AOS_TEST_FIXTURE="$fixture" \
  AOS_VERSION=2026.1.3 \
  REAL_MV="$real_mv" \
  MV_FAILED="$work/mv-failed" \
  MV_FAIL_DESTINATION="$release_dir" \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer ignored a mid-install failure" >&2
  exit 1
fi
for binary in aos astrid astrid-daemon astrid-build astrid-emit; do
  case "$binary" in
    aos) destination="$work/home/.aos/bin/aos" ;;
    *) destination="$work/home/.aos/runtime/bin/$binary" ;;
  esac
  test "$("$destination")" = "old-$binary"
done
test "$(cat "$release_dir/release-manifest.json")" = old-release-manifest
test "$(find "$release_dir/capsules" -mindepth 1 -maxdepth 1 -type f | wc -l | tr -d ' ')" -eq 21
while IFS= read -r capsule; do
  cmp "$work/capsules/$capsule" "$release_dir/capsules/$capsule"
done < "$release_dir/capsule-assets.txt"
test "$(cat "$work/home/.astrid/sentinel")" = standalone-runtime-state

cat > "$work/aos-mismatch" <<'EOF'
#!/bin/sh
if [ "${1:-}" = --version ]; then
  echo 'Unicity AOS 2026.2.0'
fi
EOF
chmod 755 "$work/aos-mismatch"
bash "$repo_root/scripts/package-release.sh" \
  x86_64-unknown-linux-gnu \
  "$work/aos-mismatch" \
  "$work/runtime.tar.gz" \
  0000000000000000000000000000000000000000000000000000000000000000 \
  "$work/capsules" \
  "$fixture" >/dev/null
cp "$asset" "$signed_asset"
if PATH="$fake_bin:$PATH" HOME="$work/mismatch-home" AOS_TEST_FIXTURE="$fixture" AOS_VERSION=2026.1.3 \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer accepted a bundle whose binary version did not match the requested release" >&2
  exit 1
fi
test ! -e "$work/mismatch-home/.aos"

unsafe_root="$work/unsafe-bundle/unicity-aos-2026.1.3-x86_64-unknown-linux-gnu"
mkdir -p "$unsafe_root/bin" "$unsafe_root/runtime/bin"
ln -s "$work/aos" "$unsafe_root/bin/aos"
for binary in astrid astrid-daemon astrid-build astrid-emit; do
  cp "$runtime_root/$binary" "$unsafe_root/runtime/bin/$binary"
done
printf '{}\n' > "$unsafe_root/release-manifest.json"
COPYFILE_DISABLE=1 tar -czf "$fixture/unicity-aos-2026.1.3-x86_64-unknown-linux-gnu.tar.gz" \
  -C "$work/unsafe-bundle" "$(basename "$unsafe_root")"
cp "$asset" "$signed_asset"
if PATH="$fake_bin:$PATH" HOME="$work/unsafe-home" AOS_TEST_FIXTURE="$fixture" AOS_VERSION=2026.1.3 \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer accepted a symlink in the release archive" >&2
  exit 1
fi
test ! -e "$work/unsafe-home/.aos"
