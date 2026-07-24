#!/usr/bin/env python3
"""Tests for the Linux Realm benchmark orchestrator."""

from __future__ import annotations

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


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
    def test_main_admits_current_artifacts_and_resolves_the_reference_binary(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifacts = [root / name for name in ("Image", "system", "checkpoint")]
            reference = root / "benchmark_linux"
            for artifact in [*artifacts, reference]:
                artifact.touch()

            with (
                mock.patch.object(BENCHMARK, "IMAGE", artifacts[0]),
                mock.patch.object(BENCHMARK, "SYSTEM", artifacts[1]),
                mock.patch.object(BENCHMARK, "CHECKPOINT", artifacts[2]),
                mock.patch.object(BENCHMARK, "reference_binary", return_value=reference),
                mock.patch.object(BENCHMARK, "metadata", return_value={"kind": "metadata"}),
                mock.patch.object(BENCHMARK, "run_reference", return_value=[]),
                mock.patch.object(BENCHMARK, "run_docker", return_value=[]),
                mock.patch.object(BENCHMARK, "write_records") as write_records,
            ):
                self.assertEqual(
                    BENCHMARK.main(
                        [
                            "--samples",
                            "1",
                            "--warmups",
                            "1",
                            "--skip-qemu",
                            "--no-build",
                        ]
                    ),
                    0,
                )
                write_records.assert_called_once()

    def test_hart_matrix_is_explicit_and_defaults_to_checkpoint_topology(self) -> None:
        self.assertEqual(BENCHMARK.parse_args([]).hart_counts, [2])
        self.assertEqual(
            BENCHMARK.parse_args(["--hart-counts", "1", "2", "4"]).hart_counts,
            [1, 2, 4],
        )

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
        self.assertEqual(command[command.index("-m") + 1], "1G")
        self.assertEqual(command[command.index("-smp") + 1], "2")
        self.assertIn(
            f"aos.system_bytes={BENCHMARK.SYSTEM.stat().st_size}",
            command[command.index("-append") + 1],
        )


if __name__ == "__main__":
    unittest.main()
