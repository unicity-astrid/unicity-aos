#!/bin/sh
set -eu

AOS_RELEASE_REPO="${AOS_RELEASE_REPO:-unicity-aos/aos-ce}"
AOS_TRUSTED_RELEASE_REPO=unicity-aos/aos-ce
AOS_CHANNEL_INPUT="${AOS_CHANNEL-}"
AOS_VERSION_INPUT="${AOS_VERSION-}"
AOS_CHANNEL="${AOS_CHANNEL_INPUT:-stable}"
AOS_HOME="${AOS_HOME:-$HOME/.aos}"
AOS_BIN_DIR="${AOS_BIN_DIR:-$AOS_HOME/bin}"
AOS_VERSION="$AOS_VERSION_INPUT"
AOS_CHANNEL_BASE_URL="${AOS_CHANNEL_BASE_URL:-https://github.com/${AOS_RELEASE_REPO}/releases/download}"
COSIGN_VERSION=v3.1.1
ASSUME_YES=0
SKIP_MIGRATION_PROMPT=0
channel_explicit=0
version_explicit=0
installation_started=0
release_committed=0
release_backup=
rollback=
channel_root=
channel_current_path=
channel_generation_dir=
install_lock=
install_lock_acquired=0

[ -z "$AOS_CHANNEL_INPUT" ] || channel_explicit=1
if [ "$AOS_VERSION" = latest ]; then
  AOS_VERSION=
elif [ -n "$AOS_VERSION" ]; then
  version_explicit=1
fi

usage() {
  cat <<'EOF'
Install or upgrade Unicity AOS Community Edition.

Usage: install.sh [--yes] [--channel CHANNEL | --version VERSION] [--no-migrate-prompt]

  --yes                do not ask before replacing an existing installation
  --channel CHANNEL    follow the signed stable, dev, or nightly channel
  --version VERSION    install a specific calendar-semver release
  --no-migrate-prompt  do not launch the optional Astrid state-import prompt
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    -y|--yes) ASSUME_YES=1 ;;
    --channel)
      [ "$#" -ge 2 ] || { echo "missing value for --channel" >&2; exit 2; }
      AOS_CHANNEL=$2
      channel_explicit=1
      shift
      ;;
    --version)
      [ "$#" -ge 2 ] || { echo "missing value for --version" >&2; exit 2; }
      AOS_VERSION=$2
      version_explicit=1
      shift
      ;;
    --no-migrate-prompt) SKIP_MIGRATION_PROMPT=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

if [ "$channel_explicit" -eq 1 ] && [ "$version_explicit" -eq 1 ]; then
  echo "--channel and --version are mutually exclusive" >&2
  exit 2
fi
case "$AOS_CHANNEL" in
  stable|dev|nightly) ;;
  *) echo "invalid AOS channel: $AOS_CHANNEL" >&2; exit 2 ;;
esac
is_aos_nightly_version() {
  printf '%s\n' "$1" | grep -Eq '^(202[6-9]|20[3-9][0-9])\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)-nightly\.[0-9]{8}\.g[0-9a-f]{40}$' || return 1
  nightly_date=${1#*-nightly.}
  nightly_date=${nightly_date%%.g*}
  year=$(printf '%.4s' "$nightly_date")
  month_day=${nightly_date#????}
  month_text=$(printf '%.2s' "$month_day")
  day_text=${month_day#??}
  month=${month_text#0}
  day=${day_text#0}
  [ -n "$month" ] || month=0
  [ -n "$day" ] || day=0
  [ "$month" -ge 1 ] && [ "$month" -le 12 ] && [ "$day" -ge 1 ] || return 1
  case "$month" in
    1|3|5|7|8|10|12) max_day=31 ;;
    4|6|9|11) max_day=30 ;;
    2)
      max_day=28
      if { [ $((year % 4)) -eq 0 ] && [ $((year % 100)) -ne 0 ]; } || [ $((year % 400)) -eq 0 ]; then
        max_day=29
      fi
      ;;
  esac
  [ "$day" -le "$max_day" ]
}

is_aos_release_version() {
  printf '%s\n' "$1" | grep -Eq '^(202[6-9]|20[3-9][0-9])\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' \
    || is_aos_nightly_version "$1"
}

if [ -n "$AOS_VERSION" ] && ! is_aos_release_version "$AOS_VERSION"; then
  echo "invalid AOS version: $AOS_VERSION" >&2
  exit 2
fi

case "$AOS_HOME" in
  /*) ;;
  *) echo "AOS_HOME must be an absolute path" >&2; exit 1 ;;
esac
case "$AOS_BIN_DIR" in
  /*) ;;
  *) echo "AOS_BIN_DIR must be an absolute path" >&2; exit 1 ;;
esac

need() { command -v "$1" >/dev/null 2>&1 || { echo "required command not found: $1" >&2; exit 1; }; }
need curl
need cmp
need tar
need awk
need find
need grep
need install
need sort
need sync
need tr
need wc

has_interactive_tty() {
  { [ -t 1 ] || [ -t 2 ]; } && : 2>/dev/null </dev/tty && : 2>/dev/null >/dev/tty
}

prompt_from_tty() {
  prompt=$1
  if ! printf '%s' "$prompt" 2>/dev/null >/dev/tty; then
    return 1
  fi
  if ! IFS= read -r answer 2>/dev/null </dev/tty; then
    return 1
  fi
}

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

utc_before() {
  awk -v left="$1" -v right="$2" 'BEGIN { exit left < right ? 0 : 1 }'
}

utc_before_or_equal() {
  awk -v left="$1" -v right="$2" 'BEGIN { exit left <= right ? 0 : 1 }'
}

utc_epoch() {
  value=$1
  date -u -d "$value" '+%s' 2>/dev/null ||
    date -j -u -f '%Y-%m-%dT%H:%M:%SZ' "$value" '+%s' 2>/dev/null
}

toml_value() {
  metadata=$1
  wanted_section=$2
  wanted_key=$3
  awk -v wanted_section="$wanted_section" -v wanted_key="$wanted_key" '
    /^\[/ { section = $0; next }
    section == wanted_section && $1 == wanted_key && $2 == "=" {
      value = substr($0, index($0, "=") + 1)
      sub(/^[[:space:]]+/, "", value)
      if (value ~ /^"/) {
        sub(/^"/, "", value)
        sub(/"$/, "", value)
      }
      print value
      exit
    }
  ' "$metadata"
}

toml_raw_value() {
  metadata=$1
  wanted_section=$2
  wanted_key=$3
  awk -v wanted_section="$wanted_section" -v wanted_key="$wanted_key" '
    /^\[/ { section = $0; next }
    section == wanted_section && $1 == wanted_key && $2 == "=" {
      value = substr($0, index($0, "=") + 1)
      sub(/^[[:space:]]+/, "", value)
      print value
      exit
    }
  ' "$metadata"
}

require_toml_string() {
  raw=$(toml_raw_value "$1" "$2" "$3")
  printf '%s\n' "$raw" | grep -Eq '^"[^"\\]*"$'
}

require_toml_integer() {
  raw=$(toml_raw_value "$1" "$2" "$3")
  printf '%s\n' "$raw" | grep -Eq "$4"
}

require_toml_boolean() {
  raw=$(toml_raw_value "$1" "$2" "$3")
  [ "$raw" = true ] || [ "$raw" = false ]
}

validate_channel_metadata() {
  metadata=$1
  expected_channel=$2
  check_expiry=${3:-1}
  awk '
    function mark(name) { if (seen[name]++) bad = 1 }
    /^$/ { next }
    /^\[/ {
      section = $0
      if (section == "[release]") mark(section)
      else if (section ~ /^\[targets\.(aarch64-apple-darwin|x86_64-apple-darwin|aarch64-unknown-linux-gnu|x86_64-unknown-linux-gnu)\]$/) mark(section)
      else bad = 1
      next
    }
    {
      key = $1
      if ($2 != "=") { bad = 1; next }
      if (section == "") {
        if (key !~ /^(schema-version|kind|product|channel|generation|published-at|expires-at)$/) bad = 1
      } else if (section == "[release]") {
        if (key !~ /^(repository|version|tag|source-commit|metadata-asset|metadata-sha256|release-workflow-identity)$/) bad = 1
      } else if (section ~ /^\[targets\./) {
        if (key !~ /^(asset|sha256|blake3|sigstore-bundle|size)$/) bad = 1
      } else bad = 1
      mark(section SUBSEP key)
    }
    END {
      required_top = "schema-version kind product channel generation published-at expires-at"
      split(required_top, top)
      for (i in top) if (seen[SUBSEP top[i]] != 1) bad = 1
      required_release = "repository version tag source-commit metadata-asset metadata-sha256 release-workflow-identity"
      split(required_release, release)
      for (i in release) if (seen["[release]" SUBSEP release[i]] != 1) bad = 1
      targets[1] = "aarch64-apple-darwin"
      targets[2] = "x86_64-apple-darwin"
      targets[3] = "aarch64-unknown-linux-gnu"
      targets[4] = "x86_64-unknown-linux-gnu"
      required_target = "asset sha256 blake3 sigstore-bundle size"
      split(required_target, target_keys)
      for (t in targets) {
        target_section = "[targets." targets[t] "]"
        if (seen[target_section] != 1) bad = 1
        for (i in target_keys) if (seen[target_section SUBSEP target_keys[i]] != 1) bad = 1
      }
      exit bad ? 1 : 0
    }
  ' "$metadata" || { echo "signed channel metadata does not match schema 1" >&2; return 1; }

  require_toml_integer "$metadata" "" schema-version '^1$' || return 1
  for key in kind product channel published-at expires-at; do
    require_toml_string "$metadata" "" "$key" || return 1
  done
  require_toml_integer "$metadata" "" generation '^[1-9][0-9]{0,17}$' || return 1
  for key in repository version tag source-commit metadata-asset metadata-sha256 release-workflow-identity; do
    require_toml_string "$metadata" "[release]" "$key" || return 1
  done
  for lexical_target in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
    lexical_section="[targets.${lexical_target}]"
    for key in asset sha256 blake3 sigstore-bundle; do
      require_toml_string "$metadata" "$lexical_section" "$key" || return 1
    done
    require_toml_integer "$metadata" "$lexical_section" size '^[1-9][0-9]*$' || return 1
  done

  [ "$(toml_value "$metadata" "" schema-version)" = 1 ] || return 1
  [ "$(toml_value "$metadata" "" kind)" = aos-channel ] || return 1
  [ "$(toml_value "$metadata" "" product)" = unicity-aos-ce ] || return 1
  [ "$(toml_value "$metadata" "" channel)" = "$expected_channel" ] || {
    echo "signed channel metadata names a different channel" >&2
    return 1
  }
  generation=$(toml_value "$metadata" "" generation)
  published_at=$(toml_value "$metadata" "" published-at)
  expires_at=$(toml_value "$metadata" "" expires-at)
  # Keep generations inside the signed 64-bit range used by POSIX test(1).
  printf '%s\n' "$generation" | grep -Eq '^[1-9][0-9]{0,17}$' || return 1
  printf '%s\n' "$published_at" | grep -Eq '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$' || return 1
  printf '%s\n' "$expires_at" | grep -Eq '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$' || return 1
  utc_before "$published_at" "$expires_at" || { echo "signed channel metadata has an invalid lifetime" >&2; return 1; }
  published_epoch=$(utc_epoch "$published_at") || { echo "signed channel published-at is invalid" >&2; return 1; }
  expires_epoch=$(utc_epoch "$expires_at") || { echo "signed channel expires-at is invalid" >&2; return 1; }
  case "$expected_channel" in
    stable) max_lifetime_seconds=2592000 ;;
    dev) max_lifetime_seconds=604800 ;;
    nightly) max_lifetime_seconds=172800 ;;
    *) return 1 ;;
  esac
  [ $((expires_epoch - published_epoch)) -le "$max_lifetime_seconds" ] || {
    echo "signed channel lifetime exceeds the maximum for $expected_channel" >&2
    return 1
  }
  if [ "$check_expiry" -eq 1 ]; then
    now=$(date -u '+%Y-%m-%dT%H:%M:%SZ')
    now_epoch=$(date -u '+%s')
    utc_before_or_equal "$now" "$expires_at" || {
      echo "signed channel metadata has expired" >&2
      return 1
    }
    [ "$published_epoch" -le $((now_epoch + 300)) ] || {
      echo "signed channel published-at is unreasonably far in the future" >&2
      return 1
    }
  fi

  release_repository=$(toml_value "$metadata" "[release]" repository)
  release_version=$(toml_value "$metadata" "[release]" version)
  release_tag_value=$(toml_value "$metadata" "[release]" tag)
  release_source_commit=$(toml_value "$metadata" "[release]" source-commit)
  release_metadata_asset_value=$(toml_value "$metadata" "[release]" metadata-asset)
  release_metadata_sha_value=$(toml_value "$metadata" "[release]" metadata-sha256)
  release_workflow_identity=$(toml_value "$metadata" "[release]" release-workflow-identity)
  [ "$release_repository" = "$AOS_TRUSTED_RELEASE_REPO" ] || return 1
  is_aos_release_version "$release_version" || return 1
  if [ "$expected_channel" = nightly ]; then
    is_aos_nightly_version "$release_version" || return 1
  else
    ! is_aos_nightly_version "$release_version" || return 1
  fi
  [ "$release_tag_value" = "$release_version" ] || return 1
  printf '%s\n' "$release_source_commit" | grep -Eq '^[0-9a-f]{40}$' || return 1
  if is_aos_nightly_version "$release_version"; then
    [ "${release_version##*.g}" = "$release_source_commit" ] || return 1
  fi
  [ "$release_metadata_asset_value" = "unicity-aos-${release_version}-release.toml" ] || return 1
  printf '%s\n' "$release_metadata_sha_value" | grep -Eq '^[0-9a-f]{64}$' || return 1
  [ "$release_workflow_identity" = "https://github.com/${AOS_TRUSTED_RELEASE_REPO}/.github/workflows/release.yml@refs/tags/${release_version}" ] || return 1
  for metadata_target in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
    metadata_section="[targets.${metadata_target}]"
    metadata_target_asset=$(toml_value "$metadata" "$metadata_section" asset)
    metadata_target_sha=$(toml_value "$metadata" "$metadata_section" sha256)
    metadata_target_blake3=$(toml_value "$metadata" "$metadata_section" blake3)
    metadata_target_bundle=$(toml_value "$metadata" "$metadata_section" sigstore-bundle)
    metadata_target_size=$(toml_value "$metadata" "$metadata_section" size)
    [ "$metadata_target_asset" = "unicity-aos-${release_version}-${metadata_target}.tar.gz" ] || return 1
    [ "$metadata_target_bundle" = "${metadata_target_asset}.sigstore.json" ] || return 1
    printf '%s\n' "$metadata_target_sha" | grep -Eq '^[0-9a-f]{64}$' || return 1
    printf '%s\n' "$metadata_target_blake3" | grep -Eq '^[0-9a-f]{64}$' || return 1
    printf '%s\n' "$metadata_target_size" | grep -Eq '^[1-9][0-9]*$' || return 1
  done
}

validate_release_metadata() {
  metadata=$1
  expected_version=$2
  expected_identity=$3
  awk '
    function mark(name) { if (seen[name]++) bad = 1 }
    /^$/ { next }
    /^\[/ {
      section = $0
      if (section ~ /^\[(runtime|contracts|gates)\]$/) mark(section)
      else if (section ~ /^\[targets\.(aarch64-apple-darwin|x86_64-apple-darwin|aarch64-unknown-linux-gnu|x86_64-unknown-linux-gnu)\]$/) mark(section)
      else bad = 1
      next
    }
    {
      key = $1
      if ($2 != "=") { bad = 1; next }
      if (section == "") {
        if (key !~ /^(schema-version|kind|product|version|tag|source-commit|published-at|release-workflow-identity)$/) bad = 1
      } else if (section == "[runtime]") {
        if (key !~ /^(repository|version|tag|release-workflow-identity|release-metadata-available|source-commit|release-metadata-asset|release-metadata-blake3)$/) bad = 1
      } else if (section == "[contracts]") {
        if (key !~ /^(repository|commit|sdk-rust-version|sdk-rust-commit)$/) bad = 1
      } else if (section == "[gates]") {
        if (key !~ /^(release-ready|upgrade-self-heal-ready)$/) bad = 1
      } else if (section ~ /^\[targets\./) {
        if (key !~ /^(asset|sha256|blake3|sigstore-bundle|size)$/) bad = 1
      } else bad = 1
      mark(section SUBSEP key)
    }
    END {
      required_top = "schema-version kind product version tag source-commit published-at release-workflow-identity"
      split(required_top, top)
      for (i in top) if (seen[SUBSEP top[i]] != 1) bad = 1
      tables[1] = "[runtime]|repository version tag release-workflow-identity release-metadata-available source-commit release-metadata-asset release-metadata-blake3"
      tables[2] = "[contracts]|repository commit sdk-rust-version sdk-rust-commit"
      tables[3] = "[gates]|release-ready upgrade-self-heal-ready"
      for (t in tables) {
        split(tables[t], parts, "|")
        if (seen[parts[1]] != 1) bad = 1
        split(parts[2], keys)
        for (i in keys) if (seen[parts[1] SUBSEP keys[i]] != 1) bad = 1
      }
      targets[1] = "aarch64-apple-darwin"
      targets[2] = "x86_64-apple-darwin"
      targets[3] = "aarch64-unknown-linux-gnu"
      targets[4] = "x86_64-unknown-linux-gnu"
      split("asset sha256 blake3 sigstore-bundle size", target_keys)
      for (t in targets) {
        target_section = "[targets." targets[t] "]"
        if (seen[target_section] != 1) bad = 1
        for (i in target_keys) if (seen[target_section SUBSEP target_keys[i]] != 1) bad = 1
      }
      exit bad ? 1 : 0
    }
  ' "$metadata" || { echo "signed release metadata does not match schema 1" >&2; return 1; }
  require_toml_integer "$metadata" "" schema-version '^1$' || return 1
  for key in kind product version tag source-commit published-at release-workflow-identity; do
    require_toml_string "$metadata" "" "$key" || return 1
  done
  for key in repository version tag release-workflow-identity source-commit release-metadata-asset release-metadata-blake3; do
    require_toml_string "$metadata" "[runtime]" "$key" || return 1
  done
  require_toml_boolean "$metadata" "[runtime]" release-metadata-available || return 1
  for key in repository commit sdk-rust-version sdk-rust-commit; do
    require_toml_string "$metadata" "[contracts]" "$key" || return 1
  done
  require_toml_boolean "$metadata" "[gates]" release-ready || return 1
  require_toml_boolean "$metadata" "[gates]" upgrade-self-heal-ready || return 1
  for lexical_target in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
    lexical_section="[targets.${lexical_target}]"
    for key in asset sha256 blake3 sigstore-bundle; do
      require_toml_string "$metadata" "$lexical_section" "$key" || return 1
    done
    require_toml_integer "$metadata" "$lexical_section" size '^[1-9][0-9]*$' || return 1
  done
  [ "$(toml_value "$metadata" "" schema-version)" = 1 ] || return 1
  [ "$(toml_value "$metadata" "" kind)" = aos-release ] || return 1
  [ "$(toml_value "$metadata" "" product)" = unicity-aos-ce ] || return 1
  [ "$(toml_value "$metadata" "" version)" = "$expected_version" ] || return 1
  [ "$(toml_value "$metadata" "" tag)" = "$expected_version" ] || return 1
  [ "$(toml_value "$metadata" "" release-workflow-identity)" = "$expected_identity" ] || {
    echo "signed release metadata does not name the exact tag workflow identity" >&2
    return 1
  }
  metadata_source_commit=$(toml_value "$metadata" "" source-commit)
  printf '%s\n' "$metadata_source_commit" | grep -Eq '^[0-9a-f]{40}$' || return 1
  if is_aos_nightly_version "$expected_version"; then
    [ "${expected_version##*.g}" = "$metadata_source_commit" ] || return 1
  fi
  printf '%s\n' "$(toml_value "$metadata" "" published-at)" | grep -Eq '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$' || return 1

  runtime_repository=$(toml_value "$metadata" "[runtime]" repository)
  runtime_version=$(toml_value "$metadata" "[runtime]" version)
  runtime_tag=$(toml_value "$metadata" "[runtime]" tag)
  runtime_identity=$(toml_value "$metadata" "[runtime]" release-workflow-identity)
  runtime_metadata_available=$(toml_value "$metadata" "[runtime]" release-metadata-available)
  runtime_source_commit=$(toml_value "$metadata" "[runtime]" source-commit)
  runtime_metadata_asset=$(toml_value "$metadata" "[runtime]" release-metadata-asset)
  runtime_metadata_blake3=$(toml_value "$metadata" "[runtime]" release-metadata-blake3)
  [ "$runtime_repository" = astrid-runtime/astrid ] || return 1
  printf '%s\n' "$runtime_version" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' || return 1
  [ "$runtime_tag" = "v${runtime_version}" ] || return 1
  case "$runtime_identity" in
    "https://github.com/astrid-runtime/astrid/.github/workflows/release.yml@refs/tags/v${runtime_version}"|\
    "https://github.com/unicity-astrid/astrid/.github/workflows/release.yml@refs/tags/v${runtime_version}") ;;
    *) return 1 ;;
  esac
  if [ "$runtime_metadata_available" = true ]; then
    [ "$runtime_identity" = "https://github.com/astrid-runtime/astrid/.github/workflows/release.yml@refs/tags/v${runtime_version}" ] || return 1
    printf '%s\n' "$runtime_source_commit" | grep -Eq '^[0-9a-f]{40}$' || return 1
    [ "$runtime_metadata_asset" = "astrid-${runtime_version}-release.toml" ] || return 1
    printf '%s\n' "$runtime_metadata_blake3" | grep -Eq '^[0-9a-f]{64}$' || return 1
  else
    [ -z "$runtime_source_commit" ] && [ -z "$runtime_metadata_asset" ] && [ -z "$runtime_metadata_blake3" ] || return 1
  fi

  [ "$(toml_value "$metadata" "[contracts]" repository)" = astrid-runtime/wit ] || return 1
  printf '%s\n' "$(toml_value "$metadata" "[contracts]" commit)" | grep -Eq '^[0-9a-f]{40}$' || return 1
  printf '%s\n' "$(toml_value "$metadata" "[contracts]" sdk-rust-version)" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' || return 1
  printf '%s\n' "$(toml_value "$metadata" "[contracts]" sdk-rust-commit)" | grep -Eq '^[0-9a-f]{40}$' || return 1
  for metadata_target in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
    metadata_section="[targets.${metadata_target}]"
    metadata_target_asset=$(toml_value "$metadata" "$metadata_section" asset)
    metadata_target_sha=$(toml_value "$metadata" "$metadata_section" sha256)
    metadata_target_blake3=$(toml_value "$metadata" "$metadata_section" blake3)
    metadata_target_bundle=$(toml_value "$metadata" "$metadata_section" sigstore-bundle)
    metadata_target_size=$(toml_value "$metadata" "$metadata_section" size)
    [ "$metadata_target_asset" = "unicity-aos-${expected_version}-${metadata_target}.tar.gz" ] || return 1
    [ "$metadata_target_bundle" = "${metadata_target_asset}.sigstore.json" ] || return 1
    printf '%s\n' "$metadata_target_sha" | grep -Eq '^[0-9a-f]{64}$' || return 1
    printf '%s\n' "$metadata_target_blake3" | grep -Eq '^[0-9a-f]{64}$' || return 1
    printf '%s\n' "$metadata_target_size" | grep -Eq '^[1-9][0-9]*$' || return 1
  done
}

validate_accepted_channel() {
  [ -n "$channel_root" ] || return 0
  for state_parent in "$AOS_HOME" "$AOS_HOME/update" "$AOS_HOME/update/channels" "$channel_root" "$channel_root/generations"; do
    [ ! -L "$state_parent" ] || { echo "refusing symlinked channel state path: $state_parent" >&2; return 1; }
  done
  if [ ! -e "$channel_current_path" ]; then
    return 0
  fi
  [ -f "$channel_current_path" ] && [ ! -L "$channel_current_path" ] || {
    echo "accepted channel pointer is not a regular file" >&2
    return 1
  }
  [ "$(wc -l < "$channel_current_path" | tr -d ' ')" = 1 ] || {
    echo "accepted channel pointer is malformed" >&2
    return 1
  }
  accepted_generation=
  IFS= read -r accepted_generation < "$channel_current_path"
  printf '%s\n' "$accepted_generation" | grep -Eq '^[1-9][0-9]{0,17}$' || {
    echo "accepted channel pointer is malformed" >&2
    return 1
  }
  accepted_dir="$channel_root/generations/$accepted_generation"
  [ -d "$accepted_dir" ] && [ ! -L "$accepted_dir" ] || {
    echo "accepted channel generation is incomplete" >&2
    return 1
  }
  accepted_metadata="$accepted_dir/channel.toml"
  accepted_bundle="$accepted_dir/channel.toml.sigstore.json"
  [ -f "$accepted_metadata" ] && [ ! -L "$accepted_metadata" ] && \
    [ -f "$accepted_bundle" ] && [ ! -L "$accepted_bundle" ] || {
    echo "accepted channel generation is incomplete" >&2
    return 1
  }
  "$COSIGN_BIN" verify-blob \
    --bundle "$accepted_bundle" \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    --certificate-identity "$channel_identity" \
    --use-signed-timestamps \
    "$accepted_metadata" >/dev/null
  validate_channel_metadata "$accepted_metadata" "$AOS_CHANNEL" 0
  [ "$(toml_value "$accepted_metadata" "" generation)" = "$accepted_generation" ] || {
    echo "accepted channel pointer does not match its signed generation" >&2
    return 1
  }
  if [ "$channel_generation" -lt "$accepted_generation" ]; then
    echo "signed channel generation $channel_generation is older than accepted generation $accepted_generation" >&2
    return 1
  fi
  if [ "$channel_generation" -eq "$accepted_generation" ] && ! cmp -s "$work/channel.toml" "$accepted_metadata"; then
    echo "signed channel generation $channel_generation conflicts with the accepted metadata" >&2
    return 1
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

work=$(mktemp -d "${TMPDIR:-/tmp}/unicity-aos-install.XXXXXX")

release_install_lock() {
  if [ "$install_lock_acquired" -ne 1 ]; then
    return
  fi
  owner=
  if [ -f "$install_lock/pid" ] && IFS= read -r owner < "$install_lock/pid" && [ "$owner" = "$$" ]; then
    rm -rf "$install_lock"
  fi
  install_lock_acquired=0
}

acquire_install_lock() {
  for lock_parent in "$AOS_HOME" "$AOS_HOME/update"; do
    [ ! -L "$lock_parent" ] || { echo "refusing symlinked install lock path: $lock_parent" >&2; return 1; }
  done
  mkdir -p "$AOS_HOME/update"
  chmod 700 "$AOS_HOME" "$AOS_HOME/update"
  install_lock="$AOS_HOME/update/install.lock"
  [ ! -L "$install_lock" ] || { echo "refusing symlinked install lock" >&2; return 1; }
  if ! mkdir "$install_lock" 2>/dev/null; then
    owner=
    if [ -f "$install_lock/pid" ] && [ ! -L "$install_lock/pid" ] && \
      IFS= read -r owner < "$install_lock/pid" && \
      printf '%s\n' "$owner" | grep -Eq '^[1-9][0-9]*$' && \
      ! kill -0 "$owner" 2>/dev/null; then
      stale_lock="${install_lock}.stale.$$"
      if ! mv "$install_lock" "$stale_lock" 2>/dev/null; then
        echo "another AOS installation owns the install lock" >&2
        return 1
      fi
      rm -rf "$stale_lock"
      mkdir "$install_lock"
    else
      echo "another AOS installation owns the install lock" >&2
      return 1
    fi
  fi
  printf '%s\n' "$$" > "$install_lock/pid"
  chmod 600 "$install_lock/pid"
  install_lock_acquired=1
}

cleanup() {
  status=$?
  trap - EXIT HUP INT TERM
  if [ "$installation_started" -eq 1 ]; then
    restore || status=1
  elif [ "$release_committed" -eq 1 ] && [ -n "$release_backup" ]; then
    rm -rf "$release_backup" || status=1
  fi
  release_install_lock || status=1
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

if [ -z "$AOS_VERSION" ]; then
  channel_base="${AOS_CHANNEL_BASE_URL}/channel-${AOS_CHANNEL}"
  echo "Resolving the signed $AOS_CHANNEL channel..."
  curl --proto '=https' --tlsv1.2 -fsSL "$channel_base/channel.toml" -o "$work/channel.toml"
  curl --proto '=https' --tlsv1.2 -fsSL "$channel_base/channel.toml.sigstore.json" -o "$work/channel.toml.sigstore.json"
  channel_identity="https://github.com/${AOS_TRUSTED_RELEASE_REPO}/.github/workflows/promote-channel.yml@refs/heads/main"
  "$COSIGN_BIN" verify-blob \
    --bundle "$work/channel.toml.sigstore.json" \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    --certificate-identity "$channel_identity" \
    --use-signed-timestamps \
    "$work/channel.toml" >/dev/null
  validate_channel_metadata "$work/channel.toml" "$AOS_CHANNEL"
  AOS_VERSION=$(toml_value "$work/channel.toml" "[release]" version)
  release_tag=$(toml_value "$work/channel.toml" "[release]" tag)
  release_metadata_asset=$(toml_value "$work/channel.toml" "[release]" metadata-asset)
  release_metadata_sha256=$(toml_value "$work/channel.toml" "[release]" metadata-sha256)
  channel_generation=$(toml_value "$work/channel.toml" "" generation)

  channel_root="$AOS_HOME/update/channels/$AOS_CHANNEL"
  channel_current_path="$channel_root/current"
  channel_generation_dir="$channel_root/generations/$channel_generation"
else
  release_tag=$AOS_VERSION
  release_metadata_asset="unicity-aos-${AOS_VERSION}-release.toml"
  release_metadata_sha256=
fi

release_identity="https://github.com/${AOS_TRUSTED_RELEASE_REPO}/.github/workflows/release.yml@refs/tags/${release_tag}"
release_base="https://github.com/${AOS_RELEASE_REPO}/releases/download/${release_tag}"
curl --proto '=https' --tlsv1.2 -fsSL "$release_base/$release_metadata_asset" -o "$work/$release_metadata_asset"
curl --proto '=https' --tlsv1.2 -fsSL "$release_base/$release_metadata_asset.sigstore.json" -o "$work/$release_metadata_asset.sigstore.json"
"$COSIGN_BIN" verify-blob \
  --bundle "$work/$release_metadata_asset.sigstore.json" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-identity "$release_identity" \
  --use-signed-timestamps \
  "$work/$release_metadata_asset" >/dev/null
if [ -n "$release_metadata_sha256" ] && [ "$(sha256_file "$work/$release_metadata_asset")" != "$release_metadata_sha256" ]; then
  echo "signed release metadata does not match the channel digest" >&2
  exit 1
fi
validate_release_metadata "$work/$release_metadata_asset" "$AOS_VERSION" "$release_identity"

target_section="[targets.${target}]"
asset=$(toml_value "$work/$release_metadata_asset" "$target_section" asset)
asset_sha256=$(toml_value "$work/$release_metadata_asset" "$target_section" sha256)
asset_blake3=$(toml_value "$work/$release_metadata_asset" "$target_section" blake3)
asset_bundle=$(toml_value "$work/$release_metadata_asset" "$target_section" sigstore-bundle)
expected_asset="unicity-aos-${AOS_VERSION}-${target}.tar.gz"
[ "$asset" = "$expected_asset" ] || { echo "release metadata selected a non-canonical target asset" >&2; exit 1; }
[ "$asset_bundle" = "$asset.sigstore.json" ] || { echo "release metadata selected a non-canonical signature bundle" >&2; exit 1; }
printf '%s\n' "$asset_sha256" | grep -Eq '^[0-9a-f]{64}$' || {
  echo "release metadata contains a malformed target SHA-256 digest" >&2
  exit 1
}
printf '%s\n' "$asset_blake3" | grep -Eq '^[0-9a-f]{64}$' || {
  echo "release metadata contains a malformed target digest" >&2
  exit 1
}
if [ -f "$work/channel.toml" ]; then
  [ "$(toml_value "$work/$release_metadata_asset" "[gates]" release-ready)" = true ] || {
    echo "signed channel points to release metadata whose release-ready gate is false" >&2
    exit 1
  }
  [ "$(toml_value "$work/$release_metadata_asset" "[gates]" upgrade-self-heal-ready)" = true ] || {
    echo "signed channel points to release metadata whose upgrade-self-heal-ready gate is false" >&2
    exit 1
  }
  [ "$(toml_value "$work/channel.toml" "[release]" source-commit)" = "$(toml_value "$work/$release_metadata_asset" "" source-commit)" ] || {
    echo "signed channel source commit does not match immutable release metadata" >&2
    exit 1
  }
  for key in asset sha256 blake3 sigstore-bundle size; do
    [ "$(toml_value "$work/channel.toml" "$target_section" "$key")" = "$(toml_value "$work/$release_metadata_asset" "$target_section" "$key")" ] || {
      echo "signed channel target does not match immutable release metadata: $key" >&2
      exit 1
    }
  done
fi

echo "Downloading Unicity AOS $AOS_VERSION for $target..."
curl --proto '=https' --tlsv1.2 -fsSL "$release_base/$asset" -o "$work/$asset"
curl --proto '=https' --tlsv1.2 -fsSL "$release_base/$asset_bundle" -o "$work/$asset_bundle"
[ "$(sha256_file "$work/$asset")" = "$asset_sha256" ] || {
  echo "Unicity AOS archive checksum mismatch" >&2
  exit 1
}
"$COSIGN_BIN" verify-blob \
  --bundle "$work/$asset_bundle" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-identity "$release_identity" \
  --use-signed-timestamps \
  "$work/$asset" >/dev/null

mkdir "$work/unpack"
if tar -tzf "$work/$asset" | awk -v target="$target" -v version="$AOS_VERSION" '
  {
    root = $0
    sub(/\/.*/, "", root)
    if (first == "") first = root
    if (root != first) unsafe = 1
    if (root != "unicity-aos-" version "-" target) {
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

for file in bin/aos libexec/install.sh runtime/bin/astrid runtime/bin/astrid-daemon runtime/bin/astrid-build runtime/bin/astrid-emit release-manifest.json Distro.toml capsule-assets.txt; do
  [ -f "$bundle/$file" ] || { echo "release archive is missing $file" >&2; exit 1; }
done
[ -d "$bundle/capsules" ] || { echo "release archive has no capsule directory" >&2; exit 1; }
if ! awk '
  !/^astrid-capsule-[a-z0-9-]+\.capsule$/ { invalid = 1 }
  seen[$0]++ { duplicate = 1 }
  END { exit invalid || duplicate || NR == 0 }
' "$bundle/capsule-assets.txt"; then
  echo "release archive has an invalid capsule asset manifest" >&2
  exit 1
fi
expected_capsule_count=$(wc -l < "$bundle/capsule-assets.txt" | tr -d ' ')
capsule_count=$(find "$bundle/capsules" -mindepth 1 -maxdepth 1 -print | wc -l | tr -d ' ')
[ "$capsule_count" -eq "$expected_capsule_count" ] || { echo "release archive capsule set is incomplete" >&2; exit 1; }
while IFS= read -r capsule; do
  capsule_path="$bundle/capsules/$capsule"
  [ -f "$capsule_path" ] && [ ! -L "$capsule_path" ] || {
    echo "release archive contains a missing or non-regular capsule asset: $capsule" >&2
    exit 1
  }
done < "$bundle/capsule-assets.txt"

staged_version=$("$bundle/bin/aos" --version | awk '{print $NF}')
if ! is_aos_release_version "$staged_version"; then
  echo "staged AOS binary reported an invalid product version" >&2
  exit 1
fi
if [ "$staged_version" != "$AOS_VERSION" ]; then
  echo "staged AOS version $staged_version does not match requested version $AOS_VERSION" >&2
  exit 1
fi
if [ "$bundle_name" != "unicity-aos-${staged_version}-${target}" ]; then
  echo "release bundle root does not match its product version and target" >&2
  exit 1
fi

release_dir="$AOS_HOME/releases/$staged_version"
release_stage="$AOS_HOME/releases/.${staged_version}.new.$$"
release_backup="$AOS_HOME/releases/.${staged_version}.rollback.$$"
for managed in "$AOS_HOME" "$AOS_HOME/libexec" "$AOS_HOME/runtime" "$AOS_HOME/runtime/bin" "$AOS_HOME/releases" "$release_dir" "$release_dir/capsules" "$AOS_HOME/update" "$AOS_HOME/update/channels"; do
  [ ! -L "$managed" ] || { echo "refusing symlinked managed path: $managed" >&2; exit 1; }
done
if [ -e "$release_dir" ] && [ ! -d "$release_dir" ]; then
  echo "refusing non-directory release destination: $release_dir" >&2
  exit 1
fi
if [ -e "$release_stage" ] || [ -e "$release_backup" ]; then
  echo "refusing stale release transaction state for $staged_version" >&2
  exit 1
fi
if [ -L "$AOS_BIN_DIR" ]; then
  echo "refusing symlinked binary directory: $AOS_BIN_DIR" >&2
  exit 1
fi
if [ -L "$AOS_BIN_DIR/aos" ] || { [ -e "$AOS_BIN_DIR/aos" ] && [ ! -f "$AOS_BIN_DIR/aos" ]; }; then
  echo "refusing non-regular install destination: $AOS_BIN_DIR/aos" >&2
  exit 1
fi

if [ -x "$AOS_BIN_DIR/aos" ] && [ "$ASSUME_YES" -ne 1 ]; then
  answer=
  if ! has_interactive_tty || ! prompt_from_tty 'Replace the existing Unicity AOS installation? [y/N] '; then
    echo "an existing Unicity AOS installation was found; rerun with --yes to replace it without a prompt" >&2
    exit 1
  fi
  case "$answer" in y|Y|yes|YES) ;; *) echo "Installation cancelled."; exit 0 ;; esac
fi

acquire_install_lock
validate_accepted_channel

if [ -x "$AOS_BIN_DIR/aos" ]; then
  "$AOS_BIN_DIR/aos" stop >/dev/null 2>&1 || true
fi

mkdir -p "$AOS_BIN_DIR" "$AOS_HOME/libexec" "$AOS_HOME/runtime/bin" "$AOS_HOME/releases"
chmod 700 "$AOS_HOME" "$AOS_HOME/libexec" "$AOS_HOME/runtime" "$AOS_HOME/runtime/bin" "$AOS_HOME/releases"
if [ -n "$channel_root" ]; then
  mkdir -p "$channel_root/generations"
  chmod 700 "$AOS_HOME/update" "$AOS_HOME/update/channels" "$channel_root" "$channel_root/generations"
fi
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

stage_channel_receipt() {
  [ -n "$channel_root" ] || return 0
  mkdir -p "$channel_root/generations"
  chmod 700 "$AOS_HOME/update" "$AOS_HOME/update/channels" "$channel_root" "$channel_root/generations"
  if [ -e "$channel_generation_dir" ]; then
    [ -d "$channel_generation_dir" ] && [ ! -L "$channel_generation_dir" ] || {
      echo "channel generation receipt is not a directory" >&2
      return 1
    }
    existing_metadata="$channel_generation_dir/channel.toml"
    existing_bundle="$channel_generation_dir/channel.toml.sigstore.json"
    [ -f "$existing_metadata" ] && [ ! -L "$existing_metadata" ] && \
      [ -f "$existing_bundle" ] && [ ! -L "$existing_bundle" ] || {
      echo "channel generation receipt is incomplete" >&2
      return 1
    }
    "$COSIGN_BIN" verify-blob \
      --bundle "$existing_bundle" \
      --certificate-oidc-issuer https://token.actions.githubusercontent.com \
      --certificate-identity "$channel_identity" \
      --use-signed-timestamps \
      "$existing_metadata" >/dev/null
    validate_channel_metadata "$existing_metadata" "$AOS_CHANNEL" 0
    cmp -s "$work/channel.toml" "$existing_metadata" || {
      echo "channel generation receipt conflicts with signed metadata" >&2
      return 1
    }
  else
    generation_stage="$channel_root/generations/.${channel_generation}.new.$$"
    [ ! -e "$generation_stage" ] || { echo "stale channel receipt transaction exists" >&2; return 1; }
    mkdir "$generation_stage"
    chmod 700 "$generation_stage"
    install -m 0600 "$work/channel.toml" "$generation_stage/channel.toml"
    install -m 0600 "$work/channel.toml.sigstore.json" "$generation_stage/channel.toml.sigstore.json"
    mv "$generation_stage" "$channel_generation_dir"
  fi
  printf '%s\n' "$channel_generation" > "$work/channel-current"
  chmod 600 "$work/channel-current"
  sync
  install_one "$work/channel-current" "$channel_current_path" channel-current 600
  sync
}

restore() {
  result=0
  rm -rf "$release_stage"
  if [ -d "$release_backup" ]; then
    rm -rf "$release_dir"
    mv "$release_backup" "$release_dir" || result=1
  elif [ -f "$rollback/release.touched" ]; then
    rm -rf "$release_dir" || result=1
  fi
  for name in aos installer astrid astrid-daemon astrid-build astrid-emit channel-current; do
    case "$name" in
      aos) destination="$AOS_BIN_DIR/aos" ;;
      installer) destination="$AOS_HOME/libexec/install.sh" ;;
      channel-current) destination=$channel_current_path ;;
      *) destination="$AOS_HOME/runtime/bin/$name" ;;
    esac
    [ -n "$destination" ] || continue
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
if ! install_one "$bundle/libexec/install.sh" "$AOS_HOME/libexec/install.sh" installer 600; then exit 1; fi
for name in astrid astrid-daemon astrid-build astrid-emit; do
  if ! install_one "$bundle/runtime/bin/$name" "$AOS_HOME/runtime/bin/$name" "$name" 755; then exit 1; fi
done
mkdir "$release_stage"
chmod 700 "$release_stage"
mkdir "$release_stage/capsules"
chmod 700 "$release_stage/capsules"
install -m 0600 "$bundle/release-manifest.json" "$release_stage/release-manifest.json"
install -m 0600 "$bundle/Distro.toml" "$release_stage/Distro.toml"
install -m 0600 "$bundle/capsule-assets.txt" "$release_stage/capsule-assets.txt"
while IFS= read -r capsule; do
  install -m 0600 "$bundle/capsules/$capsule" "$release_stage/capsules/$capsule"
done < "$bundle/capsule-assets.txt"
if [ -d "$release_dir" ]; then
  mv "$release_dir" "$release_backup"
fi
: > "$rollback/release.touched"
if ! mv "$release_stage" "$release_dir"; then
  exit 1
fi
release_committed=1
stage_channel_receipt
installation_started=0
rm -rf "$release_backup"
release_install_lock

echo "Installed Unicity AOS $staged_version."
case ":$PATH:" in
  *":$AOS_BIN_DIR:"*) init_command="aos init" ;;
  *)
    echo "Add $AOS_BIN_DIR to PATH."
    init_command="$AOS_BIN_DIR/aos init"
    ;;
esac

if [ "$SKIP_MIGRATION_PROMPT" -ne 1 ] && has_interactive_tty; then
  "$AOS_BIN_DIR/aos" </dev/tty >/dev/tty 2>/dev/tty || true
else
  echo "Run: $init_command"
fi
