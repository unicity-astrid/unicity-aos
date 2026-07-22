#!/usr/bin/env python3
"""Validate AOS's root command contract against a pinned runtime binary."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - Python 3.10 release fallback
    import tomli as tomllib


ROOT_NAME = re.compile(r"^[a-z][a-z0-9-]*$")
COMMAND_LINE = re.compile(r"^  ([a-z][a-z0-9-]*) {2,}\S")
PUBLIC_BUCKETS = ("inherited", "product-owned", "shared")
ALL_BUCKETS = (*PUBLIC_BUCKETS, "hidden-inherited")


class SurfaceError(ValueError):
    """The runtime command surface and AOS contract disagree."""


def load_contract(path: Path, runtime_version: str) -> dict[str, list[str]]:
    """Load and validate the version-bound AOS command classification."""
    with path.open("rb") as file:
        value = tomllib.load(file)
    if value.get("schema-version") != 1:
        raise SurfaceError("runtime command surface schema-version must be 1")
    if value.get("runtime-version") != runtime_version:
        raise SurfaceError(
            "runtime command surface version does not match runtime-compatibility"
        )
    roots = value.get("roots")
    if not isinstance(roots, dict) or set(roots) != set(ALL_BUCKETS):
        raise SurfaceError(
            "runtime command surface must define exactly inherited, product-owned, shared, and hidden-inherited roots"
        )

    parsed: dict[str, list[str]] = {}
    seen: set[str] = set()
    for bucket in ALL_BUCKETS:
        entries = roots.get(bucket)
        if not isinstance(entries, list) or not entries:
            raise SurfaceError(f"runtime command surface {bucket} must be a non-empty list")
        for entry in entries:
            if not isinstance(entry, str) or ROOT_NAME.fullmatch(entry) is None:
                raise SurfaceError(f"invalid runtime command root in {bucket}: {entry!r}")
            if entry in seen:
                raise SurfaceError(f"runtime command root is classified more than once: {entry}")
            seen.add(entry)
        parsed[bucket] = entries
    return parsed


def parse_help(text: str) -> set[str]:
    """Extract public root commands from Clap's stable Commands section."""
    inside = False
    commands: set[str] = set()
    for line in text.splitlines():
        if line == "Commands:":
            inside = True
            continue
        if inside and line == "Options:":
            break
        if inside and (match := COMMAND_LINE.match(line)):
            commands.add(match.group(1))
    if not inside or not commands:
        raise SurfaceError("runtime --help did not contain a parseable Commands section")
    return commands


def validate(actual: set[str], contract: dict[str, list[str]]) -> None:
    """Require the actual public runtime inventory to match the contract exactly."""
    expected = {
        root for bucket in PUBLIC_BUCKETS for root in contract[bucket]
    }
    if actual == expected:
        return
    missing = sorted(expected - actual)
    unexpected = sorted(actual - expected)
    details = []
    if missing:
        details.append(f"missing from runtime: {', '.join(missing)}")
    if unexpected:
        details.append(f"new or unclassified runtime roots: {', '.join(unexpected)}")
    raise SurfaceError("runtime command surface changed; " + "; ".join(details))


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("runtime", type=Path)
    parser.add_argument("contract", type=Path)
    parser.add_argument("--runtime-version", required=True)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    contract = load_contract(args.contract, args.runtime_version)
    result = subprocess.run(
        [args.runtime, "--help"],
        text=True,
        capture_output=True,
        check=False,
        timeout=30,
    )
    if result.returncode != 0:
        raise SurfaceError(
            f"runtime --help failed with exit {result.returncode}: {result.stderr.strip()}"
        )
    validate(parse_help(result.stdout), contract)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, subprocess.SubprocessError, SurfaceError) as error:
        print(f"runtime command surface error: {error}", file=sys.stderr)
        raise SystemExit(1) from error
