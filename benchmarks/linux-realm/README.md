# Linux Realm benchmarks

This directory holds raw, versioned benchmark records for the AOS-owned Linux
Realm machine and explicitly labelled comparison engines. Generate a local run
from the repository root with:

```sh
python3 scripts/benchmark-linux-realm.py \
  --samples 30 --warmups 3 \
  --output /tmp/aos-linux-realm-benchmark.jsonl
```

The orchestrator never pulls a container image, silently changes an engine, or
turns an unavailable comparison into a numeric result. See the capsule README
and `docs/principal-linux-realm.md` for exact measurement boundaries.

## Recorded baseline

`2026-07-21-m2-ultra-9aa1885.jsonl` contains 30 samples after three discarded
warmups on an Apple M2 Ultra. It measures commit `9aa1885`, Linux Image SHA-256
`fd394b7e5b09638d52483fe2f417985ae1af6a730eea5bc3e415b97262f863de`,
and checkpoint SHA-256
`99d2f209891c1ad340a64a79b062c6c1156a0c0e68ca61ce3c0a622644fac4d1`.

| Engine and boundary | Median | p95 |
| --- | ---: | ---: |
| AOS native reference, cold to PID 1 marker | 276.619 ms | 323.653 ms |
| QEMU 11.0.2 single-threaded TCG, exact Image to PID 1 marker | 263.929 ms | 313.747 ms |
| AOS checkpoint validation/materialization to fresh authority bind | 4.862 ms | 5.027 ms |

At the shared cold boundary, this AOS reference run is 4.8% slower than QEMU's
median. Checkpoint admission is 56.9 times faster than the AOS native cold path,
but excludes completion of the freshly attached principal providers and the
remaining guest steps to `AOS READY`. The QEMU lane likewise stops at PID 1
because it cannot supply those Astrid providers. These are machine-backend
results, not full outer-Wasm/MCP latency.

Docker 29.1.2 was installed but its server was unavailable, so the baseline
contains a skip record and makes no Docker claim. QEMU snapshot restore,
Docker/CRIU restore, governed MCP request-to-result latency, memory residency,
and concurrent-principal scaling remain required comparison lanes.
