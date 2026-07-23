#!/usr/bin/env python3
"""Regression checks for the native Linux musl workflow boundary."""

from __future__ import annotations

import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
AMD64_IMAGE = (
    "docker.io/library/rust@"
    "sha256:e98196986adced5602f6e21c54babdbf2a8700400c7a78868324a3630e0c5d15"
)
ARM64_IMAGE = (
    "docker.io/library/rust@"
    "sha256:594694ee6b07747b63b5c265be2616b62e814180b66227e2c18c6ee85e4136be"
)


class MuslWorkflowContractTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ci = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        self.release = (ROOT / ".github/workflows/release.yml").read_text(
            encoding="utf-8"
        )

    def test_both_native_targets_use_exact_platform_images(self) -> None:
        for workflow in (self.ci, self.release):
            self.assertIn("target: x86_64-unknown-linux-musl", workflow)
            self.assertIn("os: ubuntu-latest", workflow)
            self.assertIn("platform: linux/amd64", workflow)
            self.assertIn(AMD64_IMAGE, workflow)
            self.assertIn("target: aarch64-unknown-linux-musl", workflow)
            self.assertIn("os: ubuntu-24.04-arm", workflow)
            self.assertIn("platform: linux/arm64", workflow)
            self.assertIn(ARM64_IMAGE, workflow)
            self.assertNotIn("setup-qemu", workflow.lower())

    def test_release_authenticates_runtime_metadata_before_packaging(self) -> None:
        legacy = self.release.index('for metadata in "$RUNTIME_LEGACY_METADATA"')
        validator = self.release.index("scripts/astrid_musl_release.py")
        package = self.release.index("scripts/package-release.sh", validator)
        self.assertLess(legacy, validator)
        self.assertLess(validator, package)
        self.assertIn("--use-signed-timestamps", self.release[legacy:validator])

    def test_release_gate_and_signed_install_smoke_are_mandatory(self) -> None:
        self.assertIn(
            "validate-release-contract.py --require-release-ready", self.release
        )
        self.assertIn(
            'test-clean-home-musl-install.sh "$CANDIDATE"', self.release
        )
        self.assertIn("runtime-musl-compatibility.toml", self.release)
        self.assertIn("unicity-aos-*-release.toml", self.release)

    def test_static_validation_covers_aos_and_all_runtime_binaries(self) -> None:
        validation = self.release[
            self.release.index("Validate and exercise every Linux musl binary") :
        ]
        for binary in ("$AOS_BINARY", "astrid", "astrid-daemon", "astrid-build", "astrid-emit"):
            self.assertIn(binary, validation)


if __name__ == "__main__":
    unittest.main()
