#!/usr/bin/env python3
"""Verify that every capsule-declared Skill is present in its archive."""

from __future__ import annotations

import argparse
import pathlib
import sys
import tarfile
import tomllib


def safe_relative_asset(value: str) -> bool:
    if not value or value.startswith("/") or "\\" in value or "://" in value:
        return False
    parts = value.split("/")
    return all(part not in {"", ".", ".."} for part in parts)


def validate_capsule(path: pathlib.Path) -> None:
    with tarfile.open(path, "r:gz") as archive:
        members = {member.name: member for member in archive.getmembers()}
        manifest_member = members.get("Capsule.toml")
        if manifest_member is None or not manifest_member.isfile():
            raise ValueError(f"{path.name}: missing regular Capsule.toml")
        manifest_file = archive.extractfile(manifest_member)
        if manifest_file is None:
            raise ValueError(f"{path.name}: Capsule.toml cannot be read")
        manifest = tomllib.loads(manifest_file.read().decode("utf-8"))

        for skill in manifest.get("skill", []):
            name = skill.get("name", "<unnamed>")
            asset = skill.get("file")
            if not isinstance(asset, str) or not safe_relative_asset(asset):
                raise ValueError(
                    f"{path.name}: skill {name!r} has unsafe file path {asset!r}"
                )
            member = members.get(asset)
            if member is None or not member.isfile():
                raise ValueError(
                    f"{path.name}: skill {name!r} asset is absent or not a regular file: {asset}"
                )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("artifacts", type=pathlib.Path)
    args = parser.parse_args()

    capsules = sorted(args.artifacts.glob("*.capsule"))
    if not capsules:
        parser.error(f"no .capsule artifacts found in {args.artifacts}")
    for capsule in capsules:
        try:
            validate_capsule(capsule)
        except (OSError, UnicodeError, ValueError, tarfile.TarError) as error:
            print(error, file=sys.stderr)
            return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
