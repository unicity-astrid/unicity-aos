#!/usr/bin/env python3
"""Tests for the Linux Realm benchmark orchestrator."""

from __future__ import annotations

import importlib.util
import json
import sys
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("benchmark-linux-realm.py")
SPEC = importlib.util.spec_from_file_location("benchmark_linux_realm", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("could not load Linux Realm benchmark")
BENCHMARK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BENCHMARK)
BASELINE = (
    SCRIPT.parent.parent
    / "benchmarks"
    / "linux-realm"
    / "2026-07-21-m2-ultra-9aa1885.jsonl"
)


class BenchmarkTests(unittest.TestCase):
    def test_committed_baseline_is_complete_and_recomputes(self) -> None:
        records = [json.loads(line) for line in BASELINE.read_text().splitlines()]
        self.assertEqual(records[0]["git_commit"][:7], "9aa1885")
        self.assertEqual(
            len([record for record in records if record["kind"] == "sample"]),
            120,
        )
        recorded = {
            (record["engine"], record["scenario"]): record
            for record in records
            if record["kind"] == "summary"
        }
        recomputed = {
            (record["engine"], record["scenario"]): record
            for record in BENCHMARK.summarize(records)
        }
        self.assertEqual(recorded, recomputed)

    def test_hardware_profile_keeps_performance_fields_only(self) -> None:
        profile = BENCHMARK.parse_hardware_profile(
            "Chip: Apple M2 Ultra\n"
            "Model Identifier: Mac14,14\n"
            "Memory: 192 GB\n"
            "Serial Number (system): secret\n"
        )
        self.assertEqual(
            profile,
            {
                "model": "Apple M2 Ultra",
                "model_identifier": "Mac14,14",
                "memory": "192 GB",
            },
        )

    def test_percentile_interpolates_without_mutating_input(self) -> None:
        values = [40, 10, 30, 20]
        self.assertEqual(BENCHMARK.percentile(values, 0.5), 25)
        self.assertEqual(values, [40, 10, 30, 20])

    def test_summary_groups_engine_and_scenario(self) -> None:
        records = [
            BENCHMARK.sample("a", "1", "boot", 0, 10),
            BENCHMARK.sample("a", "1", "boot", 1, 30),
            BENCHMARK.sample("b", "1", "boot", 0, 100),
            BENCHMARK.skip("docker", "not running"),
        ]
        summaries = BENCHMARK.summarize(records)
        self.assertEqual(len(summaries), 2)
        self.assertEqual(summaries[0]["engine"], "a")
        self.assertEqual(summaries[0]["duration_ns"]["median"], 20)
        self.assertEqual(summaries[1]["duration_ns"]["stdev"], 0)

    def test_process_marker_measures_a_real_child_and_terminates_it(self) -> None:
        duration = BENCHMARK.process_to_marker(
            [
                sys.executable,
                "-c",
                "import sys,time;sys.stdout.write('READY\\n');sys.stdout.flush();time.sleep(10)",
            ],
            b"READY",
            2.0,
        )
        self.assertGreater(duration, 0)
        self.assertLess(duration, 2_000_000_000)

    def test_qemu_command_uses_tcg_and_exact_image(self) -> None:
        command = BENCHMARK.qemu_command("qemu-system-riscv64")
        self.assertIn("tcg,thread=single", command)
        self.assertIn(str(BENCHMARK.IMAGE), command)
        self.assertIn("32M", command)


if __name__ == "__main__":
    unittest.main()
