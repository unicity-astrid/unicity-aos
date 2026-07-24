#!/usr/bin/env python3

import pathlib
import subprocess
import tempfile
import unittest
from unittest import mock

import check_glibc


class GlibcCompatibilityTests(unittest.TestCase):
    def test_extracts_unique_required_versions(self) -> None:
        versions = check_glibc.required_versions(
            """
            0x0010: Name: GLIBC_2.17  Flags: none
            0x0020: Name: GLIBC_2.34  Flags: none
            0x0030: Name: GLIBC_2.17  Flags: none
            """
        )
        self.assertEqual(versions, {(2, 17), (2, 34)})

    def test_ignores_similar_non_glibc_symbols(self) -> None:
        self.assertEqual(
            check_glibc.required_versions("GLIBCXX_3.4 CXXABI_1.3"),
            set(),
        )

    def test_rejects_malformed_ceiling(self) -> None:
        for value in ("2", "2.x", "v2.34"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                check_glibc.parse_version(value)

    def test_check_binary_enforces_ceiling_and_reports_readelf_errors(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            binary = pathlib.Path(directory) / "aos"
            binary.write_bytes(b"ELF fixture")
            with mock.patch.object(
                check_glibc.subprocess,
                "run",
                return_value=subprocess.CompletedProcess(
                    args=[],
                    returncode=0,
                    stdout="Name: GLIBC_2.30",
                    stderr="",
                ),
            ):
                check_glibc.check_binary(binary, (2, 34))

            with mock.patch.object(
                check_glibc.subprocess,
                "run",
                return_value=subprocess.CompletedProcess(
                    args=[],
                    returncode=0,
                    stdout="Name: GLIBC_2.39",
                    stderr="",
                ),
            ), self.assertRaisesRegex(ValueError, "newer than GLIBC_2.34"):
                check_glibc.check_binary(binary, (2, 34))

            failure = subprocess.CalledProcessError(
                1,
                ["readelf"],
                stderr="invalid ELF",
            )
            with mock.patch.object(
                check_glibc.subprocess,
                "run",
                side_effect=failure,
            ), self.assertRaisesRegex(ValueError, "invalid ELF"):
                check_glibc.check_binary(binary, (2, 34))


if __name__ == "__main__":
    unittest.main()
