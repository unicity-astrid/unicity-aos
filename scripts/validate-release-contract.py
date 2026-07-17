#!/usr/bin/env python3
"""Validate that every independently published AOS compatibility pin agrees."""

from __future__ import annotations

import argparse
import datetime as dt
import re
import sys
from pathlib import Path
from typing import Any

try:
    import tomllib
except ModuleNotFoundError:
    tomllib = None


ROOT = Path(__file__).resolve().parent.parent


def strings(path: str) -> dict[tuple[str, str], str]:
    """Read the string values used by the release contract's TOML files."""
    values: dict[tuple[str, str], str] = {}
    section = ""
    for line in (ROOT / path).read_text(encoding="utf-8").splitlines():
        section_match = re.match(r"\s*\[([^]]+)]\s*$", line)
        if section_match:
            section = section_match.group(1)
            continue
        value_match = re.match(r'\s*([A-Za-z0-9_-]+)\s*=\s*"([^"]*)"', line)
        if value_match:
            values[(section, value_match.group(1))] = value_match.group(2)
    return values


def workspace_dependency(values: dict[tuple[str, str], str], name: str) -> str:
    direct = values.get(("workspace.dependencies", name))
    if direct is not None:
        return direct
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    match = re.search(
        rf'^\s*{re.escape(name)}\s*=\s*\{{[^\n]*\bversion\s*=\s*"([^"]+)"',
        text,
        re.MULTILINE,
    )
    if match:
        return match.group(1)
    raise ValueError(f"{name} must have an exact workspace version")


def readiness_metadata(path: str | Path) -> dict[str, Any]:
    """Parse release metadata with the standard-library TOML implementation."""
    if tomllib is None:
        raise ValueError(
            "Python 3.11 or newer with standard-library tomllib is required"
        )
    metadata_path = Path(path)
    if not metadata_path.is_absolute():
        metadata_path = ROOT / metadata_path
    with metadata_path.open("rb") as file:
        return tomllib.load(file)


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def validate_product_version(value: str, *, allow_nightly: bool) -> None:
    canonical = r"(?:202[6-9]|20[3-9][0-9])\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
    accepted = canonical
    if allow_nightly:
        accepted = rf"(?:{canonical}|{canonical}-nightly\.[0-9]{{8}}\.g[0-9a-f]{{40}})"
    require(
        re.fullmatch(accepted, value) is not None,
        "product version must be an allowed calendar SemVer release identity",
    )
    if "-nightly." in value:
        nightly_date = value.rsplit("-nightly.", 1)[1].split(".g", 1)[0]
        try:
            dt.datetime.strptime(nightly_date, "%Y%m%d")
        except ValueError as error:
            raise ValueError("product nightly version contains an invalid date") from error


def validate_release_readiness(
    metadata: dict[str, Any], *, require_release_ready: bool
) -> bool:
    """Validate the compatibility schema and optional publication gate."""
    require(
        metadata.get("schema-version") == 1,
        "runtime-compatibility schema-version must be 1",
    )
    runtime = metadata.get("runtime")
    require(isinstance(runtime, dict), "runtime-compatibility must define [runtime]")
    release_ready = runtime.get("release-ready")
    upgrade_self_heal_ready = runtime.get("upgrade-self-heal-ready")
    require(
        type(release_ready) is bool,
        "runtime release-ready must be a boolean",
    )
    require(
        type(upgrade_self_heal_ready) is bool,
        "runtime upgrade-self-heal-ready must be a boolean",
    )
    if require_release_ready:
        require(
            release_ready,
            "runtime release-ready is false; refusing to publish this staged product",
        )
        require(
            upgrade_self_heal_ready,
            "runtime upgrade-self-heal-ready is false; refusing to publish before the exact candidate passes upgrade, self-heal, and boot validation",
        )
    return release_ready


def parse_args(argv: list[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Validate the Unicity AOS release compatibility contract."
    )
    parser.add_argument(
        "--require-release-ready",
        action="store_true",
        help="fail unless the pinned runtime has been explicitly approved for publication",
    )
    parser.add_argument(
        "--allow-nightly",
        action="store_true",
        help="accept the strict deterministic nightly suffix on the product version",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    workspace = strings("Cargo.toml")
    product = strings("crates/unicity-aos-bootstrap/Cargo.toml")
    distro = strings("distros/community/unicity-ce/Distro.toml")
    compatibility = strings("release/runtime-compatibility.toml")
    validate_release_readiness(
        readiness_metadata("release/runtime-compatibility.toml"),
        require_release_ready=args.require_release_ready,
    )

    product_version = product[("package", "version")]
    runtime_version = compatibility[("runtime", "version")]
    sdk_version = compatibility[("contracts", "sdk-rust-version")]
    canonical_semver = r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"

    validate_product_version(product_version, allow_nightly=args.allow_nightly)
    require(
        compatibility[("product", "version")] == product_version,
        "runtime-compatibility product version does not match the AOS crate",
    )
    require(
        distro[("distro", "version")] == product_version,
        "distro version does not match the AOS crate",
    )
    require(
        re.fullmatch(canonical_semver, runtime_version) is not None,
        "runtime version must be canonical semver",
    )
    require(
        re.fullmatch(canonical_semver, sdk_version) is not None,
        "SDK version must be canonical semver",
    )
    require(
        compatibility[("runtime", "repository")] == "astrid-runtime/astrid",
        "runtime repository must be astrid-runtime/astrid",
    )
    require(
        compatibility[("contracts", "repository")] == "astrid-runtime/wit",
        "contracts repository must be astrid-runtime/wit",
    )
    require(
        compatibility[("runtime", "tag")] == f"v{runtime_version}",
        "runtime tag does not match the pinned runtime version",
    )
    require(
        compatibility[("runtime", "version-requirement")] == f"={runtime_version}",
        "runtime version requirement must be an exact pin",
    )
    require(
        distro[("distro", "astrid-version")] == f"={runtime_version}",
        "distro Astrid requirement does not match the bundled runtime",
    )
    for dependency in ("astrid-core", "astrid-uplink"):
        require(
            workspace_dependency(workspace, dependency) == f"={runtime_version}",
            f"workspace {dependency} must exactly pin the bundled runtime",
        )
    require(
        workspace_dependency(workspace, "astrid-sdk") == f"={sdk_version}",
        "workspace astrid-sdk must exactly pin compatibility metadata",
    )
    require(
        re.fullmatch(r"[0-9a-f]{40}", compatibility[("contracts", "commit")])
        is not None,
        "WIT compatibility commit must be a full lowercase Git commit",
    )
    require(
        re.fullmatch(
            r"[0-9a-f]{40}", compatibility[("contracts", "sdk-rust-commit")]
        )
        is not None,
        "SDK compatibility commit must be a full lowercase Git commit",
    )
    identity = compatibility[("runtime", "release-workflow-identity")]
    approved_identities = {
        f"https://github.com/astrid-runtime/astrid/.github/workflows/release.yml@refs/tags/v{runtime_version}",
        f"https://github.com/unicity-astrid/astrid/.github/workflows/release.yml@refs/tags/v{runtime_version}",
    }
    require(identity in approved_identities, "runtime Sigstore identity must be an approved exact tag workflow identity")
    runtime = readiness_metadata("release/runtime-compatibility.toml")["runtime"]
    metadata_available = runtime.get("release-metadata-available")
    require(
        type(metadata_available) is bool,
        "runtime release-metadata-available must be a boolean",
    )
    source_commit = runtime.get("source-commit")
    metadata_asset = runtime.get("release-metadata-asset")
    metadata_blake3 = runtime.get("release-metadata-blake3")
    for key, value in (
        ("source-commit", source_commit),
        ("release-metadata-asset", metadata_asset),
        ("release-metadata-blake3", metadata_blake3),
    ):
        require(type(value) is str, f"runtime {key} must be a string")
    if metadata_available:
        require(
            identity.startswith("https://github.com/astrid-runtime/astrid/"),
            "new runtime metadata must use the astrid-runtime workflow identity",
        )
        require(
            re.fullmatch(r"[0-9a-f]{40}", source_commit) is not None,
            "runtime source-commit must be a full lowercase Git commit",
        )
        require(
            metadata_asset == f"astrid-{runtime_version}-release.toml",
            "runtime release metadata asset must match the pinned runtime version",
        )
        require(
            re.fullmatch(r"[0-9a-f]{64}", metadata_blake3) is not None,
            "runtime release metadata BLAKE3 must be lowercase hexadecimal",
        )
    else:
        require(
            source_commit == metadata_asset == metadata_blake3 == "",
            "unavailable runtime release metadata fields must remain empty",
        )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (KeyError, TypeError, ValueError) as error:
        print(f"release contract: {error}", file=sys.stderr)
        raise SystemExit(1)
