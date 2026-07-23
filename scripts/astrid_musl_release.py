#!/usr/bin/env python3
"""Validate a pinned Astrid musl release and its selected archive."""

from __future__ import annotations

import argparse
import hashlib
import subprocess
import sys
import tomllib
from pathlib import Path
from typing import Any

import musl_release_metadata
import release_metadata


PRODUCT = "astrid-runtime"
REPOSITORY = "astrid-runtime/astrid"
CONTRACTS_REPOSITORY = "astrid-runtime/wit"
LEGACY_TARGETS = (
    "aarch64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
)
TARGET_KEYS = {
    "triple",
    "asset",
    "size",
    "blake3",
    "sha256",
    "sigstore-bundle",
}
LEGACY_ROOT_KEYS = {
    "schema-version",
    "kind",
    "product",
    "repository",
    "version",
    "tag",
    "source-commit",
    "release-workflow-identity",
    "contracts",
    "targets",
}
MUSL_ROOT_KEYS = {
    "schema-version",
    "kind",
    "product",
    "repository",
    "version",
    "tag",
    "source-commit",
    "release-workflow-identity",
    "legacy-release",
    "targets",
}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def exact_keys(value: Any, expected: set[str], context: str) -> dict[str, Any]:
    require(isinstance(value, dict), f"{context} must be a TOML table")
    actual = set(value)
    require(
        actual == expected,
        f"{context} keys differ; missing={sorted(expected - actual)}, "
        f"unknown={sorted(actual - expected)}",
    )
    return value


def load(path: Path) -> dict[str, Any]:
    try:
        with path.open("rb") as source:
            value = tomllib.load(source)
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise ValueError(f"could not parse {path}: {error}") from error
    require(isinstance(value, dict), f"{path} must contain a TOML table")
    return value


def blake3_file(path: Path) -> str:
    try:
        result = subprocess.run(
            ["b3sum", "--", str(path)],
            check=True,
            capture_output=True,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        raise ValueError(f"could not compute BLAKE3 for {path}: {error}") from error
    digest = result.stdout.split(maxsplit=1)[0] if result.stdout else ""
    require(
        release_metadata.HEX_64.fullmatch(digest) is not None,
        f"b3sum returned a malformed digest for {path.name}",
    )
    return digest


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def validate_targets(
    value: Any,
    *,
    version: str,
    expected_targets: tuple[str, ...],
    context: str,
) -> dict[str, dict[str, Any]]:
    require(isinstance(value, list), f"{context} must be an array of tables")
    require(
        len(value) == len(expected_targets),
        f"{context} must contain exactly {len(expected_targets)} entries",
    )
    targets: dict[str, dict[str, Any]] = {}
    for raw in value:
        entry = exact_keys(raw, TARGET_KEYS, f"{context} entry")
        triple = entry["triple"]
        require(
            isinstance(triple, str)
            and triple in expected_targets
            and triple not in targets,
            f"{context} target set is invalid",
        )
        asset = f"astrid-{version}-{triple}.tar.gz"
        require(entry["asset"] == asset, f"{context} asset is invalid for {triple}")
        require(
            entry["sigstore-bundle"] == f"{asset}.sigstore.json",
            f"{context} Sigstore bundle is invalid for {triple}",
        )
        require(
            type(entry["size"]) is int and entry["size"] > 0,
            f"{context} size is invalid for {triple}",
        )
        for algorithm in ("sha256", "blake3"):
            require(
                isinstance(entry[algorithm], str)
                and release_metadata.HEX_64.fullmatch(entry[algorithm]) is not None,
                f"{context} {algorithm} is invalid for {triple}",
            )
        targets[triple] = entry
    require(
        set(targets) == set(expected_targets),
        f"{context} target set is incomplete",
    )
    return targets


def validate_identity(
    value: dict[str, Any],
    *,
    version: str,
    source_commit: str,
    workflow_identity: str,
    context: str,
) -> None:
    require(value["product"] == PRODUCT, f"{context} product is invalid")
    require(value["repository"] == REPOSITORY, f"{context} repository is invalid")
    require(value["version"] == version, f"{context} version differs from the pin")
    require(value["tag"] == f"v{version}", f"{context} tag differs from the pin")
    require(
        value["source-commit"] == source_commit,
        f"{context} source commit differs from the pin",
    )
    require(
        value["release-workflow-identity"] == workflow_identity,
        f"{context} workflow identity differs from the pin",
    )


def validate_release(
    *,
    compatibility_path: Path,
    legacy_path: Path,
    extension_path: Path,
    target: str,
    archive_path: Path,
) -> dict[str, Any]:
    pin = musl_release_metadata.validate_runtime_pin(
        release_metadata.load(compatibility_path), require_ready=True
    )
    require(target in musl_release_metadata.MUSL_TARGETS, "unsupported Astrid musl target")
    require(
        legacy_path.name == pin["legacy-release-metadata-asset"],
        "Astrid legacy metadata filename differs from the pin",
    )
    require(
        extension_path.name == pin["musl-release-metadata-asset"],
        "Astrid musl metadata filename differs from the pin",
    )
    require(
        blake3_file(legacy_path) == pin["legacy-release-metadata-blake3"],
        "Astrid legacy metadata BLAKE3 differs from the pin",
    )
    require(
        blake3_file(extension_path) == pin["musl-release-metadata-blake3"],
        "Astrid musl metadata BLAKE3 differs from the pin",
    )

    legacy = exact_keys(load(legacy_path), LEGACY_ROOT_KEYS, "Astrid legacy metadata")
    require(
        type(legacy["schema-version"]) is int and legacy["schema-version"] == 1,
        "Astrid legacy metadata schema-version must be integer 1",
    )
    require(legacy["kind"] == "astrid-release", "Astrid legacy metadata kind is invalid")
    validate_identity(
        legacy,
        version=pin["version"],
        source_commit=pin["source-commit"],
        workflow_identity=pin["release-workflow-identity"],
        context="Astrid legacy metadata",
    )
    contracts = exact_keys(
        legacy["contracts"], {"repository", "commit"}, "Astrid legacy metadata contracts"
    )
    require(
        contracts["repository"] == CONTRACTS_REPOSITORY,
        "Astrid legacy metadata contracts repository is invalid",
    )
    require(
        isinstance(contracts["commit"], str)
        and release_metadata.COMMIT.fullmatch(contracts["commit"]) is not None,
        "Astrid legacy metadata contracts commit is invalid",
    )
    validate_targets(
        legacy["targets"],
        version=pin["version"],
        expected_targets=LEGACY_TARGETS,
        context="Astrid legacy metadata targets",
    )

    extension = exact_keys(
        load(extension_path), MUSL_ROOT_KEYS, "Astrid musl metadata"
    )
    require(
        type(extension["schema-version"]) is int
        and extension["schema-version"] == 1,
        "Astrid musl metadata schema-version must be integer 1",
    )
    require(
        extension["kind"] == "astrid-release-musl-extension",
        "Astrid musl metadata kind is invalid",
    )
    validate_identity(
        extension,
        version=pin["version"],
        source_commit=pin["source-commit"],
        workflow_identity=pin["release-workflow-identity"],
        context="Astrid musl metadata",
    )
    for key in (
        "product",
        "repository",
        "version",
        "tag",
        "source-commit",
        "release-workflow-identity",
    ):
        require(
            extension[key] == legacy[key],
            f"Astrid musl metadata {key} differs from the legacy metadata",
        )
    legacy_link = exact_keys(
        extension["legacy-release"],
        {"metadata-asset", "metadata-blake3"},
        "Astrid musl metadata legacy-release",
    )
    require(
        legacy_link["metadata-asset"] == legacy_path.name,
        "Astrid musl metadata names a different legacy metadata asset",
    )
    require(
        legacy_link["metadata-blake3"] == pin["legacy-release-metadata-blake3"],
        "Astrid musl metadata does not bind the pinned legacy metadata",
    )
    targets = validate_targets(
        extension["targets"],
        version=pin["version"],
        expected_targets=musl_release_metadata.MUSL_TARGETS,
        context="Astrid musl metadata targets",
    )
    selected = targets[target]
    require(
        archive_path.is_file()
        and not archive_path.is_symlink()
        and archive_path.name == selected["asset"],
        "Astrid musl archive is missing or has a non-canonical name",
    )
    require(
        archive_path.stat().st_size == selected["size"],
        "Astrid musl archive size differs from authenticated metadata",
    )
    require(
        sha256_file(archive_path) == selected["sha256"],
        "Astrid musl archive SHA-256 differs from authenticated metadata",
    )
    require(
        blake3_file(archive_path) == selected["blake3"],
        "Astrid musl archive BLAKE3 differs from authenticated metadata",
    )
    return selected


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    root.add_argument("--compatibility", type=Path, required=True)
    root.add_argument("--legacy", type=Path, required=True)
    root.add_argument("--extension", type=Path, required=True)
    root.add_argument("--target", required=True)
    root.add_argument("--archive", type=Path, required=True)
    return root


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    validate_release(
        compatibility_path=args.compatibility,
        legacy_path=args.legacy,
        extension_path=args.extension,
        target=args.target,
        archive_path=args.archive,
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (KeyError, OSError, ValueError) as error:
        print(f"Astrid musl release: {error}", file=sys.stderr)
        raise SystemExit(1)
