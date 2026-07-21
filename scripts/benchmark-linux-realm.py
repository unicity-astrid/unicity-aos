#!/usr/bin/env python3
"""Run reproducible Linux Realm and locally available comparison benchmarks."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import platform
import selectors
import shutil
import statistics
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Sequence


SCHEMA = "aos-linux-realm-benchmark/v1"
ROOT = Path(__file__).resolve().parent.parent
REALM = ROOT / "capsules" / "capsule-linux-realm"
IMAGE = REALM / "linux" / "Image"
SOURCES = REALM / "linux" / "SOURCES.lock"
CHECKPOINT = REALM / "linux" / "prewarm-32m.aos-machine"
REFERENCE_BINARY = ROOT / "target" / "release" / "examples" / "benchmark_linux"
INIT_MARKER = b"AOS LINUX /init"
MAX_TRANSCRIPT_BYTES = 2 * 1024 * 1024


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--samples", type=positive_int, default=10)
    parser.add_argument("--warmups", type=positive_int, default=2)
    parser.add_argument("--timeout", type=positive_float, default=10.0)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--skip-qemu", action="store_true")
    parser.add_argument(
        "--docker-image",
        help="existing local image used for Docker run and unpause measurements",
    )
    return parser.parse_args(argv)


def positive_int(value: str) -> int:
    parsed = int(value)
    if parsed <= 0:
        raise argparse.ArgumentTypeError("must be greater than zero")
    return parsed


def positive_float(value: str) -> float:
    parsed = float(value)
    if not math.isfinite(parsed) or parsed <= 0:
        raise argparse.ArgumentTypeError("must be finite and greater than zero")
    return parsed


def command_output(command: Sequence[str]) -> str | None:
    try:
        completed = subprocess.run(
            command,
            cwd=ROOT,
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            timeout=10,
        )
    except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired):
        return None
    return completed.stdout.strip()


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def metadata() -> dict[str, Any]:
    return {
        "schema": SCHEMA,
        "kind": "metadata",
        "recorded_at": datetime.now(timezone.utc).isoformat(),
        "git_commit": command_output(["git", "rev-parse", "HEAD"]),
        "host": {
            "system": platform.system(),
            "release": platform.release(),
            "machine": platform.machine(),
            "model": command_output(["sysctl", "-n", "machdep.cpu.brand_string"])
            or platform.processor()
            or None,
            "cpu_count": os.cpu_count(),
        },
        "tools": {
            "python": platform.python_version(),
            "rustc": command_output(["rustc", "-vV"]),
            "qemu": command_output(["qemu-system-riscv64", "--version"]),
            "docker_client": command_output(
                ["docker", "version", "--format", "{{.Client.Version}}"]
            ),
        },
        "artifacts": {
            "linux_image_sha256": sha256(IMAGE),
            "checkpoint_sha256": sha256(CHECKPOINT),
            "checkpoint_bytes": CHECKPOINT.stat().st_size,
        },
        "boundaries": {
            "cold-to-init": (
                "preloaded image; includes 32 MiB machine allocation, image admission, "
                "and RV64 execution through PID 1's AOS LINUX /init marker; observation "
                "is rounded up to the next 100,000-step cooperative slice boundary"
            ),
            "cold-to-principal-bind": (
                "preloaded image; includes 32 MiB machine allocation, image admission, "
                "and RV64 execution through PID 1's first principal-home request"
            ),
            "checkpoint-to-bindable": (
                "preloaded checkpoint; includes integrity and artifact-binding validation, "
                "32 MiB machine allocation, sparse RAM materialization, and handoff at the "
                "pending principal-home request; excludes provider completion"
            ),
            "qemu-tcg-cold-to-init": (
                "fresh QEMU process with the exact AOS Image through PID 1's init marker; "
                "QEMU cannot reach AOS READY without Astrid storage providers"
            ),
            "docker-run-to-exit": (
                "Docker CLI create, start, /bin/true, and --rm using an existing local image"
            ),
            "docker-unpause": "Docker CLI round trip to unfreeze an already resident container",
        },
    }


def build_reference() -> None:
    subprocess.run(
        [
            "cargo",
            "build",
            "--release",
            "-p",
            "aos-realm-machine",
            "--example",
            "benchmark_linux",
        ],
        cwd=ROOT,
        check=True,
    )


def run_reference(samples: int, warmups: int) -> list[dict[str, Any]]:
    completed = subprocess.run(
        [
            str(REFERENCE_BINARY),
            str(IMAGE),
            str(SOURCES),
            str(CHECKPOINT),
            str(samples),
            str(warmups),
        ],
        cwd=ROOT,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    records = []
    for line in completed.stdout.splitlines():
        record = json.loads(line)
        if record.get("schema") != SCHEMA or record.get("kind") != "sample":
            raise RuntimeError("reference benchmark emitted an unknown record")
        records.append(record)
    if len(records) != samples * 3:
        raise RuntimeError(
            f"reference benchmark emitted {len(records)} samples, expected {samples * 3}"
        )
    return records


def qemu_command(qemu: str) -> list[str]:
    return [
        qemu,
        "-machine",
        "virt",
        "-accel",
        "tcg,thread=single",
        "-m",
        "32M",
        "-smp",
        "1",
        "-nographic",
        "-kernel",
        str(IMAGE),
        "-append",
        "earlycon=sbi console=hvc0 init=/init panic=-1",
        "-no-reboot",
    ]


def process_to_marker(command: Sequence[str], marker: bytes, timeout: float) -> int:
    started = time.perf_counter_ns()
    process = subprocess.Popen(
        command,
        cwd=ROOT,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    assert process.stdout is not None
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    transcript = bytearray()
    deadline = time.monotonic() + timeout
    found_at: int | None = None
    try:
        while time.monotonic() < deadline:
            events = selector.select(max(0.0, deadline - time.monotonic()))
            if not events:
                break
            chunk = os.read(process.stdout.fileno(), 64 * 1024)
            if not chunk:
                break
            transcript.extend(chunk)
            if len(transcript) > MAX_TRANSCRIPT_BYTES:
                del transcript[: len(transcript) - MAX_TRANSCRIPT_BYTES]
            if marker in transcript:
                found_at = time.perf_counter_ns()
                break
    finally:
        selector.close()
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=2)
        process.stdout.close()
    if found_at is None:
        tail = bytes(transcript[-4096:]).decode("utf-8", errors="replace")
        raise RuntimeError(f"process did not emit {marker!r} within {timeout}s; tail:\n{tail}")
    return found_at - started


def run_qemu(samples: int, warmups: int, timeout: float) -> list[dict[str, Any]]:
    qemu = shutil.which("qemu-system-riscv64")
    if qemu is None:
        return [skip("qemu-tcg-cold-to-init", "qemu-system-riscv64 not installed")]
    command = qemu_command(qemu)
    for _ in range(warmups):
        process_to_marker(command, INIT_MARKER, timeout)
    version = command_output([qemu, "--version"])
    records = []
    for iteration in range(samples):
        records.append(
            {
                "schema": SCHEMA,
                "kind": "sample",
                "engine": "qemu-system-riscv64",
                "engine_version": version.splitlines()[0] if version else None,
                "scenario": "qemu-tcg-cold-to-init",
                "iteration": iteration,
                "duration_ns": process_to_marker(command, INIT_MARKER, timeout),
                "guest_steps": None,
                "guest_instructions_retired": None,
                "ram_bytes": 32 * 1024 * 1024,
                "checkpoint_bytes": None,
            }
        )
    return records


def docker_server_version(docker: str) -> str | None:
    return command_output([docker, "version", "--format", "{{.Server.Version}}"])


def timed_command(command: Sequence[str], timeout: float) -> int:
    started = time.perf_counter_ns()
    subprocess.run(
        command,
        cwd=ROOT,
        check=True,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        timeout=timeout,
    )
    return time.perf_counter_ns() - started


def run_docker(
    image: str | None, samples: int, warmups: int, timeout: float
) -> list[dict[str, Any]]:
    if image is None:
        return [skip("docker", "no --docker-image supplied; implicit pulls are forbidden")]
    docker = shutil.which("docker")
    if docker is None:
        return [skip("docker", "docker CLI not installed")]
    version = docker_server_version(docker)
    if not version:
        return [skip("docker", "Docker server is unavailable")]
    if command_output([docker, "image", "inspect", image]) is None:
        return [skip("docker", f"local image {image!r} is absent; no pull attempted")]

    run_command = [docker, "run", "--rm", image, "/bin/true"]
    for _ in range(warmups):
        timed_command(run_command, timeout)
    records: list[dict[str, Any]] = []
    for iteration in range(samples):
        records.append(
            sample("docker", version, "docker-run-to-exit", iteration, timed_command(run_command, timeout))
        )

    container = command_output(
        [docker, "create", image, "/bin/sh", "-c", "while :; do sleep 3600; done"]
    )
    if not container:
        records.append(skip("docker-unpause", "could not create benchmark container"))
        return records
    try:
        subprocess.run([docker, "start", container], check=True, stdout=subprocess.DEVNULL)
        for iteration in range(samples + warmups):
            subprocess.run([docker, "pause", container], check=True, stdout=subprocess.DEVNULL)
            duration_ns = timed_command([docker, "unpause", container], timeout)
            if iteration >= warmups:
                records.append(
                    sample(
                        "docker",
                        version,
                        "docker-unpause",
                        iteration - warmups,
                        duration_ns,
                    )
                )
    finally:
        subprocess.run(
            [docker, "rm", "-f", container],
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    return records


def sample(
    engine: str, version: str | None, scenario: str, iteration: int, duration_ns: int
) -> dict[str, Any]:
    return {
        "schema": SCHEMA,
        "kind": "sample",
        "engine": engine,
        "engine_version": version,
        "scenario": scenario,
        "iteration": iteration,
        "duration_ns": duration_ns,
        "guest_steps": None,
        "guest_instructions_retired": None,
        "ram_bytes": None,
        "checkpoint_bytes": None,
    }


def skip(scenario: str, reason: str) -> dict[str, Any]:
    return {
        "schema": SCHEMA,
        "kind": "skip",
        "scenario": scenario,
        "reason": reason,
    }


def percentile(values: Sequence[int], fraction: float) -> int:
    if not values:
        raise ValueError("cannot summarize an empty sample")
    ordered = sorted(values)
    position = (len(ordered) - 1) * fraction
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return ordered[lower]
    weight = position - lower
    return round(ordered[lower] * (1 - weight) + ordered[upper] * weight)


def summarize(records: Sequence[dict[str, Any]]) -> list[dict[str, Any]]:
    by_scenario: dict[tuple[str, str], list[int]] = {}
    for record in records:
        if record.get("kind") != "sample":
            continue
        key = (str(record["engine"]), str(record["scenario"]))
        by_scenario.setdefault(key, []).append(int(record["duration_ns"]))
    summaries = []
    for (engine, scenario), durations in sorted(by_scenario.items()):
        summaries.append(
            {
                "schema": SCHEMA,
                "kind": "summary",
                "engine": engine,
                "scenario": scenario,
                "samples": len(durations),
                "duration_ns": {
                    "min": min(durations),
                    "median": round(statistics.median(durations)),
                    "mean": round(statistics.fmean(durations)),
                    "p95": percentile(durations, 0.95),
                    "max": max(durations),
                    "stdev": round(statistics.stdev(durations))
                    if len(durations) > 1
                    else 0,
                },
            }
        )
    return summaries


def write_records(records: Sequence[dict[str, Any]], output: Path | None) -> None:
    payload = "".join(json.dumps(record, sort_keys=True) + "\n" for record in records)
    if output is None:
        sys.stdout.write(payload)
        return
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(payload, encoding="utf-8")
    print(f"wrote {len(records)} records to {output}", file=sys.stderr)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    for artifact in (IMAGE, SOURCES, CHECKPOINT):
        if not artifact.is_file():
            raise RuntimeError(f"required benchmark artifact is missing: {artifact}")
    if not args.no_build:
        build_reference()
    elif not REFERENCE_BINARY.is_file():
        raise RuntimeError(f"--no-build requested but {REFERENCE_BINARY} does not exist")

    records = [metadata()]
    records.extend(run_reference(args.samples, args.warmups))
    if args.skip_qemu:
        records.append(skip("qemu-tcg-cold-to-init", "disabled by --skip-qemu"))
    else:
        records.extend(run_qemu(args.samples, args.warmups, args.timeout))
    records.extend(run_docker(args.docker_image, args.samples, args.warmups, args.timeout))
    records.extend(summarize(records))
    write_records(records, args.output)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"benchmark failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
