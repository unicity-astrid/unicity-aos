#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fixture="$work/fixture"
fake_bin="$work/fake-bin"
mkdir -p "$fixture" "$fake_bin" "$work/home"
mkdir -p "$work/home/.astrid"
printf 'standalone-runtime-state\n' > "$work/home/.astrid/sentinel"

cat > "$work/aos" <<'EOF'
#!/bin/sh
if [ "${1:-}" = --version ]; then
  echo 'Unicity AOS 2026.1.0'
  exit 0
fi
exit 0
EOF
chmod 755 "$work/aos"

runtime_root="$work/astrid-0.9.4-x86_64-unknown-linux-gnu"
mkdir -p "$runtime_root"
for binary in astrid astrid-daemon astrid-build astrid-emit; do
  printf '#!/bin/sh\necho %s\n' "$binary" > "$runtime_root/$binary"
  chmod 755 "$runtime_root/$binary"
done
tar -czf "$work/runtime.tar.gz" -C "$work" "$(basename "$runtime_root")"
"$repo_root/scripts/package-release.sh" \
  x86_64-unknown-linux-gnu \
  "$work/aos" \
  "$work/runtime.tar.gz" \
  0000000000000000000000000000000000000000000000000000000000000000 \
  "$fixture" >/dev/null
(
  cd "$fixture"
  asset=unicity-aos-x86_64-unknown-linux-gnu.tar.gz
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$asset" > SHA256SUMS.txt
  else
    shasum -a 256 "$asset" > SHA256SUMS.txt
  fi
)
: > "$fixture/unicity-aos-x86_64-unknown-linux-gnu.tar.gz.sigstore.json"

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
cat > "$fake_bin/cosign" <<'EOF'
#!/bin/sh
set -eu
[ "${1:-}" = verify-blob ]
: > "$AOS_TEST_FIXTURE/cosign-called"
EOF
chmod 755 "$fake_bin/uname" "$fake_bin/curl" "$fake_bin/cosign"

PATH="$fake_bin:$PATH" \
HOME="$work/home" \
AOS_TEST_FIXTURE="$fixture" \
AOS_VERSION=2026.1.0 \
sh "$repo_root/install.sh" --yes --no-migrate-prompt

test -x "$work/home/.unicity-os/bin/aos"
test -x "$work/home/.unicity-os/runtime/bin/astrid-daemon"
test -f "$work/home/.unicity-os/releases/2026.1.0.json"
test "$("$work/home/.unicity-os/bin/aos" --version)" = 'Unicity AOS 2026.1.0'
test "$(cat "$work/home/.astrid/sentinel")" = 'standalone-runtime-state'
test -f "$fixture/cosign-called"
test "$(stat -c '%a' "$work/home/.unicity-os" 2>/dev/null || stat -f '%Lp' "$work/home/.unicity-os")" = 700
test "$(stat -c '%a' "$work/home/.unicity-os/releases/2026.1.0.json" 2>/dev/null || stat -f '%Lp' "$work/home/.unicity-os/releases/2026.1.0.json")" = 600

# Exercise the no-preinstalled-cosign path without downloading the 140 MB real
# verifier. The production digest stays hard-coded; only this fake hash command
# substitutes that digest for the tiny fixture executable.
bootstrap_bin="$work/bootstrap-bin"
mkdir "$bootstrap_bin"
if command -v sha256sum >/dev/null 2>&1; then
  real_hash=$(command -v sha256sum)
  hash_kind=sha256sum
else
  real_hash=$(command -v shasum)
  hash_kind=shasum
fi
cat > "$bootstrap_bin/sha256sum" <<'EOF'
#!/bin/sh
set -eu
case "$1" in
  */cosign)
    printf '%s  %s\n' ae1ecd212663f3693ad9edf8b1a183900c9a52d3155ba6e354237f9a0f6463fc "$1"
    ;;
  *)
    if [ "$HASH_KIND" = shasum ]; then
      exec "$REAL_HASH" -a 256 "$@"
    fi
    exec "$REAL_HASH" "$@"
    ;;
esac
EOF
chmod 755 "$bootstrap_bin/sha256sum"
mv "$fake_bin/cosign" "$fixture/cosign-linux-amd64"
rm -f "$fixture/cosign-called"
PATH="$bootstrap_bin:$fake_bin:/usr/bin:/bin" \
HOME="$work/bootstrap-home" \
AOS_TEST_FIXTURE="$fixture" \
AOS_VERSION=2026.1.0 \
REAL_HASH="$real_hash" \
HASH_KIND="$hash_kind" \
sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null
test -f "$fixture/cosign-called"
mv "$fixture/cosign-linux-amd64" "$fake_bin/cosign"

cp "$fixture/SHA256SUMS.txt" "$work/good-sums"
printf '%064d  unicity-aos-x86_64-unknown-linux-gnu.tar.gz\n' 1 > "$fixture/SHA256SUMS.txt"
if PATH="$fake_bin:$PATH" HOME="$work/other-home" AOS_TEST_FIXTURE="$fixture" AOS_VERSION=2026.1.0 \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer accepted a bad checksum" >&2
  exit 1
fi
cp "$work/good-sums" "$fixture/SHA256SUMS.txt"

symlink_home="$work/symlink-destination-home"
mkdir -p "$symlink_home/.unicity-os/bin"
printf 'outside-install-root\n' > "$work/symlink-target"
ln -s "$work/symlink-target" "$symlink_home/.unicity-os/bin/aos"
if PATH="$fake_bin:$PATH" HOME="$symlink_home" AOS_TEST_FIXTURE="$fixture" AOS_VERSION=2026.1.0 \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer replaced a symlinked destination" >&2
  exit 1
fi
test -L "$symlink_home/.unicity-os/bin/aos"
test "$(cat "$work/symlink-target")" = outside-install-root

directory_home="$work/directory-destination-home"
mkdir -p "$directory_home/.unicity-os/bin/aos"
if PATH="$fake_bin:$PATH" HOME="$directory_home" AOS_TEST_FIXTURE="$fixture" AOS_VERSION=2026.1.0 \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer replaced a directory destination" >&2
  exit 1
fi
test -d "$directory_home/.unicity-os/bin/aos"

for binary in aos astrid astrid-daemon astrid-build astrid-emit; do
  case "$binary" in
    aos) destination="$work/home/.unicity-os/bin/aos" ;;
    *) destination="$work/home/.unicity-os/runtime/bin/$binary" ;;
  esac
  printf '#!/bin/sh\necho old-%s\n' "$binary" > "$destination"
  chmod 755 "$destination"
done
printf 'old-release-manifest\n' > "$work/home/.unicity-os/releases/2026.1.0.json"

fail_bin="$work/fail-bin"
mkdir "$fail_bin"
cat > "$fail_bin/mv" <<'EOF'
#!/bin/sh
set -eu
last=
for argument in "$@"; do last=$argument; done
case "$last" in
  */runtime/bin/astrid-daemon)
    if [ ! -f "$MV_FAILED" ]; then
      : > "$MV_FAILED"
      exit 1
    fi
    ;;
esac
exec "$REAL_MV" "$@"
EOF
chmod 755 "$fail_bin/mv"
real_mv=$(command -v mv)
if PATH="$fail_bin:$fake_bin:$PATH" \
  HOME="$work/home" \
  AOS_TEST_FIXTURE="$fixture" \
  AOS_VERSION=2026.1.0 \
  REAL_MV="$real_mv" \
  MV_FAILED="$work/mv-failed" \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer ignored a mid-install failure" >&2
  exit 1
fi
for binary in aos astrid astrid-daemon astrid-build astrid-emit; do
  case "$binary" in
    aos) destination="$work/home/.unicity-os/bin/aos" ;;
    *) destination="$work/home/.unicity-os/runtime/bin/$binary" ;;
  esac
  test "$("$destination")" = "old-$binary"
done
test "$(cat "$work/home/.unicity-os/releases/2026.1.0.json")" = old-release-manifest
test "$(cat "$work/home/.astrid/sentinel")" = standalone-runtime-state

cat > "$work/aos-mismatch" <<'EOF'
#!/bin/sh
if [ "${1:-}" = --version ]; then
  echo 'Unicity AOS 2026.2.0'
fi
EOF
chmod 755 "$work/aos-mismatch"
"$repo_root/scripts/package-release.sh" \
  x86_64-unknown-linux-gnu \
  "$work/aos-mismatch" \
  "$work/runtime.tar.gz" \
  0000000000000000000000000000000000000000000000000000000000000000 \
  "$fixture" >/dev/null
(
  cd "$fixture"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum unicity-aos-x86_64-unknown-linux-gnu.tar.gz > SHA256SUMS.txt
  else
    shasum -a 256 unicity-aos-x86_64-unknown-linux-gnu.tar.gz > SHA256SUMS.txt
  fi
)
if PATH="$fake_bin:$PATH" HOME="$work/mismatch-home" AOS_TEST_FIXTURE="$fixture" AOS_VERSION=2026.1.0 \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer accepted a bundle whose binary version did not match the requested release" >&2
  exit 1
fi
test ! -e "$work/mismatch-home/.unicity-os"

unsafe_root="$work/unsafe-bundle/unicity-aos-2026.1.0-x86_64-unknown-linux-gnu"
mkdir -p "$unsafe_root/bin" "$unsafe_root/runtime/bin"
ln -s "$work/aos" "$unsafe_root/bin/aos"
for binary in astrid astrid-daemon astrid-build astrid-emit; do
  cp "$runtime_root/$binary" "$unsafe_root/runtime/bin/$binary"
done
printf '{}\n' > "$unsafe_root/release-manifest.json"
tar -czf "$fixture/unicity-aos-x86_64-unknown-linux-gnu.tar.gz" \
  -C "$work/unsafe-bundle" "$(basename "$unsafe_root")"
(
  cd "$fixture"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum unicity-aos-x86_64-unknown-linux-gnu.tar.gz > SHA256SUMS.txt
  else
    shasum -a 256 unicity-aos-x86_64-unknown-linux-gnu.tar.gz > SHA256SUMS.txt
  fi
)
if PATH="$fake_bin:$PATH" HOME="$work/unsafe-home" AOS_TEST_FIXTURE="$fixture" AOS_VERSION=2026.1.0 \
  sh "$repo_root/install.sh" --yes --no-migrate-prompt >/dev/null 2>&1; then
  echo "installer accepted a symlink in the release archive" >&2
  exit 1
fi
test ! -e "$work/unsafe-home/.unicity-os"
