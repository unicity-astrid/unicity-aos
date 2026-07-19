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
import release_metadata
import release_publication


VERSION = "2026.1.2"
SOURCE_COMMIT = "a" * 40


def add_file(archive: tarfile.TarFile, name: str, value: bytes) -> None:
    member = tarfile.TarInfo(name)
    member.size = len(value)
    member.mtime = 0
    member.uid = 0
    member.gid = 0
    archive.addfile(member, io.BytesIO(value))


class ReleasePublicationTests(unittest.TestCase):
    def fixture(self, root: Path) -> tuple[Path, Path]:
        artifacts = root / "artifacts"
        artifacts.mkdir()
        specs = capsule_release.source_contract()
        for spec in specs:
            with tarfile.open(artifacts / spec.asset, "w:gz") as archive:
                add_file(archive, "Capsule.toml", spec.manifest.read_bytes())
                for component in spec.components:
                    add_file(archive, component, b"\0asm")
                for skill in spec.skills:
                    add_file(archive, skill, (spec.manifest.parent / skill).read_bytes())

        for target in release_metadata.TARGETS:
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

        payloads = [
            *checksummed,
            "BLAKE3SUMS.txt",
            "SHA256SUMS.txt",
            "runtime-compatibility.toml",
            metadata.name,
        ]
        for name in payloads:
            (artifacts / f"{name}.sigstore.json").write_text("{}\n")
        return artifacts, compatibility

    def validate(self, artifacts: Path, compatibility: Path) -> list[str]:
        return release_publication.validate_release_assets(
            artifacts,
            version=VERSION,
            source_commit=SOURCE_COMMIT,
            compatibility_path=compatibility,
        )

    def test_accepts_complete_authenticated_inventory(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            artifacts, compatibility = self.fixture(Path(temp))
            payloads = self.validate(artifacts, compatibility)
            self.assertIn(f"unicity-aos-{VERSION}-release.toml", payloads)
            self.assertEqual(len([name for name in payloads if name.endswith(".capsule")]), 19)

    def test_rejects_missing_asset(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            artifacts, compatibility = self.fixture(Path(temp))
            next(artifacts.glob("*.capsule.sigstore.json")).unlink()
            with self.assertRaisesRegex(ValueError, "asset set differs"):
                self.validate(artifacts, compatibility)

    def test_rejects_unexpected_asset(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            artifacts, compatibility = self.fixture(Path(temp))
            (artifacts / "unexpected").write_text("no")
            with self.assertRaisesRegex(ValueError, "asset set differs"):
                self.validate(artifacts, compatibility)

    def test_rejects_changed_payload(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            artifacts, compatibility = self.fixture(Path(temp))
            next(artifacts.glob("*.tar.gz")).write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
                self.validate(artifacts, compatibility)

    def test_rejects_wrong_source_commit(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            artifacts, compatibility = self.fixture(Path(temp))
            with self.assertRaisesRegex(ValueError, "source commit"):
                release_publication.validate_release_assets(
                    artifacts,
                    version=VERSION,
                    source_commit="b" * 40,
                    compatibility_path=compatibility,
                )

    def test_rejects_compatibility_drift(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            artifacts, compatibility = self.fixture(Path(temp))
            compatibility.write_text(compatibility.read_text() + "\n# drift\n")
            with self.assertRaisesRegex(ValueError, "tagged source"):
                self.validate(artifacts, compatibility)


if __name__ == "__main__":
    unittest.main()
