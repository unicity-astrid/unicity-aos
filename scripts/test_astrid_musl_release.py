#!/usr/bin/env python3

from __future__ import annotations

import copy
import hashlib
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import astrid_musl_release


VERSION = "0.11.0"
SOURCE = "a" * 40
CONTRACTS = "b" * 40
IDENTITY = (
    "https://github.com/astrid-runtime/astrid/.github/workflows/"
    f"release.yml@refs/tags/v{VERSION}"
)


def fake_blake3(path: Path) -> str:
    return hashlib.sha256(b"blake3:" + path.read_bytes()).hexdigest()


def target_entry(target: str, payload: bytes) -> dict[str, object]:
    asset = f"astrid-{VERSION}-{target}.tar.gz"
    return {
        "triple": target,
        "asset": asset,
        "size": len(payload),
        "blake3": hashlib.sha256(b"blake3:" + payload).hexdigest(),
        "sha256": hashlib.sha256(payload).hexdigest(),
        "sigstore-bundle": f"{asset}.sigstore.json",
    }


def render(value: dict[str, object]) -> str:
    def quote(item: object) -> str:
        return f'"{item}"'

    lines = [
        f"schema-version = {value['schema-version']}",
        f"kind = {quote(value['kind'])}",
        f"product = {quote(value['product'])}",
        f"repository = {quote(value['repository'])}",
        f"version = {quote(value['version'])}",
        f"tag = {quote(value['tag'])}",
        f"source-commit = {quote(value['source-commit'])}",
        f"release-workflow-identity = {quote(value['release-workflow-identity'])}",
    ]
    if "surprise" in value:
        lines.append(f"surprise = {quote(value['surprise'])}")
    if "contracts" in value:
        lines.extend(
            [
                "",
                "[contracts]",
                f"repository = {quote(value['contracts']['repository'])}",
                f"commit = {quote(value['contracts']['commit'])}",
            ]
        )
    if "legacy-release" in value:
        lines.extend(
            [
                "",
                "[legacy-release]",
                f"metadata-asset = {quote(value['legacy-release']['metadata-asset'])}",
                f"metadata-blake3 = {quote(value['legacy-release']['metadata-blake3'])}",
            ]
        )
    for entry in value["targets"]:
        lines.extend(
            [
                "",
                "[[targets]]",
                f"triple = {quote(entry['triple'])}",
                f"asset = {quote(entry['asset'])}",
                f"size = {entry['size']}",
                f"blake3 = {quote(entry['blake3'])}",
                f"sha256 = {quote(entry['sha256'])}",
                f"sigstore-bundle = {quote(entry['sigstore-bundle'])}",
            ]
        )
    return "\n".join(lines) + "\n"


class AstridMuslReleaseTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.target = "x86_64-unknown-linux-musl"
        self.archive = self.root / f"astrid-{VERSION}-{self.target}.tar.gz"
        self.archive.write_bytes(b"static archive")
        payloads = {
            target: (
                self.archive.read_bytes()
                if target == self.target
                else f"archive:{target}".encode()
            )
            for target in (
                *astrid_musl_release.LEGACY_TARGETS,
                "aarch64-unknown-linux-musl",
                self.target,
            )
        }
        base = {
            "schema-version": 1,
            "product": "astrid-runtime",
            "repository": "astrid-runtime/astrid",
            "version": VERSION,
            "tag": f"v{VERSION}",
            "source-commit": SOURCE,
            "release-workflow-identity": IDENTITY,
        }
        self.legacy = {
            **base,
            "kind": "astrid-release",
            "contracts": {
                "repository": "astrid-runtime/wit",
                "commit": CONTRACTS,
            },
            "targets": [
                target_entry(target, payloads[target])
                for target in astrid_musl_release.LEGACY_TARGETS
            ],
        }
        self.legacy_path = self.root / f"astrid-{VERSION}-release.toml"
        self.legacy_path.write_text(render(self.legacy), encoding="utf-8")
        self.extension = {
            **base,
            "kind": "astrid-release-musl-extension",
            "legacy-release": {
                "metadata-asset": self.legacy_path.name,
                "metadata-blake3": fake_blake3(self.legacy_path),
            },
            "targets": [
                target_entry(target, payloads[target])
                for target in (
                    "aarch64-unknown-linux-musl",
                    "x86_64-unknown-linux-musl",
                )
            ],
        }
        self.extension_path = self.root / f"astrid-{VERSION}-musl-release.toml"
        self.extension_path.write_text(render(self.extension), encoding="utf-8")
        self.compatibility_path = self.root / "runtime-musl.toml"
        self.write_compatibility()

    def write_compatibility(self, *, ready: bool = True) -> None:
        self.compatibility_path.write_text(
            "\n".join(
                [
                    "schema-version = 1",
                    "",
                    "[runtime]",
                    'repository = "astrid-runtime/astrid"',
                    f"release-ready = {str(ready).lower()}",
                    f'version = "{VERSION}"',
                    f'tag = "v{VERSION}"',
                    f'release-workflow-identity = "{IDENTITY}"',
                    f'source-commit = "{SOURCE}"',
                    f'legacy-release-metadata-asset = "{self.legacy_path.name}"',
                    f'legacy-release-metadata-blake3 = "{fake_blake3(self.legacy_path)}"',
                    (
                        f'musl-release-metadata-asset = "{self.extension_path.name}"'
                        if ready
                        else 'musl-release-metadata-asset = ""'
                    ),
                    (
                        f'musl-release-metadata-blake3 = "{fake_blake3(self.extension_path)}"'
                        if ready
                        else 'musl-release-metadata-blake3 = ""'
                    ),
                    "",
                ]
            ),
            encoding="utf-8",
        )

    def validate(self) -> dict[str, object]:
        with mock.patch.object(
            astrid_musl_release, "blake3_file", side_effect=fake_blake3
        ):
            return astrid_musl_release.validate_release(
                compatibility_path=self.compatibility_path,
                legacy_path=self.legacy_path,
                extension_path=self.extension_path,
                target=self.target,
                archive_path=self.archive,
            )

    def rewrite_extension(self, value: dict[str, object]) -> None:
        self.extension_path.write_text(render(value), encoding="utf-8")
        self.write_compatibility()

    def test_accepts_the_exact_pinned_release_and_archive(self) -> None:
        selected = self.validate()
        self.assertEqual(selected["triple"], self.target)

    def test_false_release_gate_blocks_the_release_path(self) -> None:
        self.write_compatibility(ready=False)
        with self.assertRaisesRegex(ValueError, "gate is false"):
            self.validate()

    def test_rejects_extension_not_bound_to_legacy_bytes(self) -> None:
        changed = copy.deepcopy(self.extension)
        changed["legacy-release"]["metadata-blake3"] = "f" * 64
        self.rewrite_extension(changed)
        with self.assertRaisesRegex(ValueError, "bind"):
            self.validate()

    def test_rejects_identity_and_target_set_changes(self) -> None:
        changed = copy.deepcopy(self.extension)
        changed["source-commit"] = "c" * 40
        self.rewrite_extension(changed)
        with self.assertRaisesRegex(ValueError, "source commit"):
            self.validate()

        changed = copy.deepcopy(self.extension)
        changed["targets"][0] = copy.deepcopy(changed["targets"][1])
        self.rewrite_extension(changed)
        with self.assertRaisesRegex(ValueError, "target set"):
            self.validate()

    def test_rejects_archive_size_and_digest_changes(self) -> None:
        self.archive.write_bytes(b"tampered")
        with self.assertRaisesRegex(ValueError, "size|SHA-256|BLAKE3"):
            self.validate()

    def test_rejects_unknown_schema_fields(self) -> None:
        changed = copy.deepcopy(self.extension)
        changed["surprise"] = "value"
        self.rewrite_extension(changed)
        with self.assertRaisesRegex(ValueError, "unknown"):
            self.validate()


if __name__ == "__main__":
    unittest.main()
