#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <signed-release-artifacts>" >&2
  exit 2
fi

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
artifacts=$(cd "$1" && pwd -P)
version=$(python3 - "$repo_root/crates/unicity-aos-bootstrap/Cargo.toml" <<'PY'
import pathlib
import sys
import tomllib

with pathlib.Path(sys.argv[1]).open("rb") as source:
    print(tomllib.load(source)["package"]["version"])
PY
)
target=x86_64-unknown-linux-musl
legacy="unicity-aos-${version}-release.toml"
extension="unicity-aos-${version}-musl-release.toml"
archive="unicity-aos-${version}-${target}.tar.gz"
cosign=$(command -v cosign)
expected_cosign_sha256=ae1ecd212663f3693ad9edf8b1a183900c9a52d3155ba6e354237f9a0f6463fc
actual_cosign_sha256=$(sha256sum -- "$cosign" | awk '{print $1}')
[[ "$actual_cosign_sha256" == "$expected_cosign_sha256" ]] || {
  echo "release smoke requires the installer's pinned cosign v3.1.1 binary" >&2
  exit 1
}

PYTHONPATH="$repo_root/scripts" python3 - "$repo_root" <<'PY'
import pathlib
import sys

import musl_release_metadata
import release_metadata

root = pathlib.Path(sys.argv[1])
musl_release_metadata.validate_runtime_pin(
    release_metadata.load(root / "release/runtime-musl-compatibility.toml"),
    require_ready=True,
)
PY

for required in \
  "$legacy" \
  "$legacy.sigstore.json" \
  "$extension" \
  "$extension.sigstore.json" \
  "$archive" \
  "$archive.sigstore.json"; do
  [[ -f "$artifacts/$required" && ! -L "$artifacts/$required" ]] || {
    echo "signed musl installer smoke is missing $required" >&2
    exit 1
  }
done

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p \
  "$work/good" \
  "$work/bad-legacy" \
  "$work/bad-extension" \
  "$work/fake-bin"
for fixture in good bad-legacy bad-extension; do
  cp \
    "$artifacts/$legacy" \
    "$artifacts/$legacy.sigstore.json" \
    "$artifacts/$extension" \
    "$artifacts/$extension.sigstore.json" \
    "$artifacts/$archive" \
    "$artifacts/$archive.sigstore.json" \
    "$work/$fixture/"
  cp "$cosign" "$work/$fixture/cosign-linux-amd64"
done
printf '\n# signature-invalidating tamper\n' >> "$work/bad-legacy/$legacy"
printf '\n# signature-invalidating tamper\n' >> "$work/bad-extension/$extension"

cat > "$work/fake-bin/curl" <<'EOF'
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
[ -n "$output" ] && [ -n "$url" ]
asset=${url##*/}
printf '%s\n' "$asset" >> "$AOS_INSTALL_TRACE"
[ -f "$AOS_INSTALL_FIXTURE/$asset" ]
cp "$AOS_INSTALL_FIXTURE/$asset" "$output"
EOF
chmod 755 "$work/fake-bin/curl"

image=docker.io/library/rust@sha256:e98196986adced5602f6e21c54babdbf2a8700400c7a78868324a3630e0c5d15
docker run --rm \
  --platform linux/amd64 \
  --env AOS_HOME=/work/good-home/.aos \
  --env AOS_INSTALL_FIXTURE=/work/good \
  --env AOS_INSTALL_TRACE=/work/good-trace \
  --env HOME=/work/good-home \
  --env PATH=/work/fake-bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
  --volume "$repo_root:/workspace:ro" \
  --volume "$work:/work" \
  --workdir /workspace \
  "$image" \
  sh -ceu '
    test "$(uname -m)" = x86_64
    apk add --no-cache bash bubblewrap python3 zsh
    sh install.sh --yes --no-migrate-prompt --version "'"$version"'"
    bash scripts/test-clean-home-init.sh \
      "/work/good-home/.aos/releases/'"$version"'"
  '

cat > "$work/expected-good-trace" <<EOF
cosign-linux-amd64
$legacy
$legacy.sigstore.json
$extension
$extension.sigstore.json
$archive
$archive.sigstore.json
EOF
cmp "$work/expected-good-trace" "$work/good-trace"

run_negative_install() {
  local fixture=$1
  if docker run --rm \
    --platform linux/amd64 \
    --env "AOS_HOME=/work/${fixture}-home/.aos" \
    --env "AOS_INSTALL_FIXTURE=/work/$fixture" \
    --env "AOS_INSTALL_TRACE=/work/${fixture}-trace" \
    --env "HOME=/work/${fixture}-home" \
    --env PATH=/work/fake-bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    --volume "$repo_root:/workspace:ro" \
    --volume "$work:/work" \
    --workdir /workspace \
    "$image" \
    sh -ceu '
      test "$(uname -m)" = x86_64
      sh install.sh --yes --no-migrate-prompt --version "'"$version"'"
    ' >"$work/${fixture}-install.log" 2>&1; then
    echo "clean Alpine installer accepted tampered $fixture metadata" >&2
    return 1
  fi
  [[ ! -x "$work/${fixture}-home/.aos/bin/aos" ]]
}

run_negative_install bad-legacy
cat > "$work/expected-bad-legacy-trace" <<EOF
cosign-linux-amd64
$legacy
$legacy.sigstore.json
EOF
cmp "$work/expected-bad-legacy-trace" "$work/bad-legacy-trace"

run_negative_install bad-extension
cat > "$work/expected-bad-extension-trace" <<EOF
cosign-linux-amd64
$legacy
$legacy.sigstore.json
$extension
$extension.sigstore.json
EOF
cmp "$work/expected-bad-extension-trace" "$work/bad-extension-trace"

if grep -Fx "$archive" "$work/bad-extension-trace" >/dev/null; then
  echo "installer downloaded the archive before authenticating musl metadata" >&2
  exit 1
fi

echo "signed musl installer authenticated metadata in order and initialized a clean Alpine home"
