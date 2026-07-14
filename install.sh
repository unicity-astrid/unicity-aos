#!/bin/sh
set -eu

AOS_RELEASE_REPO="${AOS_RELEASE_REPO:-unicity-aos/aos-ce}"
AOS_HOME="${UNICITY_AOS_HOME:-$HOME/.unicity-os}"
AOS_BIN_DIR="${AOS_BIN_DIR:-$AOS_HOME/bin}"
AOS_VERSION="${AOS_VERSION:-latest}"
COSIGN_VERSION=v3.1.1
ASSUME_YES=0
SKIP_MIGRATION_PROMPT=0
installation_started=0
rollback=

usage() {
  cat <<'EOF'
Install or upgrade Unicity AOS Community Edition.

Usage: install.sh [--yes] [--version VERSION] [--no-migrate-prompt]

  --yes                do not ask before replacing an existing installation
  --version VERSION    install a specific calendar-semver release
  --no-migrate-prompt  do not launch the optional Astrid state-import prompt
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    -y|--yes) ASSUME_YES=1 ;;
    --version)
      [ "$#" -ge 2 ] || { echo "missing value for --version" >&2; exit 2; }
      AOS_VERSION=$2
      shift
      ;;
    --no-migrate-prompt) SKIP_MIGRATION_PROMPT=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

case "$AOS_HOME" in
  /*) ;;
  *) echo "UNICITY_AOS_HOME must be an absolute path" >&2; exit 1 ;;
esac
case "$AOS_BIN_DIR" in
  /*) ;;
  *) echo "AOS_BIN_DIR must be an absolute path" >&2; exit 1 ;;
esac

need() { command -v "$1" >/dev/null 2>&1 || { echo "required command not found: $1" >&2; exit 1; }; }
need curl
need tar

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    echo "required command not found: sha256sum or shasum" >&2
    exit 1
  fi
}

os=$(uname -s)
arch=$(uname -m)
case "$os:$arch" in
  Darwin:arm64|Darwin:aarch64)
    target=aarch64-apple-darwin
    cosign_asset=cosign-darwin-arm64
    cosign_sha256=94b42a9e697be95675f6160ab031a9a5f1ec1e646d6f648d7b2f5cd59ececbc5
    ;;
  Darwin:x86_64)
    target=x86_64-apple-darwin
    cosign_asset=cosign-darwin-amd64
    cosign_sha256=14d2678dfbfde18798151e86fbd91ebdadbb7424b18412a42a155dd8a2df4c7a
    ;;
  Linux:aarch64|Linux:arm64)
    target=aarch64-unknown-linux-gnu
    cosign_asset=cosign-linux-arm64
    cosign_sha256=2ec865872e331c32fd12b08dae15332d3f92c0aa029219589684a4903ca85d11
    ;;
  Linux:x86_64|Linux:amd64)
    target=x86_64-unknown-linux-gnu
    cosign_asset=cosign-linux-amd64
    cosign_sha256=ae1ecd212663f3693ad9edf8b1a183900c9a52d3155ba6e354237f9a0f6463fc
    ;;
  *) echo "Unicity AOS does not publish a bundle for $os/$arch yet" >&2; exit 1 ;;
esac

asset="unicity-aos-${target}.tar.gz"
if [ "$AOS_VERSION" = latest ]; then
  base="https://github.com/${AOS_RELEASE_REPO}/releases/latest/download"
else
  if ! printf '%s\n' "$AOS_VERSION" | grep -Eq '^20[0-9]{2}\.[0-9]+\.[0-9]+$'; then
    echo "invalid AOS version: $AOS_VERSION" >&2
    exit 2
  fi
  base="https://github.com/${AOS_RELEASE_REPO}/releases/download/${AOS_VERSION}"
fi

work=$(mktemp -d "${TMPDIR:-/tmp}/unicity-aos-install.XXXXXX")
cleanup() {
  status=$?
  trap - EXIT HUP INT TERM
  if [ "$installation_started" -eq 1 ]; then
    restore || status=1
  fi
  rm -rf "$work"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

COSIGN_BIN="$work/cosign"
echo "Downloading the pinned Sigstore verifier..."
curl --proto '=https' --tlsv1.2 -fsSL \
  "https://github.com/sigstore/cosign/releases/download/${COSIGN_VERSION}/${cosign_asset}" \
  -o "$COSIGN_BIN"
[ ! -L "$COSIGN_BIN" ] || { echo "downloaded Sigstore verifier is a symlink" >&2; exit 1; }
[ "$(sha256_file "$COSIGN_BIN")" = "$cosign_sha256" ] || {
  echo "Sigstore verifier checksum mismatch" >&2
  exit 1
}
chmod 700 "$COSIGN_BIN"

echo "Downloading Unicity AOS for $target..."
curl --proto '=https' --tlsv1.2 -fsSL "$base/$asset" -o "$work/$asset"
curl --proto '=https' --tlsv1.2 -fsSL "$base/SHA256SUMS.txt" -o "$work/SHA256SUMS.txt"
curl --proto '=https' --tlsv1.2 -fsSL "$base/$asset.sigstore.json" -o "$work/$asset.sigstore.json"
"$COSIGN_BIN" verify-blob \
  --bundle "$work/$asset.sigstore.json" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-identity-regexp '^https://github.com/unicity-aos/aos-ce/.github/workflows/release.yml@refs/tags/20[0-9]{2}\.[0-9]+\.[0-9]+$' \
  "$work/$asset" >/dev/null

expected=$(awk -v asset="$asset" '
  {
    checksum_asset = $2
    sub(/^\*/, "", checksum_asset)
    sub(/^\.\//, "", checksum_asset)
    if (checksum_asset == asset) {
      print $1
      exit
    }
  }
' "$work/SHA256SUMS.txt")
[ -n "$expected" ] || { echo "release checksum list does not contain $asset" >&2; exit 1; }
actual=$(sha256_file "$work/$asset")
[ "$actual" = "$expected" ] || { echo "Unicity AOS archive checksum mismatch" >&2; exit 1; }

mkdir "$work/unpack"
if tar -tzf "$work/$asset" | awk -v target="$target" -v version="$AOS_VERSION" '
  {
    root = $0
    sub(/\/.*/, "", root)
    if (first == "") first = root
    if (root != first) unsafe = 1
    if (version == "latest") {
      if (root !~ ("^unicity-aos-20[0-9][0-9]\\.[0-9]+\\.[0-9]+-" target "$")) unsafe = 1
    } else if (root != "unicity-aos-" version "-" target) {
      unsafe = 1
    }
  }
  /^\// || /(^|\/)\.\.($|\/)/ { unsafe = 1 }
  END { exit unsafe ? 0 : 1 }
'; then
  echo "release archive contains an unsafe path or unexpected bundle root" >&2
  exit 1
fi
if tar -tvzf "$work/$asset" | awk '
  substr($1, 1, 1) != "-" && substr($1, 1, 1) != "d" { unsafe = 1 }
  END { exit unsafe ? 0 : 1 }
'; then
  echo "release archive contains a link or special file" >&2
  exit 1
fi
tar -xzf "$work/$asset" -C "$work/unpack"
bundle_name=$(tar -tzf "$work/$asset" | awk 'NR == 1 { sub(/\/.*/, "", $0); print; exit }')
[ -n "$bundle_name" ] || { echo "release archive has no Unicity AOS bundle" >&2; exit 1; }
bundle="$work/unpack/$bundle_name"
[ -d "$bundle" ] || { echo "release archive has no Unicity AOS bundle" >&2; exit 1; }

for file in bin/aos runtime/bin/astrid runtime/bin/astrid-daemon runtime/bin/astrid-build runtime/bin/astrid-emit release-manifest.json; do
  [ -f "$bundle/$file" ] || { echo "release archive is missing $file" >&2; exit 1; }
done

staged_version=$("$bundle/bin/aos" --version | awk '{print $NF}')
if ! printf '%s\n' "$staged_version" | grep -Eq '^20[0-9]{2}\.[0-9]+\.[0-9]+$'; then
  echo "staged AOS binary reported an invalid product version" >&2
  exit 1
fi
if [ "$AOS_VERSION" != latest ] && [ "$staged_version" != "$AOS_VERSION" ]; then
  echo "staged AOS version $staged_version does not match requested version $AOS_VERSION" >&2
  exit 1
fi
if [ "$bundle_name" != "unicity-aos-${staged_version}-${target}" ]; then
  echo "release bundle root does not match its product version and target" >&2
  exit 1
fi

for managed in "$AOS_HOME" "$AOS_HOME/runtime" "$AOS_HOME/runtime/bin" "$AOS_HOME/releases"; do
  [ ! -L "$managed" ] || { echo "refusing symlinked managed path: $managed" >&2; exit 1; }
done
if [ -L "$AOS_BIN_DIR" ]; then
  echo "refusing symlinked managed path: $AOS_BIN_DIR" >&2
  exit 1
fi
if [ -L "$AOS_BIN_DIR/aos" ] || { [ -e "$AOS_BIN_DIR/aos" ] && [ ! -f "$AOS_BIN_DIR/aos" ]; }; then
  echo "refusing non-regular install destination: $AOS_BIN_DIR/aos" >&2
  exit 1
fi

if [ -x "$AOS_BIN_DIR/aos" ] && [ "$ASSUME_YES" -ne 1 ] && [ -t 0 ]; then
  printf 'Replace the existing Unicity AOS installation? [y/N] '
  read -r answer
  case "$answer" in y|Y|yes|YES) ;; *) echo "Installation cancelled."; exit 0 ;; esac
fi

if [ -x "$AOS_BIN_DIR/aos" ]; then
  "$AOS_BIN_DIR/aos" stop >/dev/null 2>&1 || true
fi

mkdir -p "$AOS_BIN_DIR" "$AOS_HOME/runtime/bin" "$AOS_HOME/releases"
chmod 700 "$AOS_HOME" "$AOS_HOME/runtime" "$AOS_HOME/runtime/bin" "$AOS_HOME/releases"
if [ "$AOS_BIN_DIR" = "$AOS_HOME/bin" ]; then
  chmod 700 "$AOS_BIN_DIR"
fi
rollback="$work/rollback"
mkdir "$rollback"

install_one() {
  source=$1
  destination=$2
  name=$3
  mode=$4
  temporary="${destination}.new.$$"
  if [ -L "$destination" ] || { [ -e "$destination" ] && [ ! -f "$destination" ]; }; then
    echo "refusing non-regular install destination: $destination" >&2
    return 1
  fi
  if [ -f "$destination" ]; then
    cp -p "$destination" "$rollback/$name" || return 1
  fi
  cp "$source" "$temporary" || return 1
  chmod "$mode" "$temporary" || return 1
  : > "$rollback/$name.touched" || return 1
  mv -f "$temporary" "$destination" || return 1
}

restore() {
  result=0
  for name in aos astrid astrid-daemon astrid-build astrid-emit release-manifest; do
    case "$name" in
      aos) destination="$AOS_BIN_DIR/aos" ;;
      release-manifest) destination="$AOS_HOME/releases/${staged_version}.json" ;;
      *) destination="$AOS_HOME/runtime/bin/$name" ;;
    esac
    rm -f "${destination}.new.$$" "${destination}.restore.$$"
    if [ ! -f "$rollback/$name.touched" ]; then
      continue
    elif [ -f "$rollback/$name" ]; then
      temporary="${destination}.restore.$$"
      if ! cp -p "$rollback/$name" "$temporary" || ! mv -f "$temporary" "$destination"; then
        result=1
      fi
    else
      rm -f "$destination" || result=1
    fi
  done
  return "$result"
}

installation_started=1
if ! install_one "$bundle/bin/aos" "$AOS_BIN_DIR/aos" aos 755; then exit 1; fi
for name in astrid astrid-daemon astrid-build astrid-emit; do
  if ! install_one "$bundle/runtime/bin/$name" "$AOS_HOME/runtime/bin/$name" "$name" 755; then exit 1; fi
done
if ! install_one "$bundle/release-manifest.json" "$AOS_HOME/releases/${staged_version}.json" release-manifest 600; then exit 1; fi
installation_started=0

echo "Installed Unicity AOS $staged_version."
case ":$PATH:" in
  *":$AOS_BIN_DIR:"*) init_command="aos init" ;;
  *)
    echo "Add $AOS_BIN_DIR to PATH."
    init_command="$AOS_BIN_DIR/aos init"
    ;;
esac

if [ "$SKIP_MIGRATION_PROMPT" -ne 1 ] && [ -t 0 ]; then
  "$AOS_BIN_DIR/aos" || true
else
  echo "Run: $init_command"
fi
