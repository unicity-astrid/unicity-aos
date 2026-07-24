#!/usr/bin/env python3
"""Regression tests for the immutable AOS Linux musl metadata extension."""

from __future__ import annotations

import copy
import hashlib
import tempfile
import unittest
from pathlib import Path

import musl_release_metadata
import release_metadata
from test_release_metadata import release_fixture


VERSION = "2026.1.3"


def render_legacy(value: dict[str, object]) -> str:
    lines = [
        "schema-version = 1",
        'kind = "aos-release"',
        'product = "unicity-aos-ce"',
        f'version = "{value["version"]}"',
        f'tag = "{value["tag"]}"',
        f'source-commit = "{value["source-commit"]}"',
        f'published-at = "{value["published-at"]}"',
        f'release-workflow-identity = "{value["release-workflow-identity"]}"',
        "",
        "[runtime]",
    ]
    for key in (
        "repository",
        "version",
        "tag",
        "release-workflow-identity",
    ):
        lines.append(f'{key} = "{value["runtime"][key]}"')
    lines.extend(
        [
            f'release-metadata-available = {str(value["runtime"]["release-metadata-available"]).lower()}',
            f'source-commit = "{value["runtime"]["source-commit"]}"',
            f'release-metadata-asset = "{value["runtime"]["release-metadata-asset"]}"',
            f'release-metadata-blake3 = "{value["runtime"]["release-metadata-blake3"]}"',
            "",
            "[contracts]",
        ]
    )
    for key in ("repository", "commit", "sdk-rust-version", "sdk-rust-commit"):
        lines.append(f'{key} = "{value["contracts"][key]}"')
    lines.extend(
        [
            "",
            "[gates]",
            f'release-ready = {str(value["gates"]["release-ready"]).lower()}',
            f'upgrade-self-heal-ready = {str(value["gates"]["upgrade-self-heal-ready"]).lower()}',
        ]
    )
    release_metadata.write_target_tables(lines, value["targets"])
    return "\n".join(lines) + "\n"


class MuslReleaseMetadataTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.artifacts = self.root / "artifacts"
        self.artifacts.mkdir()
        self.legacy_path = self.artifacts / musl_release_metadata.legacy_metadata_name(VERSION)
        self.legacy_path.write_text(render_legacy(release_fixture()), encoding="utf-8")
        self.compatibility_path = self.root / "runtime-musl.toml"
        self.compatibility_path.write_text(
            "\n".join(
                [
                    "schema-version = 1",
                    "",
                    "[runtime]",
                    'repository = "astrid-runtime/astrid"',
                    "release-ready = true",
                    'version = "0.11.0"',
                    'tag = "v0.11.0"',
                    'release-workflow-identity = "https://github.com/astrid-runtime/astrid/.github/workflows/release.yml@refs/tags/v0.11.0"',
                    f'source-commit = "{"d" * 40}"',
                    'legacy-release-metadata-asset = "astrid-0.11.0-release.toml"',
                    f'legacy-release-metadata-blake3 = "{"e" * 64}"',
                    'musl-release-metadata-asset = "astrid-0.11.0-musl-release.toml"',
                    f'musl-release-metadata-blake3 = "{"f" * 64}"',
                    "",
                ]
            ),
            encoding="utf-8",
        )
        sha_lines = []
        blake_lines = []
        for target in (*release_metadata.TARGETS, *musl_release_metadata.MUSL_TARGETS):
            name = f"unicity-aos-{VERSION}-{target}.tar.gz"
            payload = f"archive:{target}".encode()
            (self.artifacts / name).write_bytes(payload)
            sha_lines.append(f"{hashlib.sha256(payload).hexdigest()}  {name}")
            blake_lines.append(f"{hashlib.sha256(b'blake:' + payload).hexdigest()}  {name}")
        (self.artifacts / "SHA256SUMS.txt").write_text(
            "\n".join(sha_lines) + "\n", encoding="utf-8"
        )
        (self.artifacts / "BLAKE3SUMS.txt").write_text(
            "\n".join(blake_lines) + "\n", encoding="utf-8"
        )

    def extension(self) -> dict[str, object]:
        return musl_release_metadata.build_extension(
            artifacts=self.artifacts,
            legacy_path=self.legacy_path,
            compatibility_path=self.compatibility_path,
        )

    def validate_bound(self, value: dict[str, object]) -> None:
        musl_release_metadata.validate_extension(
            value,
            legacy=release_metadata.load(self.legacy_path),
            legacy_bytes=self.legacy_path.read_bytes(),
        )

    def test_round_trip_binds_exact_two_targets_to_legacy_release(self) -> None:
        value = self.extension()
        rendered = musl_release_metadata.render_extension(value)
        output = self.artifacts / musl_release_metadata.metadata_name(VERSION)
        output.write_text(rendered, encoding="utf-8")
        self.validate_bound(release_metadata.load(output))
        self.assertEqual(
            set(value["targets"]), set(musl_release_metadata.MUSL_TARGETS)
        )
        self.assertEqual(
            value["legacy-release"]["metadata-sha256"],
            hashlib.sha256(self.legacy_path.read_bytes()).hexdigest(),
        )

    def test_rejects_missing_duplicate_or_legacy_target(self) -> None:
        missing = self.extension()
        missing["targets"].pop("aarch64-unknown-linux-musl")
        with self.assertRaisesRegex(ValueError, "missing keys"):
            musl_release_metadata.validate_extension(missing)

        legacy = self.extension()
        legacy["targets"]["x86_64-unknown-linux-gnu"] = copy.deepcopy(
            legacy["targets"]["x86_64-unknown-linux-musl"]
        )
        with self.assertRaisesRegex(ValueError, "unknown keys"):
            musl_release_metadata.validate_extension(legacy)

        duplicate = self.extension()
        duplicate["targets"]["aarch64-unknown-linux-musl"] = copy.deepcopy(
            duplicate["targets"]["x86_64-unknown-linux-musl"]
        )
        with self.assertRaisesRegex(ValueError, "asset must be"):
            musl_release_metadata.validate_extension(duplicate)

    def test_rejects_legacy_release_identity_or_digest_mismatch(self) -> None:
        for key, value in (
            ("version", "2026.1.4"),
            ("tag", "2026.1.4"),
            ("source-commit", "1" * 40),
            (
                "release-workflow-identity",
                "https://github.com/unicity-aos/aos-ce/.github/workflows/release.yml@refs/tags/2026.1.4",
            ),
        ):
            with self.subTest(key=key):
                extension = self.extension()
                extension[key] = value
                with self.assertRaises(ValueError):
                    self.validate_bound(extension)

        extension = self.extension()
        extension["legacy-release"]["metadata-sha256"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "bind"):
            self.validate_bound(extension)

    def test_rejects_runtime_pin_identity_mismatch_and_malformed_digest(self) -> None:
        extension = self.extension()
        extension["runtime-musl"]["tag"] = "v9.9.9"
        with self.assertRaisesRegex(ValueError, "tag/version"):
            musl_release_metadata.validate_extension(extension)

        extension = self.extension()
        extension["runtime-musl"]["musl-release-metadata-blake3"] = "BAD"
        with self.assertRaisesRegex(ValueError, "BLAKE3"):
            musl_release_metadata.validate_extension(extension)

        extension = self.extension()
        extension["runtime-musl"]["surprise"] = "value"
        with self.assertRaisesRegex(ValueError, "unknown keys"):
            musl_release_metadata.validate_extension(extension)

    def test_render_refuses_unready_runtime_pin(self) -> None:
        text = self.compatibility_path.read_text(encoding="utf-8")
        self.compatibility_path.write_text(
            text.replace("release-ready = true", "release-ready = false")
            .replace(
                'musl-release-metadata-asset = "astrid-0.11.0-musl-release.toml"',
                'musl-release-metadata-asset = ""',
            )
            .replace(
                f'musl-release-metadata-blake3 = "{"f" * 64}"',
                'musl-release-metadata-blake3 = ""',
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "release-ready gate is false"):
            self.extension()

    def test_repository_pin_is_staged_and_fail_closed(self) -> None:
        current = release_metadata.load(
            Path(__file__).resolve().parent.parent
            / "release/runtime-musl-compatibility.toml"
        )
        runtime = musl_release_metadata.validate_runtime_pin(
            current, require_ready=False
        )
        self.assertFalse(runtime["release-ready"])
        with self.assertRaisesRegex(ValueError, "release-ready gate is false"):
            musl_release_metadata.validate_runtime_pin(current, require_ready=True)


if __name__ == "__main__":
    unittest.main()
