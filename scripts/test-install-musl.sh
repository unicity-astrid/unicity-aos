#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fixture="$work/fixture"
fake_bin="$work/fake-bin"
capsules="$work/capsules"
mkdir -p "$fixture" "$fake_bin" "$capsules"

read -r product_version runtime_version < <(
  python3 - "$repo_root" <<'PY'
import pathlib
import sys
import tomllib

root = pathlib.Path(sys.argv[1])
with (root / "crates/unicity-aos-bootstrap/Cargo.toml").open("rb") as source:
    product = tomllib.load(source)["package"]["version"]
with (root / "release/runtime-compatibility.toml").open("rb") as source:
    runtime = tomllib.load(source)["runtime"]
print(product, runtime["version"])
PY
)
target=x86_64-unknown-linux-musl
runtime_root="$work/astrid-$runtime_version-$target"
mkdir -p "$runtime_root"
for binary in astrid astrid-daemon astrid-build astrid-emit; do
  printf '#!/bin/sh\nexit 0\n' > "$runtime_root/$binary"
  chmod 755 "$runtime_root/$binary"
done
COPYFILE_DISABLE=1 tar -czf "$work/runtime.tar.gz" -C "$work" "$(basename "$runtime_root")"
cat > "$work/aos" <<EOF
#!/bin/sh
if [ "\${1:-}" = --version ]; then
  echo "Unicity AOS $product_version"
fi
exit 0
EOF
chmod 755 "$work/aos"
PYTHONPATH="$repo_root/scripts" python3 - "$capsules" <<'PY'
import pathlib
import sys

from capsule_release import source_contract
from test_capsule_release import write_fixture

output = pathlib.Path(sys.argv[1])
for spec in source_contract():
    write_fixture(output / spec.asset, spec)
PY
runtime_musl="$work/runtime-musl.toml"
cat > "$runtime_musl" <<EOF
schema-version = 1

[runtime]
repository = "astrid-runtime/astrid"
release-ready = true
version = "$runtime_version"
tag = "v$runtime_version"
release-workflow-identity = "https://github.com/astrid-runtime/astrid/.github/workflows/release.yml@refs/tags/v$runtime_version"
source-commit = "$(printf 'b%.0s' {1..40})"
legacy-release-metadata-asset = "astrid-$runtime_version-release.toml"
legacy-release-metadata-blake3 = "$(printf 'c%.0s' {1..64})"
musl-release-metadata-asset = "astrid-$runtime_version-musl-release.toml"
musl-release-metadata-blake3 = "$(printf 'd%.0s' {1..64})"
EOF
if bash "$repo_root/scripts/package-release.sh" \
  "$target" \
  "$work/aos" \
  "$work/runtime.tar.gz" \
  "$(printf '0%.0s' {1..64})" \
  "$capsules" \
  "$fixture" >/dev/null 2>&1; then
  echo "musl release composer accepted the checked-in false readiness gate" >&2
  exit 1
fi
malformed_runtime_musl="$work/runtime-musl-malformed.toml"
sed \
  's/musl-release-metadata-blake3 = "[0-9a-f]*"/musl-release-metadata-blake3 = "BAD"/' \
  "$runtime_musl" > "$malformed_runtime_musl"
malformed_output="$work/malformed-package"
mkdir "$malformed_output"
if bash "$repo_root/scripts/package-release.sh" \
  "$target" \
  "$work/aos" \
  "$work/runtime.tar.gz" \
  "$(printf '0%.0s' {1..64})" \
  "$capsules" \
  "$malformed_output" \
  --musl-runtime-compatibility "$malformed_runtime_musl" \
  >"$work/malformed-package.log" 2>&1; then
  echo "musl release composer accepted a malformed ready compatibility pin" >&2
  exit 1
fi
grep -F 'BLAKE3 is malformed' "$work/malformed-package.log" >/dev/null
test -z "$(find "$malformed_output" -mindepth 1 -print -quit)"
bash "$repo_root/scripts/package-release.sh" \
  "$target" \
  "$work/aos" \
  "$work/runtime.tar.gz" \
  "$(printf '0%.0s' {1..64})" \
  "$capsules" \
  "$fixture" \
  --musl-runtime-compatibility "$runtime_musl" >/dev/null
tar -xOf \
  "$fixture/unicity-aos-$product_version-$target.tar.gz" \
  "unicity-aos-$product_version-$target/runtime-musl-compatibility.toml" \
  | grep -Fx 'release-ready = true' >/dev/null

for metadata_target in \
  aarch64-apple-darwin \
  x86_64-apple-darwin \
  aarch64-unknown-linux-gnu \
  x86_64-unknown-linux-gnu \
  aarch64-unknown-linux-musl; do
  printf 'fixture:%s\n' "$metadata_target" \
    > "$fixture/unicity-aos-$product_version-$metadata_target.tar.gz"
done
(
  cd "$fixture"
  b3sum -- ./*.tar.gz | sed 's#  \./#  #' > BLAKE3SUMS.txt
  shasum -a 256 -- ./*.tar.gz | sed 's#  \./#  #' > SHA256SUMS.txt
)
legacy="$fixture/unicity-aos-$product_version-release.toml"
python3 "$repo_root/scripts/release_metadata.py" render-release \
  --version "$product_version" \
  --tag "$product_version" \
  --source-commit "$(printf 'a%.0s' {1..40})" \
  --published-at 2026-07-16T10:00:00Z \
  --artifacts "$fixture" \
  --sha256 "$fixture/SHA256SUMS.txt" \
  --blake3 "$fixture/BLAKE3SUMS.txt" \
  --output "$legacy"

extension="$fixture/unicity-aos-$product_version-musl-release.toml"
python3 "$repo_root/scripts/musl_release_metadata.py" render \
  --artifacts "$fixture" \
  --legacy-release "$legacy" \
  --runtime-compatibility "$runtime_musl" \
  --output "$extension"

python3 "$repo_root/scripts/release_metadata.py" render-channel \
  --channel stable \
  --generation 2 \
  --published-at 2026-07-16T10:00:00Z \
  --expires-at 2026-08-15T10:00:00Z \
  --release-metadata "$legacy" \
  --require-ready \
  --output "$fixture/channel.toml"

printf 'valid Sigstore fixture\n' > "$fixture/valid.sigstore.json"
for signed in \
  "$legacy" \
  "$extension" \
  "$fixture/channel.toml" \
  "$fixture/unicity-aos-$product_version-$target.tar.gz"; do
  cp "$fixture/valid.sigstore.json" "$signed.sigstore.json"
done

cat > "$fake_bin/uname" <<'EOF'
#!/bin/sh
case "${1:-}" in
  -s) echo Linux ;;
  -m) echo x86_64 ;;
  *) exit 2 ;;
esac
EOF
cat > "$fake_bin/getconf" <<'EOF'
#!/bin/sh
exit 1
EOF
cat > "$fake_bin/ldd" <<'EOF'
#!/bin/sh
printf '%s\n' 'musl libc (x86_64)' >&2
exit 1
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
    --bundle) bundle=$2; shift ;;
    --certificate-identity) identity=$2; shift ;;
    -*) ;;
    *) artifact=$1 ;;
  esac
  shift
done
cmp "$AOS_TEST_FIXTURE/valid.sigstore.json" "$bundle"
[ -f "$artifact" ]
printf '%s|%s\n' "$(basename "$artifact")" "$identity" >> "$AOS_TEST_FIXTURE/cosign-trace"
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
chmod 755 "$fake_bin"/* "$fixture/cosign-linux-amd64"

run_installer() {
  local home=$1
  shift
  PATH="$fake_bin:/usr/bin:/bin" \
    HOME="$home" \
    AOS_TEST_FIXTURE="$fixture" \
    sh "$repo_root/install.sh" --yes --no-migrate-prompt "$@"
}

direct_home="$work/direct-home"
run_installer "$direct_home" --version "$product_version" >/dev/null
test -x "$direct_home/.aos/bin/aos"
test -x "$direct_home/.aos/runtime/bin/astrid-daemon"
direct_trace="$work/direct-trace"
cp "$fixture/cosign-trace" "$direct_trace"
test "$(sed -n '1s/|.*//p' "$direct_trace")" = "unicity-aos-$product_version-release.toml"
test "$(sed -n '2s/|.*//p' "$direct_trace")" = "unicity-aos-$product_version-musl-release.toml"
test "$(sed -n '3s/|.*//p' "$direct_trace")" = "unicity-aos-$product_version-$target.tar.gz"

: > "$fixture/cosign-trace"
channel_home="$work/channel-home"
run_installer "$channel_home" >/dev/null
test -x "$channel_home/.aos/bin/aos"
test "$(sed -n '1s/|.*//p' "$fixture/cosign-trace")" = channel.toml
test "$(sed -n '2s/|.*//p' "$fixture/cosign-trace")" = "unicity-aos-$product_version-release.toml"
test "$(sed -n '3s/|.*//p' "$fixture/cosign-trace")" = "unicity-aos-$product_version-musl-release.toml"
test "$(sed -n '4s/|.*//p' "$fixture/cosign-trace")" = "unicity-aos-$product_version-$target.tar.gz"
test "$(cat "$channel_home/.aos/update/channels/stable/current")" = 2

cp "$fixture/valid.sigstore.json" "$extension.sigstore.json"
printf 'invalid Sigstore fixture\n' > "$extension.sigstore.json"
if run_installer "$work/bad-signature-home" --version "$product_version" >/dev/null 2>&1; then
  echo "musl installer accepted an invalid extension signature" >&2
  exit 1
fi
test ! -e "$work/bad-signature-home/.aos/bin/aos"
cp "$fixture/valid.sigstore.json" "$extension.sigstore.json"

good_extension="$work/extension-good.toml"
cp "$extension" "$good_extension"
sed -i.bak 's/metadata-sha256 = "[0-9a-f]*"/metadata-sha256 = "0000000000000000000000000000000000000000000000000000000000000000"/' "$extension"
rm "$extension.bak"
if run_installer "$work/bad-link-home" --version "$product_version" >/dev/null 2>&1; then
  echo "musl installer accepted an extension bound to different legacy bytes" >&2
  exit 1
fi
test ! -e "$work/bad-link-home/.aos/bin/aos"
cp "$good_extension" "$extension"

sed -i.bak 's/tag = "v[0-9.]*"/tag = "v9.9.9"/' "$extension"
rm "$extension.bak"
if run_installer "$work/bad-runtime-home" --version "$product_version" >/dev/null 2>&1; then
  echo "musl installer accepted a mismatched runtime identity" >&2
  exit 1
fi
test ! -e "$work/bad-runtime-home/.aos/bin/aos"
cp "$good_extension" "$extension"

printf '\nsurprise = "value"\n' >> "$extension"
if run_installer "$work/malformed-home" --version "$product_version" >/dev/null 2>&1; then
  echo "musl installer accepted malformed extension metadata" >&2
  exit 1
fi
test ! -e "$work/malformed-home/.aos/bin/aos"
