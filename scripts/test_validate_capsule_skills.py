import io
import pathlib
import sys
import tarfile
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

import validate_capsule_skills


MANIFEST = b"""\
[package]
name = "test"
version = "0.1.0"

[[skill]]
name = "test-skill"
file = "skills/test-skill/SKILL.md"
"""


def _write_capsule(path: pathlib.Path, *, include_skill: bool = True) -> None:
    with tarfile.open(path, "w:gz") as archive:
        manifest = tarfile.TarInfo("Capsule.toml")
        manifest.size = len(MANIFEST)
        archive.addfile(manifest, io.BytesIO(MANIFEST))
        if include_skill:
            content = b"---\nname: test-skill\ndescription: Test\n---\n"
            skill = tarfile.TarInfo("skills/test-skill/SKILL.md")
            skill.size = len(content)
            archive.addfile(skill, io.BytesIO(content))


class ValidateCapsuleSkillsTests(unittest.TestCase):
    def test_accepts_packaged_declared_skill(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            capsule = pathlib.Path(directory, "test.capsule")
            _write_capsule(capsule)
            validate_capsule_skills.validate_capsule(capsule)

    def test_rejects_missing_declared_skill(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            capsule = pathlib.Path(directory, "test.capsule")
            _write_capsule(capsule, include_skill=False)
            with self.assertRaisesRegex(ValueError, "asset is absent"):
                validate_capsule_skills.validate_capsule(capsule)

    def test_rejects_unsafe_declared_skill_path(self) -> None:
        unsafe = MANIFEST.replace(
            b"skills/test-skill/SKILL.md", b"../outside/SKILL.md"
        )
        with tempfile.TemporaryDirectory() as directory:
            capsule = pathlib.Path(directory, "test.capsule")
            with tarfile.open(capsule, "w:gz") as archive:
                manifest = tarfile.TarInfo("Capsule.toml")
                manifest.size = len(unsafe)
                archive.addfile(manifest, io.BytesIO(unsafe))
            with self.assertRaisesRegex(ValueError, "unsafe file path"):
                validate_capsule_skills.validate_capsule(capsule)


if __name__ == "__main__":
    unittest.main()
