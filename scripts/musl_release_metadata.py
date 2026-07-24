#!/usr/bin/env python3
"""Render and validate AOS's immutable Linux musl metadata extension."""

from __future__ import annotations

import argparse
import hashlib
import sys
from pathlib import Path
from typing import Any

import release_metadata


KIND = "aos-release-musl-extension"
MUSL_TARGETS = (
    "aarch64-unknown-linux-musl",
    "x86_64-unknown-linux-musl",
)
ROOT_KEYS = {
    "schema-version",
    "kind",
    "product",
    "repository",
    "version",
    "tag",
    "source-commit",
    "release-workflow-identity",
    "legacy-release",
    "runtime-musl",
    "targets",
}
LEGACY_KEYS = {"metadata-asset", "metadata-sha256"}
RUNTIME_KEYS = {
    "repository",
    "version",
    "tag",
    "source-commit",
    "release-workflow-identity",
    "legacy-release-metadata-asset",
    "legacy-release-metadata-blake3",
    "musl-release-metadata-asset",
    "musl-release-metadata-blake3",
}
TARGET_KEYS = {"asset", "sha256", "blake3", "sigstore-bundle", "size"}


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def metadata_name(version: str) -> str:
    return f"unicity-aos-{version}-musl-release.toml"


def legacy_metadata_name(version: str) -> str:
    return f"unicity-aos-{version}-release.toml"


def validate_runtime_pin(value: Any, *, require_ready: bool) -> dict[str, Any]:
    root = release_metadata.exact_keys(
        value, {"schema-version", "runtime"}, "musl runtime compatibility"
    )
    release_metadata.require(
        type(root["schema-version"]) is int and root["schema-version"] == 1,
        "musl runtime compatibility schema-version must be integer 1",
    )
    runtime = release_metadata.exact_keys(
        root["runtime"], RUNTIME_KEYS | {"release-ready"}, "musl runtime compatibility.runtime"
    )
    release_metadata.require(
        type(runtime["release-ready"]) is bool,
        "musl runtime compatibility release-ready must be a boolean",
    )
    if require_ready:
        release_metadata.require(
            runtime["release-ready"],
            "musl runtime compatibility release-ready gate is false",
        )
    release_metadata.require(
        runtime["repository"] == "astrid-runtime/astrid",
        "musl runtime repository must be astrid-runtime/astrid",
    )
    version = release_metadata.string(runtime["version"], "musl runtime version")
    release_metadata.require(
        release_metadata.SEMVER.fullmatch(version) is not None,
        "musl runtime version must be canonical semver",
    )
    release_metadata.require(
        runtime["tag"] == f"v{version}", "musl runtime tag/version mismatch"
    )
    release_metadata.require(
        release_metadata.COMMIT.fullmatch(
            release_metadata.string(runtime["source-commit"], "musl runtime source-commit")
        )
        is not None,
        "musl runtime source commit is malformed",
    )
    expected_identity = (
        "https://github.com/astrid-runtime/astrid/.github/workflows/"
        f"release.yml@refs/tags/v{version}"
    )
    release_metadata.require(
        runtime["release-workflow-identity"] == expected_identity,
        "musl runtime workflow identity is not the exact tag identity",
    )
    release_metadata.require(
        runtime["legacy-release-metadata-asset"]
        == f"astrid-{version}-release.toml",
        "musl runtime legacy metadata asset is not canonical",
    )
    release_metadata.require(
        release_metadata.HEX_64.fullmatch(
            release_metadata.string(
                runtime["legacy-release-metadata-blake3"],
                "musl runtime legacy metadata BLAKE3",
            )
        )
        is not None,
        "musl runtime legacy metadata BLAKE3 is malformed",
    )
    if runtime["release-ready"]:
        release_metadata.require(
            runtime["musl-release-metadata-asset"]
            == f"astrid-{version}-musl-release.toml",
            "musl runtime extension metadata asset is not canonical",
        )
        release_metadata.require(
            release_metadata.HEX_64.fullmatch(
                release_metadata.string(
                    runtime["musl-release-metadata-blake3"],
                    "musl runtime extension metadata BLAKE3",
                )
            )
            is not None,
            "musl runtime extension metadata BLAKE3 is malformed",
        )
    else:
        release_metadata.require(
            runtime["musl-release-metadata-asset"]
            == runtime["musl-release-metadata-blake3"]
            == "",
            "unready musl runtime extension fields must remain empty",
        )
    return runtime


def validate_target_table(
    value: Any, *, version: str, context: str
) -> dict[str, dict[str, Any]]:
    table = release_metadata.exact_keys(value, set(MUSL_TARGETS), context)
    for target in MUSL_TARGETS:
        item = release_metadata.exact_keys(
            table[target], TARGET_KEYS, f"{context}.{target}"
        )
        asset = release_metadata.string(item["asset"], f"{context}.{target}.asset")
        expected = f"unicity-aos-{version}-{target}.tar.gz"
        release_metadata.require(
            asset == expected, f"{context}.{target}.asset must be {expected}"
        )
        release_metadata.require(
            item["sigstore-bundle"] == f"{asset}.sigstore.json",
            f"{context}.{target}.sigstore-bundle must name the asset bundle",
        )
        for algorithm in ("sha256", "blake3"):
            release_metadata.require(
                isinstance(item[algorithm], str)
                and release_metadata.HEX_64.fullmatch(item[algorithm]) is not None,
                f"{context}.{target}.{algorithm} is malformed",
            )
        release_metadata.require(
            type(item["size"]) is int and item["size"] > 0,
            f"{context}.{target}.size must be positive",
        )
    return table


def validate_extension(
    value: Any,
    *,
    legacy: dict[str, Any] | None = None,
    legacy_bytes: bytes | None = None,
) -> dict[str, Any]:
    root = release_metadata.exact_keys(value, ROOT_KEYS, "musl release metadata")
    release_metadata.require(
        type(root["schema-version"]) is int and root["schema-version"] == 1,
        "musl release metadata schema-version must be integer 1",
    )
    release_metadata.require(root["kind"] == KIND, f"musl release metadata kind must be {KIND}")
    release_metadata.require(
        root["product"] == release_metadata.PRODUCT,
        f"musl release metadata product must be {release_metadata.PRODUCT}",
    )
    release_metadata.require(
        root["repository"] == release_metadata.REPOSITORY,
        f"musl release metadata repository must be {release_metadata.REPOSITORY}",
    )
    version = release_metadata.string(root["version"], "musl release metadata version")
    release_metadata.require(
        release_metadata.VERSION.fullmatch(version) is not None,
        "musl release metadata version must be calendar semver",
    )
    release_metadata.require(root["tag"] == version, "musl release metadata tag must equal version")
    release_metadata.require(
        release_metadata.COMMIT.fullmatch(
            release_metadata.string(root["source-commit"], "musl release metadata source-commit")
        )
        is not None,
        "musl release metadata source-commit is malformed",
    )
    if release_metadata.is_nightly_version(version):
        release_metadata.require(
            release_metadata.nightly_source_commit(version) == root["source-commit"],
            "musl release metadata nightly version must embed its source commit",
        )
    release_metadata.require(
        root["release-workflow-identity"]
        == release_metadata.release_workflow_identity(version, root["tag"]),
        "musl release metadata workflow identity is not the exact tag identity",
    )
    legacy_link = release_metadata.exact_keys(
        root["legacy-release"], LEGACY_KEYS, "musl release metadata.legacy-release"
    )
    release_metadata.require(
        legacy_link["metadata-asset"] == legacy_metadata_name(version),
        "musl release metadata legacy asset is not canonical",
    )
    release_metadata.require(
        isinstance(legacy_link["metadata-sha256"], str)
        and release_metadata.HEX_64.fullmatch(legacy_link["metadata-sha256"]) is not None,
        "musl release metadata legacy SHA-256 is malformed",
    )
    release_metadata.exact_keys(
        root["runtime-musl"], RUNTIME_KEYS, "musl release metadata.runtime-musl"
    )
    runtime_pin = {
        "schema-version": 1,
        "runtime": {**root["runtime-musl"], "release-ready": True},
    }
    validate_runtime_pin(runtime_pin, require_ready=True)
    validate_target_table(root["targets"], version=version, context="musl release metadata.targets")

    if legacy is not None:
        release_metadata.validate_release(legacy)
        for key in (
            "product",
            "version",
            "tag",
            "source-commit",
            "release-workflow-identity",
        ):
            release_metadata.require(
                root[key] == legacy[key],
                f"musl release metadata {key} differs from the legacy release",
            )
        release_metadata.require(
            legacy_bytes is not None
            and legacy_link["metadata-sha256"]
            == hashlib.sha256(legacy_bytes).hexdigest(),
            "musl release metadata does not bind the authenticated legacy release",
        )
    return root


def build_extension(
    *,
    artifacts: Path,
    legacy_path: Path,
    compatibility_path: Path,
) -> dict[str, Any]:
    legacy_bytes = legacy_path.read_bytes()
    legacy = release_metadata.validate_release(release_metadata.load(legacy_path))
    version = legacy["version"]
    release_metadata.require(
        legacy_path.name == legacy_metadata_name(version),
        f"legacy release metadata must be named {legacy_metadata_name(version)}",
    )
    runtime = validate_runtime_pin(
        release_metadata.load(compatibility_path), require_ready=True
    )
    sha256 = release_metadata.checksum_manifest(artifacts / "SHA256SUMS.txt")
    blake3 = release_metadata.checksum_manifest(artifacts / "BLAKE3SUMS.txt")
    expected_archives = {
        f"unicity-aos-{version}-{target}.tar.gz"
        for target in (*release_metadata.TARGETS, *MUSL_TARGETS)
    }
    sha_archives = {name for name in sha256 if name.endswith(".tar.gz")}
    blake_archives = {name for name in blake3 if name.endswith(".tar.gz")}
    release_metadata.require(
        sha_archives == expected_archives and blake_archives == expected_archives,
        "musl metadata requires checksums for exactly all six release archives",
    )
    targets: dict[str, dict[str, Any]] = {}
    for target in MUSL_TARGETS:
        asset = f"unicity-aos-{version}-{target}.tar.gz"
        path = artifacts / asset
        release_metadata.require(
            path.is_file() and not path.is_symlink(),
            f"missing regular musl release asset: {asset}",
        )
        targets[target] = {
            "asset": asset,
            "sha256": sha256[asset],
            "blake3": blake3[asset],
            "sigstore-bundle": f"{asset}.sigstore.json",
            "size": path.stat().st_size,
        }
    extension = {
        "schema-version": 1,
        "kind": KIND,
        "product": legacy["product"],
        "repository": release_metadata.REPOSITORY,
        "version": version,
        "tag": legacy["tag"],
        "source-commit": legacy["source-commit"],
        "release-workflow-identity": legacy["release-workflow-identity"],
        "legacy-release": {
            "metadata-asset": legacy_path.name,
            "metadata-sha256": hashlib.sha256(legacy_bytes).hexdigest(),
        },
        "runtime-musl": {
            key: runtime[key] for key in RUNTIME_KEYS
        },
        "targets": targets,
    }
    validate_extension(extension, legacy=legacy, legacy_bytes=legacy_bytes)
    return extension


def render_extension(value: dict[str, Any]) -> str:
    root = validate_extension(value)
    quote = release_metadata.quoted
    lines = [
        "schema-version = 1",
        f"kind = {quote(root['kind'])}",
        f"product = {quote(root['product'])}",
        f"repository = {quote(root['repository'])}",
        f"version = {quote(root['version'])}",
        f"tag = {quote(root['tag'])}",
        f"source-commit = {quote(root['source-commit'])}",
        f"release-workflow-identity = {quote(root['release-workflow-identity'])}",
        "",
        "[legacy-release]",
        f"metadata-asset = {quote(root['legacy-release']['metadata-asset'])}",
        f"metadata-sha256 = {quote(root['legacy-release']['metadata-sha256'])}",
        "",
        "[runtime-musl]",
    ]
    for key in (
        "repository",
        "version",
        "tag",
        "source-commit",
        "release-workflow-identity",
        "legacy-release-metadata-asset",
        "legacy-release-metadata-blake3",
        "musl-release-metadata-asset",
        "musl-release-metadata-blake3",
    ):
        lines.append(f"{key} = {quote(root['runtime-musl'][key])}")
    for target in MUSL_TARGETS:
        item = root["targets"][target]
        lines.extend(
            [
                "",
                f"[targets.{target}]",
                f"asset = {quote(item['asset'])}",
                f"sha256 = {quote(item['sha256'])}",
                f"blake3 = {quote(item['blake3'])}",
                f"sigstore-bundle = {quote(item['sigstore-bundle'])}",
                f"size = {item['size']}",
            ]
        )
    return "\n".join(lines) + "\n"


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)
    render = commands.add_parser("render")
    render.add_argument("--artifacts", type=Path, required=True)
    render.add_argument("--legacy-release", type=Path, required=True)
    render.add_argument("--runtime-compatibility", type=Path, required=True)
    render.add_argument("--output", type=Path, required=True)
    validate = commands.add_parser("validate")
    validate.add_argument("path", type=Path)
    validate.add_argument("--legacy-release", type=Path, required=True)
    return root


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    if args.command == "render":
        value = build_extension(
            artifacts=args.artifacts,
            legacy_path=args.legacy_release,
            compatibility_path=args.runtime_compatibility,
        )
        args.output.write_text(render_extension(value), encoding="utf-8")
        return 0
    legacy_bytes = args.legacy_release.read_bytes()
    validate_extension(
        release_metadata.load(args.path),
        legacy=release_metadata.load(args.legacy_release),
        legacy_bytes=legacy_bytes,
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (KeyError, OSError, ValueError) as error:
        print(f"musl release metadata: {error}", file=sys.stderr)
        raise SystemExit(1)
