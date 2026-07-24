# Linux Realm benchmarks

This directory holds raw, versioned benchmark records for the AOS-owned Linux
Realm machine and explicitly labelled comparison engines. Generate a local run
from the repository root with:

```sh
python3 scripts/benchmark-linux-realm.py \
  --samples 30 --warmups 3 \
  --hart-counts 1 2 4 \
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

## Serialized vCPU topology baseline

`2026-07-24-m2-ultra-02e7e9f-vcpu-matrix.jsonl` contains 30 samples per
topology after three discarded warmups on the same Apple M2 Ultra. It measures
commit `02e7e9f`, Linux Image SHA-256
`1a010ecb701ff5397ebb92a12ac739993a05ef12ec76283392df2531e727a981`,
system SHA-256
`4460e0cdc883922a4ab68180f4ed8f0752cf34fe4659d14e3260826d20d1063a`,
and checkpoint SHA-256
`59aaa5e2f6764b9f027874be9d137aa100e47ad73f5eaf2bd889ded7ecd0a379`.
The JSONL file has BLAKE3
`51e63714872dbb893a9052e5c47829131a70d01e3856686307ed271f009e50dd`.

| Logical harts | Cold to PID 1 median / p95 | Cold to principal bind median / p95 | Charged steps to bind |
| ---: | ---: | ---: | ---: |
| 1 | 769.402 / 786.431 ms | 815.945 / 834.265 ms | 35,932,587 |
| 2 | 985.840 / 1,004.464 ms | 1,087.564 / 1,105.364 ms | 46,431,093 |
| 4 | 1,591.060 / 1,621.234 ms | 1,813.724 / 1,847.017 ms | 76,245,925 |

These are deliberately the pre-parallelism numbers. The deterministic engine
time-slices every logical hart on one native thread: aggregate throughput stays
near 42–44 million steps/s while Linux performs more SMP work. Relative to one
hart, two harts are 28.1% slower to init and four are 106.8% slower. This is the
baseline that real worker-affine vCPUs must beat; it is not evidence against
parallel harts.

The two-hart checkpoint reaches the principal-bind boundary in a median
21.319 ms (p95 21.838 ms), 51.0 times faster than two-hart cold bind. The run
explicitly skipped QEMU and had no Docker image configured. Signed outer-Wasm,
governed MCP, warm shell, and parallel-worker latency remain separate lanes.
