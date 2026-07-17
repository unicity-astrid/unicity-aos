#!/usr/bin/env python3
"""Render and validate signed AOS release and channel metadata.

The files produced here are deliberately small, strict TOML documents.  The
website installer can parse the channel document with POSIX tools, while the
release and promotion workflows use this module as the authoritative schema
validator.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import re
import sys
from pathlib import Path
from typing import Any

import tomllib


PRODUCT = "unicity-aos-ce"
REPOSITORY = "unicity-aos/aos-ce"
TARGETS = (
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
)
CHANNELS = ("stable", "dev", "nightly")
MAX_GENERATION = 999_999_999_999_999_999
MAX_FUTURE_SKEW = dt.timedelta(minutes=5)
MAX_CHANNEL_LIFETIME = {
    "stable": dt.timedelta(days=30),
    "dev": dt.timedelta(days=7),
    "nightly": dt.timedelta(days=2),
}
HEX_64 = re.compile(r"[0-9a-f]{64}")
COMMIT = re.compile(r"[0-9a-f]{40}")
CANONICAL_VERSION = (
    r"(?:202[6-9]|20[3-9][0-9])\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
)
NIGHTLY_VERSION = re.compile(
    rf"{CANONICAL_VERSION}-nightly\.[0-9]{{8}}\.g[0-9a-f]{{40}}"
)
VERSION = re.compile(rf"(?:{CANONICAL_VERSION}|{NIGHTLY_VERSION.pattern})")
SEMVER = re.compile(
    r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
)


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def is_nightly_version(version: str) -> bool:
    return nightly_source_commit(version) is not None


def nightly_source_commit(version: str) -> str | None:
    match = NIGHTLY_VERSION.fullmatch(version)
    if match is None:
        return None
    date = version.rsplit("-nightly.", 1)[1].split(".g", 1)[0]
    try:
        dt.datetime.strptime(date, "%Y%m%d")
    except ValueError:
        return None
    return version.rsplit(".g", 1)[1]


def release_workflow_identity(version: str, tag: str) -> str:
    require(tag == version, "release tag must equal version")
    return f"https://github.com/{REPOSITORY}/.github/workflows/release.yml@refs/tags/{tag}"


def exact_keys(value: Any, expected: set[str], context: str) -> dict[str, Any]:
    require(isinstance(value, dict), f"{context} must be a TOML table")
    actual = set(value)
    missing = sorted(expected - actual)
    unknown = sorted(actual - expected)
    require(not missing, f"{context} is missing keys: {', '.join(missing)}")
    require(not unknown, f"{context} has unknown keys: {', '.join(unknown)}")
    return value


def string(value: Any, context: str) -> str:
    require(isinstance(value, str) and value != "", f"{context} must be a non-empty string")
    require("\n" not in value and "\r" not in value, f"{context} must be one line")
    return value


def timestamp(value: Any, context: str) -> dt.datetime:
    encoded = string(value, context)
    require(
        re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z", encoded)
        is not None,
        f"{context} must be canonical UTC RFC3339 seconds",
    )
    return dt.datetime.fromisoformat(encoded.replace("Z", "+00:00"))


def quoted(value: str) -> str:
    require(re.fullmatch(r"[^\"\\\r\n]*", value) is not None, "metadata strings must not require TOML escaping")
    return f'"{value}"'


def checksum_manifest(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9_.+-]+)", line)
        require(match is not None, f"{path}:{number}: malformed checksum entry")
        digest, asset = match.groups()
        require(asset not in values, f"{path}: duplicate checksum entry for {asset}")
        values[asset] = digest
    return values


def validate_target_table(
    targets: Any, *, version: str, context: str
) -> dict[str, dict[str, Any]]:
    table = exact_keys(targets, set(TARGETS), context)
    for target in TARGETS:
        item = exact_keys(
            table[target],
            {"asset", "sha256", "blake3", "sigstore-bundle", "size"},
            f"{context}.{target}",
        )
        asset = string(item["asset"], f"{context}.{target}.asset")
        expected_asset = f"unicity-aos-{version}-{target}.tar.gz"
        require(asset == expected_asset, f"{context}.{target}.asset must be {expected_asset}")
        require(
            string(item["sigstore-bundle"], f"{context}.{target}.sigstore-bundle")
            == f"{asset}.sigstore.json",
            f"{context}.{target}.sigstore-bundle must name the asset bundle",
        )
        for algorithm in ("sha256", "blake3"):
            digest = string(item[algorithm], f"{context}.{target}.{algorithm}")
            require(HEX_64.fullmatch(digest) is not None, f"{context}.{target}.{algorithm} is malformed")
        require(type(item["size"]) is int and item["size"] > 0, f"{context}.{target}.size must be positive")
    return table


def validate_release(metadata: Any, *, require_ready: bool = False) -> dict[str, Any]:
    root = exact_keys(
        metadata,
        {
            "schema-version",
            "kind",
            "product",
            "version",
            "tag",
            "source-commit",
            "published-at",
            "release-workflow-identity",
            "runtime",
            "contracts",
            "gates",
            "targets",
        },
        "release metadata",
    )
    require(type(root["schema-version"]) is int and root["schema-version"] == 1, "release metadata schema-version must be integer 1")
    require(root["kind"] == "aos-release", "release metadata kind must be aos-release")
    require(root["product"] == PRODUCT, f"release metadata product must be {PRODUCT}")
    version = string(root["version"], "release metadata version")
    require(VERSION.fullmatch(version) is not None, "release metadata version must be calendar semver")
    tag = string(root["tag"], "release metadata tag")
    require(tag == version, "release metadata tag must equal version")
    require(COMMIT.fullmatch(string(root["source-commit"], "release metadata source-commit")) is not None, "release metadata source-commit is malformed")
    nightly_commit = nightly_source_commit(version)
    if "-nightly." in version:
        require(nightly_commit is not None, "release metadata nightly version is malformed")
        require(nightly_commit == root["source-commit"], "release metadata nightly version must embed its source commit")
    timestamp(root["published-at"], "release metadata published-at")
    expected_identity = release_workflow_identity(version, tag)
    require(
        string(root["release-workflow-identity"], "release metadata release-workflow-identity")
        == expected_identity,
        "release metadata must use the exact tag release workflow identity",
    )

    runtime = exact_keys(
        root["runtime"],
        {
            "repository",
            "version",
            "tag",
            "release-workflow-identity",
            "release-metadata-available",
            "source-commit",
            "release-metadata-asset",
            "release-metadata-blake3",
        },
        "release metadata.runtime",
    )
    require(runtime["repository"] == "astrid-runtime/astrid", "release metadata runtime repository must be astrid-runtime/astrid")
    runtime_version = string(runtime["version"], "release metadata.runtime.version")
    require(SEMVER.fullmatch(runtime_version) is not None, "release metadata runtime version must be canonical semver")
    require(runtime["tag"] == f"v{runtime_version}", "release metadata runtime tag/version mismatch")
    runtime_identity = string(runtime["release-workflow-identity"], "release metadata.runtime.release-workflow-identity")
    allowed_runtime_identities = {
        f"https://github.com/astrid-runtime/astrid/.github/workflows/release.yml@refs/tags/v{runtime_version}",
        f"https://github.com/unicity-astrid/astrid/.github/workflows/release.yml@refs/tags/v{runtime_version}",
    }
    require(runtime_identity in allowed_runtime_identities, "release metadata runtime workflow identity is not an approved exact tag identity")
    require(type(runtime["release-metadata-available"]) is bool, "release metadata.runtime.release-metadata-available must be a boolean")
    if runtime["release-metadata-available"]:
        require(runtime_identity.startswith("https://github.com/astrid-runtime/astrid/"), "new Astrid release metadata must use the astrid-runtime workflow identity")
        require(COMMIT.fullmatch(string(runtime["source-commit"], "release metadata.runtime.source-commit")) is not None, "release metadata runtime source commit is malformed")
        require(runtime["release-metadata-asset"] == f"astrid-{runtime['version']}-release.toml", "release metadata runtime metadata asset is not canonical")
        require(HEX_64.fullmatch(string(runtime["release-metadata-blake3"], "release metadata.runtime.release-metadata-blake3")) is not None, "release metadata runtime metadata BLAKE3 is malformed")
    else:
        for key in ("source-commit", "release-metadata-asset", "release-metadata-blake3"):
            require(runtime[key] == "", f"release metadata.runtime.{key} must be empty while metadata is unavailable")

    contracts = exact_keys(
        root["contracts"],
        {"repository", "commit", "sdk-rust-version", "sdk-rust-commit"},
        "release metadata.contracts",
    )
    require(contracts["repository"] == "astrid-runtime/wit", "release metadata contracts repository must be astrid-runtime/wit")
    require(SEMVER.fullmatch(string(contracts["sdk-rust-version"], "release metadata.contracts.sdk-rust-version")) is not None, "release metadata SDK version must be canonical semver")
    for key in ("commit", "sdk-rust-commit"):
        require(COMMIT.fullmatch(string(contracts[key], f"release metadata.contracts.{key}")) is not None, f"release metadata.contracts.{key} is malformed")

    gates = exact_keys(
        root["gates"],
        {"release-ready", "upgrade-self-heal-ready"},
        "release metadata.gates",
    )
    for key in gates:
        require(type(gates[key]) is bool, f"release metadata.gates.{key} must be a boolean")
    if require_ready:
        require(gates["release-ready"], "release metadata release-ready gate is false")
        require(gates["upgrade-self-heal-ready"], "release metadata upgrade-self-heal-ready gate is false")

    validate_target_table(root["targets"], version=version, context="release metadata.targets")
    return root


def validate_channel(
    metadata: Any,
    *,
    expected_channel: str | None = None,
    minimum_generation: int | None = None,
    now: dt.datetime | None = None,
) -> dict[str, Any]:
    root = exact_keys(
        metadata,
        {
            "schema-version",
            "kind",
            "product",
            "channel",
            "generation",
            "published-at",
            "expires-at",
            "release",
            "targets",
        },
        "channel metadata",
    )
    require(type(root["schema-version"]) is int and root["schema-version"] == 1, "channel metadata schema-version must be integer 1")
    require(root["kind"] == "aos-channel", "channel metadata kind must be aos-channel")
    require(root["product"] == PRODUCT, f"channel metadata product must be {PRODUCT}")
    channel = string(root["channel"], "channel metadata channel")
    require(channel in CHANNELS, "channel metadata channel must be stable, dev, or nightly")
    if expected_channel is not None:
        require(channel == expected_channel, f"channel metadata names {channel}, expected {expected_channel}")
    generation = root["generation"]
    require(
        type(generation) is int and 0 < generation <= MAX_GENERATION,
        f"channel metadata generation must be between 1 and {MAX_GENERATION}",
    )
    if minimum_generation is not None:
        require(generation >= minimum_generation, "channel metadata generation is older than the accepted generation")
    published = timestamp(root["published-at"], "channel metadata published-at")
    expires = timestamp(root["expires-at"], "channel metadata expires-at")
    require(expires > published, "channel metadata expires-at must be after published-at")
    require(
        expires - published <= MAX_CHANNEL_LIFETIME[channel],
        "channel metadata lifetime exceeds the maximum for its channel",
    )
    if now is not None:
        require(now <= expires, "channel metadata has expired")
        require(
            published <= now + MAX_FUTURE_SKEW,
            "channel metadata published-at is unreasonably far in the future",
        )

    release = exact_keys(
        root["release"],
        {
            "repository",
            "version",
            "tag",
            "source-commit",
            "metadata-asset",
            "metadata-sha256",
            "release-workflow-identity",
        },
        "channel metadata.release",
    )
    require(release["repository"] == REPOSITORY, f"channel release repository must be {REPOSITORY}")
    version = string(release["version"], "channel metadata.release.version")
    require(VERSION.fullmatch(version) is not None, "channel release version must be calendar semver")
    if channel == "nightly":
        require(is_nightly_version(version), "nightly channel must point to a nightly prerelease")
    else:
        require("-nightly." not in version, "stable and dev channels must point to canonical releases")
    require(release["tag"] == version, "channel release tag must equal version")
    require(COMMIT.fullmatch(string(release["source-commit"], "channel metadata.release.source-commit")) is not None, "channel release source-commit is malformed")
    nightly_commit = nightly_source_commit(version)
    if is_nightly_version(version):
        require(nightly_commit == release["source-commit"], "nightly channel version must embed its source commit")
    require(release["metadata-asset"] == f"unicity-aos-{version}-release.toml", "channel release metadata asset is not canonical")
    require(HEX_64.fullmatch(string(release["metadata-sha256"], "channel metadata.release.metadata-sha256")) is not None, "channel release metadata SHA-256 is malformed")
    expected_identity = release_workflow_identity(version, release["tag"])
    require(release["release-workflow-identity"] == expected_identity, "channel release workflow identity is not the exact tag identity")
    validate_target_table(root["targets"], version=version, context="channel metadata.targets")
    return root


def validate_channel_release(
    channel_metadata: Any,
    release_metadata: Any,
    release_bytes: bytes,
    *,
    expected_channel: str | None = None,
    expected_generation: int | None = None,
    minimum_generation: int | None = None,
    now: dt.datetime | None = None,
    require_ready: bool = False,
) -> tuple[dict[str, Any], dict[str, Any]]:
    channel = validate_channel(
        channel_metadata,
        expected_channel=expected_channel,
        minimum_generation=minimum_generation,
        now=now,
    )
    release = validate_release(release_metadata, require_ready=require_ready)
    if expected_generation is not None:
        require(
            channel["generation"] == expected_generation,
            f"channel metadata generation must equal {expected_generation}",
        )

    version = release["version"]
    expected_release = {
        "repository": REPOSITORY,
        "version": version,
        "tag": release["tag"],
        "source-commit": release["source-commit"],
        "metadata-asset": f"unicity-aos-{version}-release.toml",
        "metadata-sha256": hashlib.sha256(release_bytes).hexdigest(),
        "release-workflow-identity": release["release-workflow-identity"],
    }
    require(
        channel["release"] == expected_release,
        "channel metadata does not identify the authenticated release metadata exactly",
    )
    require(
        channel["targets"] == release["targets"],
        "channel metadata targets do not match the authenticated release metadata",
    )
    return channel, release


def load(path: Path) -> dict[str, Any]:
    with path.open("rb") as file:
        return tomllib.load(file)


def write_target_tables(lines: list[str], targets: dict[str, dict[str, Any]]) -> None:
    for target in TARGETS:
        item = targets[target]
        lines.extend(
            [
                "",
                f"[targets.{target}]",
                f"asset = {quoted(item['asset'])}",
                f"sha256 = {quoted(item['sha256'])}",
                f"blake3 = {quoted(item['blake3'])}",
                f"sigstore-bundle = {quoted(item['sigstore-bundle'])}",
                f"size = {item['size']}",
            ]
        )


def render_release(args: argparse.Namespace) -> None:
    root = Path(__file__).resolve().parent.parent
    compatibility = load(root / "release/runtime-compatibility.toml")
    runtime = compatibility["runtime"]
    contracts = compatibility["contracts"]
    sha256 = checksum_manifest(args.sha256)
    blake3 = checksum_manifest(args.blake3)
    targets: dict[str, dict[str, Any]] = {}
    for target in TARGETS:
        asset = f"unicity-aos-{args.version}-{target}.tar.gz"
        path = args.artifacts / asset
        require(path.is_file() and not path.is_symlink(), f"missing regular release asset: {asset}")
        require(asset in sha256 and asset in blake3, f"missing checksums for {asset}")
        targets[target] = {
            "asset": asset,
            "sha256": sha256[asset],
            "blake3": blake3[asset],
            "sigstore-bundle": f"{asset}.sigstore.json",
            "size": path.stat().st_size,
        }
    identity = release_workflow_identity(args.version, args.tag)
    lines = [
        "schema-version = 1",
        'kind = "aos-release"',
        f"product = {quoted(PRODUCT)}",
        f"version = {quoted(args.version)}",
        f"tag = {quoted(args.tag)}",
        f"source-commit = {quoted(args.source_commit)}",
        f"published-at = {quoted(args.published_at)}",
        f"release-workflow-identity = {quoted(identity)}",
        "",
        "[runtime]",
        f"repository = {quoted(runtime['repository'])}",
        f"version = {quoted(runtime['version'])}",
        f"tag = {quoted(runtime['tag'])}",
        f"release-workflow-identity = {quoted(runtime['release-workflow-identity'])}",
        f"release-metadata-available = {str(runtime['release-metadata-available']).lower()}",
        f"source-commit = {quoted(runtime['source-commit'])}",
        f"release-metadata-asset = {quoted(runtime['release-metadata-asset'])}",
        f"release-metadata-blake3 = {quoted(runtime['release-metadata-blake3'])}",
        "",
        "[contracts]",
        f"repository = {quoted(contracts['repository'])}",
        f"commit = {quoted(contracts['commit'])}",
        f"sdk-rust-version = {quoted(contracts['sdk-rust-version'])}",
        f"sdk-rust-commit = {quoted(contracts['sdk-rust-commit'])}",
        "",
        "[gates]",
        f"release-ready = {str(runtime['release-ready']).lower()}",
        f"upgrade-self-heal-ready = {str(runtime['upgrade-self-heal-ready']).lower()}",
    ]
    write_target_tables(lines, targets)
    args.output.write_text("\n".join(lines) + "\n", encoding="utf-8")
    validate_release(load(args.output))


def render_channel(args: argparse.Namespace) -> None:
    release = validate_release(load(args.release_metadata), require_ready=args.require_ready)
    actual_digest = hashlib.sha256(args.release_metadata.read_bytes()).hexdigest()
    if args.release_metadata_sha256 is not None:
        require(actual_digest == args.release_metadata_sha256, "release metadata SHA-256 does not match the expected digest")
    published = timestamp(args.published_at, "channel published-at")
    expires = timestamp(args.expires_at, "channel expires-at")
    require(expires > published, "channel expires-at must be after published-at")
    version = release["version"]
    lines = [
        "schema-version = 1",
        'kind = "aos-channel"',
        f"product = {quoted(PRODUCT)}",
        f"channel = {quoted(args.channel)}",
        f"generation = {args.generation}",
        f"published-at = {quoted(args.published_at)}",
        f"expires-at = {quoted(args.expires_at)}",
        "",
        "[release]",
        f"repository = {quoted(REPOSITORY)}",
        f"version = {quoted(version)}",
        f"tag = {quoted(release['tag'])}",
        f"source-commit = {quoted(release['source-commit'])}",
        f"metadata-asset = {quoted(f'unicity-aos-{version}-release.toml')}",
        f"metadata-sha256 = {quoted(actual_digest)}",
        f"release-workflow-identity = {quoted(release['release-workflow-identity'])}",
    ]
    write_target_tables(lines, release["targets"])
    args.output.write_text("\n".join(lines) + "\n", encoding="utf-8")
    validate_channel(load(args.output), expected_channel=args.channel)


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)

    release = commands.add_parser("render-release")
    release.add_argument("--version", required=True)
    release.add_argument("--tag", required=True)
    release.add_argument("--source-commit", required=True)
    release.add_argument("--published-at", required=True)
    release.add_argument("--artifacts", type=Path, required=True)
    release.add_argument("--sha256", type=Path, required=True)
    release.add_argument("--blake3", type=Path, required=True)
    release.add_argument("--output", type=Path, required=True)
    release.set_defaults(run=render_release)

    validate_release_command = commands.add_parser("validate-release")
    validate_release_command.add_argument("path", type=Path)
    validate_release_command.add_argument("--require-ready", action="store_true")
    validate_release_command.set_defaults(
        run=lambda args: validate_release(load(args.path), require_ready=args.require_ready)
    )

    channel = commands.add_parser("render-channel")
    channel.add_argument("--channel", choices=CHANNELS, required=True)
    channel.add_argument("--generation", type=int, required=True)
    channel.add_argument("--published-at", required=True)
    channel.add_argument("--expires-at", required=True)
    channel.add_argument("--release-metadata", type=Path, required=True)
    channel.add_argument("--release-metadata-sha256")
    channel.add_argument("--require-ready", action="store_true")
    channel.add_argument("--output", type=Path, required=True)
    channel.set_defaults(run=render_channel)

    validate_channel_command = commands.add_parser("validate-channel")
    validate_channel_command.add_argument("path", type=Path)
    validate_channel_command.add_argument("--channel", choices=CHANNELS)
    validate_channel_command.add_argument("--minimum-generation", type=int)
    validate_channel_command.add_argument("--now")

    def run_validate_channel(args: argparse.Namespace) -> None:
        now = timestamp(args.now, "--now") if args.now else None
        validate_channel(
            load(args.path),
            expected_channel=args.channel,
            minimum_generation=args.minimum_generation,
            now=now,
        )

    validate_channel_command.set_defaults(run=run_validate_channel)

    validate_channel_release_command = commands.add_parser("validate-channel-release")
    validate_channel_release_command.add_argument("channel_path", type=Path)
    validate_channel_release_command.add_argument("release_path", type=Path)
    validate_channel_release_command.add_argument("--channel", choices=CHANNELS)
    validate_channel_release_command.add_argument("--generation", type=int)
    validate_channel_release_command.add_argument("--minimum-generation", type=int)
    validate_channel_release_command.add_argument("--now")
    validate_channel_release_command.add_argument("--require-ready", action="store_true")

    def run_validate_channel_release(args: argparse.Namespace) -> None:
        now = timestamp(args.now, "--now") if args.now else None
        validate_channel_release(
            load(args.channel_path),
            load(args.release_path),
            args.release_path.read_bytes(),
            expected_channel=args.channel,
            expected_generation=args.generation,
            minimum_generation=args.minimum_generation,
            now=now,
            require_ready=args.require_ready,
        )

    validate_channel_release_command.set_defaults(run=run_validate_channel_release)
    return root


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    args.run(args)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (KeyError, OSError, tomllib.TOMLDecodeError, ValueError) as error:
        print(f"release metadata: {error}", file=sys.stderr)
        raise SystemExit(1)
