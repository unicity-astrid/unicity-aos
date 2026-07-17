#!/usr/bin/env python3
"""Derive and stage deterministic AOS nightly versions."""

from __future__ import annotations

import argparse
import datetime as dt
import re
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
CANONICAL = re.compile(
    r"(?:202[6-9]|20[3-9][0-9])\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
)
NIGHTLY = re.compile(
    rf"(?P<base>{CANONICAL.pattern})-nightly\.(?P<date>[0-9]{{8}})\.g(?P<commit>[0-9a-f]{{40}})"
)
COMMIT = re.compile(r"[0-9a-f]{40}")


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def read(path: Path) -> str:
    require(path.is_file() and not path.is_symlink(), f"{path} must be a regular file")
    return path.read_text(encoding="utf-8")


def table(path: Path) -> dict[str, object]:
    with path.open("rb") as file:
        return tomllib.load(file)


def canonical_base(root: Path) -> str:
    value = table(root / "crates/unicity-aos-bootstrap/Cargo.toml")["package"]["version"]
    require(isinstance(value, str) and CANONICAL.fullmatch(value) is not None, "AOS source version must be canonical calendar SemVer")
    return value


def real_date(value: str) -> str:
    require(re.fullmatch(r"[0-9]{8}", value) is not None, "nightly date must be YYYYMMDD")
    try:
        parsed = dt.datetime.strptime(value, "%Y%m%d").date()
    except ValueError as error:
        raise ValueError(f"nightly date is invalid: {error}") from error
    return parsed.isoformat()


def derive(base: str, date: str, source_commit: str) -> str:
    require(CANONICAL.fullmatch(base) is not None, "nightly base must be canonical calendar SemVer")
    real_date(date)
    require(COMMIT.fullmatch(source_commit) is not None, "source commit must be 40 lowercase hexadecimal characters")
    return f"{base}-nightly.{date}.g{source_commit}"


def validate_dispatch_date(date: str, created_at: str) -> None:
    real_date(date)
    try:
        created = dt.datetime.fromisoformat(created_at.replace("Z", "+00:00")).date()
    except ValueError as error:
        raise ValueError(f"release dispatch timestamp is invalid: {error}") from error
    nightly = dt.datetime.strptime(date, "%Y%m%d").date()
    require(
        (created - nightly).days in (0, 1),
        "nightly date must match the release dispatch date",
    )


def replace_field(text: str, section: str, key: str, expected: str, replacement: str) -> str:
    pattern = re.compile(
        rf'(?ms)(^\[{re.escape(section)}\]\s*$.*?^\s*{re.escape(key)}\s*=\s*")([^"\r\n]*)(")(?=\s*(?:#.*)?$)'
    )
    matches = list(pattern.finditer(text))
    require(len(matches) == 1, f"[{section}] {key} must occur exactly once")
    require(matches[0].group(2) == expected, f"[{section}] {key} does not match {expected}")
    return pattern.sub(lambda match: f"{match.group(1)}{replacement}{match.group(3)}", text, count=1)


def stage(root: Path, version: str) -> None:
    match = NIGHTLY.fullmatch(version)
    require(match is not None, "nightly version must be YYYY.MINOR.PATCH-nightly.YYYYMMDD.g<40 hex>")
    base = canonical_base(root)
    require(match.group("base") == base, "nightly version must derive from the source AOS version")
    release_date = real_date(match.group("date"))

    product_path = root / "crates/unicity-aos-bootstrap/Cargo.toml"
    product = replace_field(read(product_path), "package", "version", base, version)

    compatibility_path = root / "release/runtime-compatibility.toml"
    compatibility = replace_field(read(compatibility_path), "product", "version", base, version)

    distro_path = root / "distros/community/unicity-ce/Distro.toml"
    distro = read(distro_path)
    distro = replace_field(distro, "distro", "version", base, version)
    pretty_name = table(distro_path)["distro"]["pretty-name"]
    require(isinstance(pretty_name, str) and pretty_name.count(base) == 1, "distro pretty-name must contain the canonical version exactly once")
    distro = replace_field(distro, "distro", "pretty-name", pretty_name, pretty_name.replace(base, version))
    current_release_date = table(distro_path)["distro"]["release-date"]
    require(isinstance(current_release_date, str), "distro release-date must be a string")
    distro = replace_field(distro, "distro", "release-date", current_release_date, release_date)

    lock_path = root / "Cargo.lock"
    lock = read(lock_path)
    lock_pattern = re.compile(
        r'(?ms)(^\[\[package\]\]\s*$\nname = "unicity-aos-bootstrap"\nversion = ")([^"\r\n]+)(")'
    )
    lock_matches = list(lock_pattern.finditer(lock))
    require(len(lock_matches) == 1, "Cargo.lock must contain exactly one unicity-aos-bootstrap package")
    require(lock_matches[0].group(2) == base, "Cargo.lock AOS version does not match the source version")
    lock = lock_pattern.sub(lambda item: f"{item.group(1)}{version}{item.group(3)}", lock, count=1)

    product_path.write_text(product, encoding="utf-8")
    compatibility_path.write_text(compatibility, encoding="utf-8")
    distro_path.write_text(distro, encoding="utf-8")
    lock_path.write_text(lock, encoding="utf-8")


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)
    derive_command = commands.add_parser("derive")
    derive_command.add_argument("--root", type=Path, default=ROOT)
    derive_command.add_argument("--date", required=True)
    derive_command.add_argument("--source-commit", required=True)
    stage_command = commands.add_parser("stage")
    stage_command.add_argument("--root", type=Path, default=ROOT)
    stage_command.add_argument("--version", required=True)
    dispatch_command = commands.add_parser("validate-dispatch")
    dispatch_command.add_argument("--date", required=True)
    dispatch_command.add_argument("--created-at", required=True)
    return root


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    if args.command == "derive":
        print(derive(canonical_base(args.root), args.date, args.source_commit))
    elif args.command == "stage":
        stage(args.root, args.version)
    else:
        validate_dispatch_date(args.date, args.created_at)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (KeyError, OSError, TypeError, ValueError, tomllib.TOMLDecodeError) as error:
        print(f"nightly version: {error}", file=sys.stderr)
        raise SystemExit(1)
