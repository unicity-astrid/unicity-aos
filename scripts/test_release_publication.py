#!/usr/bin/env python3

from __future__ import annotations

import argparse
import hashlib
import io
import shutil
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import capsule_release
import musl_release_metadata
import release_metadata
import release_publication


VERSION = "2026.1.3"
SOURCE_COMMIT = "a" * 40


def add_file(archive: tarfile.TarFile, name: str, value: bytes) -> None:
    member = tarfile.TarInfo(name)
    member.size = len(value)
    member.mtime = 0
    member.uid = 0
    member.gid = 0
    archive.addfile(member, io.BytesIO(value))


class ReleasePublicationTests(unittest.TestCase):
    def fixture(self, root: Path, *, musl: bool = False) -> tuple[Path, Path, Path]:
        artifacts = root / "artifacts"
        artifacts.mkdir()
        specs = capsule_release.source_contract()
        for spec in specs:
            with tarfile.open(artifacts / spec.asset, "w:gz") as archive:
                add_file(archive, "Capsule.toml", spec.manifest.read_bytes())
                for component in spec.components:
                    add_file(archive, component, b"\0asm")

        release_targets = list(release_metadata.TARGETS)
        if musl:
            release_targets.extend(musl_release_metadata.MUSL_TARGETS)
        for target in release_targets:
            (artifacts / f"unicity-aos-{VERSION}-{target}.tar.gz").write_bytes(
                f"archive:{target}".encode()
            )

        checksummed = sorted(
            path.name
            for path in artifacts.iterdir()
            if path.name.endswith((".tar.gz", ".capsule"))
        )
        sha_lines = []
        blake_lines = []
        for name in checksummed:
            value = (artifacts / name).read_bytes()
            sha_lines.append(f"{hashlib.sha256(value).hexdigest()}  {name}")
            blake_lines.append(f"{hashlib.sha256(b'blake3:' + value).hexdigest()}  {name}")
        (artifacts / "SHA256SUMS.txt").write_text("\n".join(sha_lines) + "\n")
        (artifacts / "BLAKE3SUMS.txt").write_text("\n".join(blake_lines) + "\n")

        compatibility = root / "runtime-compatibility.toml"
        compatibility.write_text(
            (release_publication.ROOT / "release" / "runtime-compatibility.toml")
            .read_text()
            .replace("release-ready = false", "release-ready = true")
            .replace("upgrade-self-heal-ready = false", "upgrade-self-heal-ready = true")
        )
        shutil.copyfile(compatibility, artifacts / "runtime-compatibility.toml")

        metadata = artifacts / f"unicity-aos-{VERSION}-release.toml"
        args = argparse.Namespace(
            version=VERSION,
            tag=VERSION,
            source_commit=SOURCE_COMMIT,
            published_at="2026-07-16T00:00:00Z",
            artifacts=artifacts,
            sha256=artifacts / "SHA256SUMS.txt",
            blake3=artifacts / "BLAKE3SUMS.txt",
            output=metadata,
        )
        release_metadata.render_release(args)
        metadata.write_text(
            metadata.read_text()
            .replace("release-ready = false", "release-ready = true")
            .replace("upgrade-self-heal-ready = false", "upgrade-self-heal-ready = true")
        )

        musl_compatibility = (
            release_publication.ROOT
            / "release"
            / "runtime-musl-compatibility.toml"
        )
        if musl:
            musl_compatibility = root / "runtime-musl-compatibility.toml"
            musl_compatibility.write_text(
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
                )
            )
            shutil.copyfile(
                musl_compatibility,
                artifacts / "runtime-musl-compatibility.toml",
            )
            extension = (
                artifacts / musl_release_metadata.metadata_name(VERSION)
            )
            extension.write_text(
                musl_release_metadata.render_extension(
                    musl_release_metadata.build_extension(
                        artifacts=artifacts,
                        legacy_path=metadata,
                        compatibility_path=musl_compatibility,
                    )
                )
            )

        payloads = [
            *checksummed,
            "BLAKE3SUMS.txt",
            "SHA256SUMS.txt",
            "runtime-compatibility.toml",
            metadata.name,
        ]
        if musl:
            payloads.extend(
                [
                    "runtime-musl-compatibility.toml",
                    musl_release_metadata.metadata_name(VERSION),
                ]
            )
        for name in payloads:
            (artifacts / f"{name}.sigstore.json").write_text("{}\n")
        return artifacts, compatibility, musl_compatibility

    def validate(
        self,
        artifacts: Path,
        compatibility: Path,
        musl_compatibility: Path | None = None,
    ) -> list[str]:
        return release_publication.validate_release_assets(
            artifacts,
            version=VERSION,
            source_commit=SOURCE_COMMIT,
            compatibility_path=compatibility,
            musl_compatibility_path=musl_compatibility,
        )

    def test_accepts_complete_authenticated_inventory(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            artifacts, compatibility, musl_compatibility = self.fixture(Path(temp))
            payloads = self.validate(artifacts, compatibility, musl_compatibility)
            self.assertIn(f"unicity-aos-{VERSION}-release.toml", payloads)
            self.assertEqual(len([name for name in payloads if name.endswith(".capsule")]), 21)

    def test_rejects_missing_asset(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            artifacts, compatibility, musl_compatibility = self.fixture(Path(temp))
            next(artifacts.glob("*.capsule.sigstore.json")).unlink()
            with self.assertRaisesRegex(ValueError, "asset set differs"):
                self.validate(artifacts, compatibility, musl_compatibility)

    def test_rejects_unexpected_asset(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            artifacts, compatibility, musl_compatibility = self.fixture(Path(temp))
            (artifacts / "unexpected").write_text("no")
            with self.assertRaisesRegex(ValueError, "asset set differs"):
                self.validate(artifacts, compatibility, musl_compatibility)

    def test_false_musl_gate_rejects_extension_assets(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            artifacts, compatibility, musl_compatibility = self.fixture(Path(temp))
            name = f"unicity-aos-{VERSION}-x86_64-unknown-linux-musl.tar.gz"
            (artifacts / name).write_bytes(b"not enabled")
            (artifacts / f"{name}.sigstore.json").write_text("{}\n")
            with self.assertRaisesRegex(ValueError, "unexpected"):
                self.validate(artifacts, compatibility, musl_compatibility)

    def test_rejects_changed_payload(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            artifacts, compatibility, musl_compatibility = self.fixture(Path(temp))
            next(artifacts.glob("*.tar.gz")).write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
                self.validate(artifacts, compatibility, musl_compatibility)

    def test_rejects_wrong_source_commit(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            artifacts, compatibility, musl_compatibility = self.fixture(Path(temp))
            with self.assertRaisesRegex(ValueError, "source commit"):
                release_publication.validate_release_assets(
                    artifacts,
                    version=VERSION,
                    source_commit="b" * 40,
                    compatibility_path=compatibility,
                    musl_compatibility_path=musl_compatibility,
                )

    def test_rejects_compatibility_drift(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            artifacts, compatibility, musl_compatibility = self.fixture(Path(temp))
            compatibility.write_text(compatibility.read_text() + "\n# drift\n")
            with self.assertRaisesRegex(ValueError, "tagged source"):
                self.validate(artifacts, compatibility, musl_compatibility)

    def test_accepts_complete_authenticated_musl_union(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            artifacts, compatibility, musl_compatibility = self.fixture(
                Path(temp), musl=True
            )
            payloads = self.validate(
                artifacts, compatibility, musl_compatibility
            )
            self.assertIn(
                musl_release_metadata.metadata_name(VERSION), payloads
            )
            self.assertEqual(
                len(
                    [
                        name
                        for name in payloads
                        if name.endswith("-unknown-linux-musl.tar.gz")
                    ]
                ),
                2,
            )

    def test_ready_musl_rejects_missing_or_partial_inventory(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            artifacts, compatibility, musl_compatibility = self.fixture(
                Path(temp), musl=True
            )
            (
                artifacts
                / f"{musl_release_metadata.metadata_name(VERSION)}.sigstore.json"
            ).unlink()
            with self.assertRaisesRegex(ValueError, "asset set differs"):
                self.validate(artifacts, compatibility, musl_compatibility)

        with tempfile.TemporaryDirectory() as temp:
            artifacts, compatibility, musl_compatibility = self.fixture(
                Path(temp), musl=True
            )
            target = f"unicity-aos-{VERSION}-aarch64-unknown-linux-musl.tar.gz"
            (artifacts / target).unlink()
            (artifacts / f"{target}.sigstore.json").unlink()
            with self.assertRaisesRegex(ValueError, "asset set differs"):
                self.validate(artifacts, compatibility, musl_compatibility)

    def test_ready_musl_rejects_unexpected_target(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            artifacts, compatibility, musl_compatibility = self.fixture(
                Path(temp), musl=True
            )
            name = f"unicity-aos-{VERSION}-riscv64-unknown-linux-musl.tar.gz"
            (artifacts / name).write_bytes(b"unexpected")
            (artifacts / f"{name}.sigstore.json").write_text("{}\n")
            with self.assertRaisesRegex(ValueError, "unexpected"):
                self.validate(artifacts, compatibility, musl_compatibility)

    def test_ready_musl_rejects_partial_checksum_union(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            artifacts, compatibility, musl_compatibility = self.fixture(
                Path(temp), musl=True
            )
            checksum = artifacts / "SHA256SUMS.txt"
            lines = checksum.read_text().splitlines()
            checksum.write_text(
                "\n".join(
                    line
                    for line in lines
                    if "aarch64-unknown-linux-musl" not in line
                )
                + "\n"
            )
            with self.assertRaisesRegex(ValueError, "exact payload set"):
                self.validate(artifacts, compatibility, musl_compatibility)

    def test_ready_musl_rejects_tampered_archive_or_extension_binding(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            artifacts, compatibility, musl_compatibility = self.fixture(
                Path(temp), musl=True
            )
            (
                artifacts
                / f"unicity-aos-{VERSION}-x86_64-unknown-linux-musl.tar.gz"
            ).write_bytes(b"tampered")
            with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
                self.validate(artifacts, compatibility, musl_compatibility)

        with tempfile.TemporaryDirectory() as temp:
            artifacts, compatibility, musl_compatibility = self.fixture(
                Path(temp), musl=True
            )
            extension = (
                artifacts / musl_release_metadata.metadata_name(VERSION)
            )
            extension.write_text(
                extension.read_text().replace(
                    "metadata-sha256 = \"",
                    f'metadata-sha256 = "{"0" * 64}" # ',
                    1,
                )
            )
            with self.assertRaisesRegex(ValueError, "bind"):
                self.validate(artifacts, compatibility, musl_compatibility)


if __name__ == "__main__":
    unittest.main()
