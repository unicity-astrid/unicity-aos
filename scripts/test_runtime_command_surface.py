#!/usr/bin/env python3
"""Regression tests for the pinned runtime root-command contract."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("validate-runtime-command-surface.py")
SPEC = importlib.util.spec_from_file_location("runtime_command_surface", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("could not load runtime command surface validator")
VALIDATOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VALIDATOR)


class RuntimeCommandSurfaceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary_directory.cleanup)
        self.contract_path = Path(self.temporary_directory.name) / "surface.toml"

    def write_contract(self, extra: str = "") -> Path:
        self.contract_path.write_text(
            'schema-version = 1\nruntime-version = "0.10.4"\n'
            "[roots]\n"
            'inherited = ["agent"]\n'
            'product-owned = ["mcp"]\n'
            'shared = ["help"]\n'
            f'hidden-inherited = ["build"]\n{extra}',
            encoding="utf-8",
        )
        return self.contract_path

    def test_help_parser_ignores_wrapped_descriptions(self) -> None:
        parsed = VALIDATOR.parse_help(
            "Commands:\n"
            "  agent  Manage agents with a description\n"
            "         that wraps onto another line\n"
            "  help   Print help\n\n"
            "Options:\n  -h, --help  Print help\n"
        )
        self.assertEqual(parsed, {"agent", "help"})

    def test_new_runtime_root_fails_until_classified(self) -> None:
        contract = VALIDATOR.load_contract(self.write_contract(), "0.10.4")
        with self.assertRaisesRegex(VALIDATOR.SurfaceError, "unclassified.*quota"):
            VALIDATOR.validate({"agent", "help", "mcp", "quota"}, contract)

    def test_removed_runtime_root_is_reported(self) -> None:
        contract = VALIDATOR.load_contract(self.write_contract(), "0.10.4")
        with self.assertRaisesRegex(VALIDATOR.SurfaceError, "missing.*agent"):
            VALIDATOR.validate({"help", "mcp"}, contract)

    def test_contract_is_bound_to_the_runtime_version(self) -> None:
        with self.assertRaisesRegex(VALIDATOR.SurfaceError, "version"):
            VALIDATOR.load_contract(self.write_contract(), "0.10.5")

    def test_roots_cannot_be_classified_twice(self) -> None:
        path = self.write_contract().read_text(encoding="utf-8").replace(
            'product-owned = ["mcp"]', 'product-owned = ["agent"]'
        )
        self.contract_path.write_text(path, encoding="utf-8")
        with self.assertRaisesRegex(VALIDATOR.SurfaceError, "more than once"):
            VALIDATOR.load_contract(self.contract_path, "0.10.4")


if __name__ == "__main__":
    unittest.main()
