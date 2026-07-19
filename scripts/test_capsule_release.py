#!/usr/bin/env python3

from __future__ import annotations

import io
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from typing import Optional

sys.path.insert(0, str(Path(__file__).resolve().parent))

from capsule_release import ContractError, CapsuleSpec, source_contract, validate_artifacts


def add_bytes(
    archive: tarfile.TarFile,
    name: str,
    data: bytes,
    *,
    kind: Optional[bytes] = None,
) -> None:
    member = tarfile.TarInfo(name)
    member.size = len(data)
    member.mtime = 0
    member.uid = 0
    member.gid = 0
    if kind is not None:
        member.type = kind
        member.size = 0
    archive.addfile(member, io.BytesIO(data) if member.isfile() else None)


def write_fixture(path: Path, spec: CapsuleSpec, *, mutation: Optional[str] = None) -> None:
    manifest = spec.manifest.read_bytes()
    with tarfile.open(path, mode="w:gz") as archive:
        if mutation == "traversal":
            add_bytes(archive, "../escape", b"bad")
        if mutation == "duplicate-manifest":
            add_bytes(archive, "Capsule.toml", manifest)
        if mutation == "dot-alias":
            add_bytes(archive, "./Capsule.toml", b'[package]\nname = "wrong-package"\nversion = "0.0.0"\n')
        if mutation == "case-alias":
            add_bytes(archive, "capsule.toml", b"bad")
        if mutation == "symlink":
            link = tarfile.TarInfo("outside")
            link.type = tarfile.SYMTYPE
            link.linkname = "/tmp"
            archive.addfile(link)
        if mutation == "hardlink":
            link = tarfile.TarInfo("outside")
            link.type = tarfile.LNKTYPE
            link.linkname = "Capsule.toml"
            archive.addfile(link)
        if mutation == "device":
            device = tarfile.TarInfo("device")
            device.type = tarfile.CHRTYPE
            archive.addfile(device)
        if mutation == "unexpected-member":
            add_bytes(archive, "unexpected.txt", b"not allowed")
        if mutation == "wrong-manifest":
            manifest = manifest.replace(
                f'name = "{spec.package}"'.encode(),
                b'name = "wrong-package"',
                1,
            )
        if mutation == "changed-capability":
            manifest = manifest.replace(b"uplink = true", b"uplink = false", 1)
        add_bytes(archive, "Capsule.toml", manifest)
        for component in spec.components:
            if mutation == "missing-component" and component == spec.components[0]:
                continue
            add_bytes(archive, component, b"\x00asm")
        for skill in spec.skills:
            if mutation == "missing-skill" and skill == spec.skills[0]:
                continue
            add_bytes(
                archive,
                skill,
                b"---\nname: fixture\ndescription: Release contract fixture\n---\n",
            )


class CapsuleReleaseTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.specs = source_contract()

    def fixture_set(self, directory: Path) -> None:
        for spec in self.specs:
            write_fixture(directory / spec.asset, spec)

    def test_source_contract_has_exact_community_set(self) -> None:
        self.assertEqual(len(self.specs), 19)
        self.assertEqual(len({spec.asset for spec in self.specs}), 19)
        assets = {spec.asset for spec in self.specs}
        self.assertIn("aos-forge.capsule", assets)
        self.assertNotIn("aos-telegram.capsule", assets)
        distro = Path(__file__).resolve().parent.parent / "distros/community/unicity-ce/Distro.toml"
        text = distro.read_text(encoding="utf-8")
        self.assertNotIn("@unicity-aos/", text)
        self.assertEqual(text.count('source = "capsules/'), 19)
        for spec in self.specs:
            self.assertIn(f'source = "capsules/{spec.asset}"', text)

    def test_accepts_exact_safe_artifact_set(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            self.fixture_set(directory)
            validate_artifacts(directory, self.specs)

    def assert_mutation_rejected(self, mutation: str) -> None:
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            self.fixture_set(directory)
            write_fixture(directory / self.specs[0].asset, self.specs[0], mutation=mutation)
            with self.assertRaises(ContractError):
                validate_artifacts(directory, self.specs)

    def test_rejects_traversal(self) -> None:
        self.assert_mutation_rejected("traversal")

    def test_rejects_exact_duplicate(self) -> None:
        self.assert_mutation_rejected("duplicate-manifest")

    def test_rejects_dot_alias(self) -> None:
        self.assert_mutation_rejected("dot-alias")

    def test_rejects_case_alias(self) -> None:
        self.assert_mutation_rejected("case-alias")

    def test_rejects_symlink(self) -> None:
        self.assert_mutation_rejected("symlink")

    def test_rejects_hardlink(self) -> None:
        self.assert_mutation_rejected("hardlink")

    def test_rejects_device(self) -> None:
        self.assert_mutation_rejected("device")

    def test_rejects_unexpected_member(self) -> None:
        self.assert_mutation_rejected("unexpected-member")

    def test_rejects_wrong_embedded_identity(self) -> None:
        self.assert_mutation_rejected("wrong-manifest")

    def test_rejects_changed_capabilities(self) -> None:
        self.assert_mutation_rejected("changed-capability")

    def test_rejects_missing_component(self) -> None:
        self.assert_mutation_rejected("missing-component")

    def test_accepts_declared_skill_assets(self) -> None:
        skill_specs = [spec for spec in self.specs if spec.skills]
        self.assertTrue(skill_specs)
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            self.fixture_set(directory)
            validate_artifacts(directory, self.specs)

    def test_rejects_missing_declared_skill_asset(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            self.fixture_set(directory)
            spec = next(spec for spec in self.specs if spec.skills)
            write_fixture(directory / spec.asset, spec, mutation="missing-skill")
            with self.assertRaises(ContractError):
                validate_artifacts(directory, self.specs)

    def test_rejects_unexpected_asset(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            self.fixture_set(directory)
            (directory / "unexpected.capsule").write_bytes(b"no")
            with self.assertRaises(ContractError):
                validate_artifacts(directory, self.specs)

    def test_rejects_unexpected_directory(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            self.fixture_set(directory)
            (directory / "unexpected").mkdir()
            with self.assertRaises(ContractError):
                validate_artifacts(directory, self.specs)

    def test_rejects_symlink_asset(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            self.fixture_set(directory)
            target = directory / self.specs[0].asset
            target.unlink()
            target.symlink_to(directory / self.specs[1].asset)
            with self.assertRaises(ContractError):
                validate_artifacts(directory, self.specs)


if __name__ == "__main__":
    unittest.main()
