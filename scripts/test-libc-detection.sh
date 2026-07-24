#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fake_bin="$work/bin"
mkdir -p "$fake_bin"

awk '
  /^detect_linux_libc\(\) \{/ { copying = 1 }
  copying { print }
  copying && /^}$/ { exit }
' "$repo_root/install.sh" > "$work/detect.sh"
# shellcheck source=/dev/null
source "$work/detect.sh"

cat > "$fake_bin/getconf" <<'EOF'
#!/bin/sh
case "${AOS_TEST_GETCONF:-missing}" in
  glibc) printf '%s\n' 'glibc 2.34' ;;
  unknown) printf '%s\n' 'unsupported libc' ;;
  missing) exit 127 ;;
esac
EOF
cat > "$fake_bin/ldd" <<'EOF'
#!/bin/sh
case "${AOS_TEST_LDD:-missing}" in
  glibc) printf '%s\n' 'ldd (GNU libc) 2.34' ;;
  musl) printf '%s\n' 'musl libc (x86_64)' >&2 ;;
  unknown) printf '%s\n' 'unknown dynamic loader' ;;
  missing) exit 127 ;;
esac
EOF
chmod 755 "$fake_bin/getconf" "$fake_bin/ldd"

PATH="$fake_bin:/usr/bin:/bin" AOS_TEST_GETCONF=glibc AOS_TEST_LDD=musl \
  test "$(PATH="$fake_bin:/usr/bin:/bin" AOS_TEST_GETCONF=glibc AOS_TEST_LDD=musl detect_linux_libc)" = gnu
PATH="$fake_bin:/usr/bin:/bin" AOS_TEST_GETCONF=unknown AOS_TEST_LDD=glibc \
  test "$(PATH="$fake_bin:/usr/bin:/bin" AOS_TEST_GETCONF=unknown AOS_TEST_LDD=glibc detect_linux_libc)" = gnu
PATH="$fake_bin:/usr/bin:/bin" AOS_TEST_GETCONF=missing AOS_TEST_LDD=musl \
  test "$(PATH="$fake_bin:/usr/bin:/bin" AOS_TEST_GETCONF=missing AOS_TEST_LDD=musl detect_linux_libc)" = musl

loader_root="$work/root"
mkdir -p "$loader_root/lib"
touch "$loader_root/lib/ld-musl-x86_64.so.1"
test "$(PATH="$fake_bin:/usr/bin:/bin" AOS_TEST_GETCONF=missing AOS_TEST_LDD=missing detect_linux_libc "$loader_root")" = musl

if PATH="$fake_bin:/usr/bin:/bin" AOS_TEST_GETCONF=unknown AOS_TEST_LDD=unknown \
  detect_linux_libc "$work/no-loader" >/dev/null; then
  echo "libc detection guessed an unknown Linux libc" >&2
  exit 1
fi
