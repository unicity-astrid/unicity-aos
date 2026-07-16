#!/usr/bin/env python3
"""Authenticate the complete asset contract of a recoverable AOS release draft."""

from __future__ import annotations

import argparse
import hashlib
import stat
import sys
from pathlib import Path

import capsule_release
import release_metadata


ROOT = Path(__file__).resolve().parent.parent
FIXED_PAYLOADS = (
    "BLAKE3SUMS.txt",
    "SHA256SUMS.txt",
    "runtime-compatibility.toml",
)


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def validate_release_assets(
    directory: Path,
    *,
    version: str,
    source_commit: str,
    compatibility_path: Path | None = None,
) -> list[str]:
    require(directory.is_dir() and not directory.is_symlink(), "release assets must be a directory")
    entries = list(directory.iterdir())
    invalid = sorted(
        path.name
        for path in entries
        if path.is_symlink() or not stat.S_ISREG(path.lstat().st_mode)
    )
    require(not invalid, f"release assets contain non-regular entries: {invalid}")

    metadata_name = f"unicity-aos-{version}-release.toml"
    metadata_path = directory / metadata_name
    metadata = release_metadata.validate_release(
        release_metadata.load(metadata_path), require_ready=True
    )
    require(metadata["version"] == version, "release metadata version does not match the tag")
    require(metadata["tag"] == version, "release metadata tag does not match the tag")
    require(
        metadata["source-commit"] == source_commit,
        "release metadata source commit does not match the tag commit",
    )

    specs = capsule_release.source_contract()
    capsules = {spec.asset for spec in specs}
    targets = {item["asset"] for item in metadata["targets"].values()}
    checksummed = targets | capsules
    payloads = checksummed | set(FIXED_PAYLOADS) | {metadata_name}
    expected = payloads | {f"{name}.sigstore.json" for name in payloads}
    actual = {path.name for path in entries}
    require(
        actual == expected,
        f"release asset set differs; missing={sorted(expected - actual)}, "
        f"unexpected={sorted(actual - expected)}",
    )

    sha256 = release_metadata.checksum_manifest(directory / "SHA256SUMS.txt")
    blake3 = release_metadata.checksum_manifest(directory / "BLAKE3SUMS.txt")
    require(set(sha256) == checksummed, "SHA-256 manifest does not cover the exact payload set")
    require(set(blake3) == checksummed, "BLAKE3 manifest does not cover the exact payload set")
    for name in checksummed:
        path = directory / name
        require(
            sha256_file(path) == sha256[name],
            f"SHA-256 mismatch for {name}",
        )

    for item in metadata["targets"].values():
        name = item["asset"]
        require(item["sha256"] == sha256[name], f"release metadata SHA-256 mismatch for {name}")
        require(item["blake3"] == blake3[name], f"release metadata BLAKE3 mismatch for {name}")
        require(item["size"] == (directory / name).stat().st_size, f"release metadata size mismatch for {name}")

    compatibility_path = compatibility_path or ROOT / "release" / "runtime-compatibility.toml"
    require(
        (directory / "runtime-compatibility.toml").read_bytes()
        == compatibility_path.read_bytes(),
        "published runtime compatibility does not match the tagged source",
    )
    compatibility = release_metadata.load(compatibility_path)
    runtime = compatibility["runtime"]
    contracts = compatibility["contracts"]
    for key in (
        "repository",
        "version",
        "tag",
        "release-workflow-identity",
        "release-metadata-available",
        "source-commit",
        "release-metadata-asset",
        "release-metadata-blake3",
    ):
        require(metadata["runtime"][key] == runtime[key], f"runtime compatibility mismatch for {key}")
    for key in ("repository", "commit", "sdk-rust-version", "sdk-rust-commit"):
        require(metadata["contracts"][key] == contracts[key], f"contract compatibility mismatch for {key}")
    require(
        metadata["gates"]
        == {
            "release-ready": runtime["release-ready"],
            "upgrade-self-heal-ready": runtime["upgrade-self-heal-ready"],
        },
        "release readiness gates do not match the tagged source",
    )

    for spec in specs:
        capsule_release.validate_archive(directory / spec.asset, spec)
    return sorted(payloads)


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    root.add_argument("--artifacts", type=Path, required=True)
    root.add_argument("--version", required=True)
    root.add_argument("--source-commit", required=True)
    return root


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    for payload in validate_release_assets(
        args.artifacts,
        version=args.version,
        source_commit=args.source_commit,
    ):
        print(payload)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (
        KeyError,
        OSError,
        ValueError,
        capsule_release.ContractError,
    ) as error:
        print(f"release publication: {error}", file=sys.stderr)
        raise SystemExit(1)
