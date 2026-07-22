#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <extracted-product-bundle>" >&2
  exit 2
fi

bundle=$(cd "$1" && pwd -P)
for required in bin/aos runtime/bin/astrid runtime/bin/astrid-daemon Distro.toml capsule-assets.txt; do
  [[ -f "$bundle/$required" && ! -L "$bundle/$required" ]] || {
    echo "clean-home init bundle is missing $required" >&2
    exit 1
  }
done
[[ -d "$bundle/capsules" && ! -L "$bundle/capsules" ]] || {
  echo "clean-home init bundle is missing capsules directory" >&2
  exit 1
}

work=$(mktemp -d)
aos_home="$work/user/.aos"
project="$work/project"
mkdir -p "$project"

run_aos() {
  (
    cd "$project"
    HOME="$work/user" \
      AOS_HOME="$aos_home" \
      UNICITY_AOS_RUNTIME_BIN="$bundle/runtime/bin/astrid" \
      UNICITY_AOS_CAPSULE_DIR="$bundle/capsules" \
      "$bundle/bin/aos" "$@"
  )
}

assert_exact_ready_capsules() {
  local ps_json
  ps_json=$(run_aos ps --format json)
  python3 - "$bundle/capsule-assets.txt" "$ps_json" <<'PY'
import json
import pathlib
import sys

assets_path = pathlib.Path(sys.argv[1])
rows = json.loads(sys.argv[2])
expected = sorted(
    line[:-len(".capsule")] if line.endswith(".capsule") else line
    for line in assets_path.read_text(encoding="utf-8").splitlines()
    if line
)
actual = sorted(row.get("capsule") for row in rows)
if actual != expected:
    raise SystemExit(
        f"running capsule set does not match the exact CE release set: "
        f"expected={expected!r}, actual={actual!r}"
    )
not_ready = [row for row in rows if row.get("state") != "ready"]
if not_ready:
    raise SystemExit(f"CE capsules are not all ready: {not_ready!r}")
PY
}

cleanup() {
  status=$?
  trap - EXIT
  run_aos stop >/dev/null 2>&1 || true
  if ! rm -rf "$work"; then
    echo "warning: clean-home fixture cleanup left runtime mounts for runner teardown" >&2
  fi
  exit "$status"
}
trap cleanup EXIT

run_aos init --offline --yes --var openai_api_key=release-gate-not-a-real-key

lock="$aos_home/runtime/home/default/.config/distro.lock"
profile="$aos_home/runtime/etc/profiles/default.toml"
manifest="$aos_home/distributions/unicity-ce/Distro.toml"
[[ -f "$lock" && -f "$profile" && -f "$manifest" ]]

python3 - "$bundle/capsule-assets.txt" "$lock" "$profile" <<'PY'
import pathlib
import sys

try:
    import tomllib
except ModuleNotFoundError:
    import tomli as tomllib

assets_path, lock_path, profile_path = map(pathlib.Path, sys.argv[1:])
expected = sorted(
    line[:-len(".capsule")] if line.endswith(".capsule") else line
    for line in assets_path.read_text(encoding="utf-8").splitlines()
    if line
)
if len(expected) != 21 or len(set(expected)) != 21:
    raise SystemExit("release capsule inventory is not the exact 21-capsule CE set")
with lock_path.open("rb") as file:
    lock = tomllib.load(file)
with profile_path.open("rb") as file:
    profile = tomllib.load(file)
if lock.get("distro", {}).get("id") != "unicity-ce":
    raise SystemExit("clean home did not receive the Unicity CE distro lock")
locked = sorted(item["name"] for item in lock.get("capsule", []))
granted = sorted(profile.get("capsules", []))
if locked != expected:
    raise SystemExit("Distro.lock does not bind the exact release capsule set")
if granted != expected:
    raise SystemExit("default principal was not granted the exact release capsule set")
PY

assert_exact_ready_capsules
run_aos doctor

cp "$lock" "$work/distro.lock.before"
cp "$profile" "$work/default.toml.before"
pid_file="$aos_home/runtime/run/system.pid"
cli_meta="$aos_home/runtime/home/default/.local/capsules/aos-cli/meta.json"
[[ -f "$pid_file" && ! -L "$pid_file" ]]
[[ -f "$cli_meta" && ! -L "$cli_meta" ]]
cp "$pid_file" "$work/system.pid.before"
cp "$cli_meta" "$work/cli-meta.json.before"
run_aos init --offline --yes --var openai_api_key=release-gate-not-a-real-key
cmp "$work/distro.lock.before" "$lock"
cmp "$work/default.toml.before" "$profile"
cmp "$work/system.pid.before" "$pid_file"
cmp "$work/cli-meta.json.before" "$cli_meta"
assert_exact_ready_capsules

[[ -d "$project/.aos" ]]
if find "$work/user" "$project" -name .astrid -print -quit | grep -q .; then
  echo "clean AOS initialization created standalone Astrid state" >&2
  exit 1
fi

[[ -f "$pid_file" && ! -L "$pid_file" ]]
IFS= read -r daemon_pid < "$pid_file"
[[ "$daemon_pid" =~ ^[1-9][0-9]*$ ]]
kill -0 "$daemon_pid"

run_aos stop
for _ in $(seq 1 200); do
  if ! kill -0 "$daemon_pid" 2>/dev/null; then
    break
  fi
  sleep 0.05
done
if kill -0 "$daemon_pid" 2>/dev/null; then
  echo "AOS runtime process $daemon_pid remained alive after stop" >&2
  exit 1
fi
for transient in system.sock system.pid system.ready system.token; do
  [[ ! -e "$aos_home/runtime/run/$transient" && ! -L "$aos_home/runtime/run/$transient" ]]
done

python3 - "$aos_home/runtime/run/system.lock" <<'PY'
import fcntl
import sys

with open(sys.argv[1], "r+b", buffering=0) as lock:
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    fcntl.flock(lock, fcntl.LOCK_UN)
PY

echo "clean AOS home initialized, loaded, rechecked, and stopped successfully"
