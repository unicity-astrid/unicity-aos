#!/usr/bin/env python3

from __future__ import annotations

import pathlib
import tempfile
import unittest

import nightly_version


BASE = "2026.1.1"
COMMIT = "0123456789abcdef0123456789abcdef01234567"
NIGHTLY = f"2026.1.1-nightly.20260717.g{COMMIT}"


class NightlyVersionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = pathlib.Path(self.temp.name)
        (self.root / "crates/unicity-aos-bootstrap").mkdir(parents=True)
        (self.root / "release").mkdir()
        (self.root / "distros/community/unicity-ce").mkdir(parents=True)
        (self.root / "crates/unicity-aos-bootstrap/Cargo.toml").write_text(
            '[package]\nname = "unicity-aos-bootstrap"\nversion = "2026.1.1"\n',
            encoding="utf-8",
        )
        (self.root / "release/runtime-compatibility.toml").write_text(
            '[product]\nname = "Unicity AOS Community Edition"\nversion = "2026.1.1"\n\n'
            '[runtime]\nversion = "0.9.4"\nrelease-ready = false\nupgrade-self-heal-ready = false\n',
            encoding="utf-8",
        )
        (self.root / "distros/community/unicity-ce/Distro.toml").write_text(
            '[distro]\npretty-name = "Unicity CE 2026.1.1 (Genesis)"\nversion = "2026.1.1"\nrelease-date = "2026-07-10"\n',
            encoding="utf-8",
        )
        (self.root / "Cargo.lock").write_text(
            'version = 4\n\n[[package]]\nname = "unicity-aos-bootstrap"\nversion = "2026.1.1"\n',
            encoding="utf-8",
        )

    def test_derivation_is_deterministic(self) -> None:
        self.assertEqual(nightly_version.derive(BASE, "20260717", COMMIT), NIGHTLY)
        nightly_version.validate_dispatch_date("20260717", "2026-07-17T23:59:59Z")
        nightly_version.validate_dispatch_date("20260717", "2026-07-18T00:00:01Z")
        with self.assertRaisesRegex(ValueError, "dispatch date"):
            nightly_version.validate_dispatch_date("20991231", "2026-07-17T00:00:00Z")

    def test_stage_updates_only_product_identity(self) -> None:
        compatibility_before = (
            self.root / "release/runtime-compatibility.toml"
        ).read_text()
        nightly_version.stage(self.root, NIGHTLY)
        self.assertIn(f'version = "{NIGHTLY}"', (self.root / "crates/unicity-aos-bootstrap/Cargo.toml").read_text())
        compatibility = (self.root / "release/runtime-compatibility.toml").read_text()
        self.assertEqual(
            compatibility,
            compatibility_before.replace('version = "2026.1.1"', f'version = "{NIGHTLY}"', 1),
        )
        self.assertIn('release-ready = false', compatibility)
        self.assertIn('upgrade-self-heal-ready = false', compatibility)
        distro = (self.root / "distros/community/unicity-ce/Distro.toml").read_text()
        self.assertIn(NIGHTLY, distro)
        self.assertIn('release-date = "2026-07-17"', distro)
        self.assertIn(f'version = "{NIGHTLY}"', (self.root / "Cargo.lock").read_text())

    def test_rejects_noncanonical_inputs(self) -> None:
        for date in ("20260230", "2026717", "2026-07-17"):
            with self.assertRaisesRegex(ValueError, "date"):
                nightly_version.derive(BASE, date, COMMIT)
        with self.assertRaisesRegex(ValueError, "source commit"):
            nightly_version.derive(BASE, "20260717", "A" * 40)
        with self.assertRaisesRegex(ValueError, "nightly version"):
            nightly_version.stage(self.root, "2026.1.1-rc.1")

    def test_rejects_wrong_base_and_ambiguous_files(self) -> None:
        with self.assertRaisesRegex(ValueError, "derive from"):
            nightly_version.stage(self.root, f"2026.2.0-nightly.20260717.g{COMMIT}")
        path = self.root / "release/runtime-compatibility.toml"
        path.write_text(path.read_text() + '\n[product]\nversion = "2026.1.1"\n', encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "exactly once"):
            nightly_version.stage(self.root, NIGHTLY)


if __name__ == "__main__":
    unittest.main()
