---
name: capacity-planning
description: Measure and model how many attached agents an Astrid or Unicity AOS installation can safely support. Use when sizing multi-tenant deployments, investigating an apparent agent limit, tuning capsule concurrency or network-stream capacity, validating a performance change, or explaining density gains without mistaking a configured ceiling for a measured maximum.
---

# Capacity Planning

Treat capacity as a measured envelope, not a universal agent count. Provisioned
principals are cheap records; attached agents consume live runtime resources;
active work adds workload-dependent CPU, memory, network, IPC, and provider
pressure. Report each separately.

## Measure

1. Record host facts: usable memory after the operator's reserve, logical CPUs,
   open-file limit, and relevant AOS config. Do not invent a reserve percentage.
2. Warm an otherwise-idle installation with its normal system capsules and no
   external agent attachments. Record steady resident memory as the first
   estimate of shared cost `S`.
3. Attach representative agents at geometric counts such as 1, 2, 4, 8, ... .
   At each point, wait for steady state, perform a functional request, and record
   resident memory, open descriptors, CPU, p95 latency, errors, and IPC drops.
4. Estimate marginal attached-agent memory `A` from the median pairwise slope
   `(memory_j - memory_i) / (agents_j - agents_i)`. Re-estimate shared cost as
   the median `memory_i - A * agents_i`. More points beat one subtraction.
5. Stop before host pressure. A safe logarithmic probe does not need to
   saturate the machine to reveal the linear terms or the first bottleneck.

If a probe stops at a round number, inspect configuration and contract limits
before calling it a maximum. A quota proves only that the quota was reached.

## Model

For `N` attached agents:

- total idle memory: `M(N) = S + N*A`
- memory per agent: `A + S/N`
- density gain over isolated per-agent runtimes:
  `G(N) = N*(S+A)/(S+N*A)`
- asymptotic density gain: `G(infinity) = (S+A)/A = 1 + S/A`
- memory-bound count from an operator-supplied usable budget `B`:
  `floor(max(0, B-S)/A)`

Call the AOS `model_capacity` tool with measured `S`, `A`, and any known
resource envelopes. It returns the math and candidate bounds without choosing
an arbitrary safety factor. For descriptor modeling, pass an operator-chosen
`usable_file_descriptors` value that already excludes desired OS headroom,
along with the measured fixed and per-agent descriptor costs.

The deployable count is the smallest independently justified bound:

`min(memory, network streams, poll set, file descriptors, CPU/latency, IPC budgets, provider quotas)`

Do not combine unlike states. Publish at least three numbers when relevant:
provisioned principals, idle attached agents, and agents active at a stated
workload and service-level objective.

## Tune AOS

Prefer existing operator controls:

- `capsule.host_net_streams` controls the process-wide persistent network-stream
  envelope. The host-derived default preserves descriptor headroom. The same
  value can be supplied through `ASTRID_CAPSULE_HOST_NET_STREAMS` or the daemon
  `--host-net-streams` override.
- `capsule.host_io_concurrency` bounds asynchronous host I/O.
- `capsule.host_blocking_concurrency` bounds blocking host calls.
- `capsule.instance_pool_size` bounds concurrent pooled capsule instances.

Raise a bound only after identifying it as the active bottleneck. Re-run the
probe after every change and watch for the bottleneck moving elsewhere. Keep
public wire contracts stable and prefer a shared host-level envelope when a
per-store or per-capsule limit would multiply with pools.

## Report

State the evidence and claim boundary:

- host and configuration used;
- `S`, `A`, sample points, and fitting method;
- formula-derived counts and which resource wins;
- highest count functionally exercised;
- whether that count was a configured stop, a safe test stop, or saturation;
- active-workload definition and SLO for any throughput claim.

Never present the asymptote as a tested density, or the highest exercised count
as the maximum when saturation was not discovered.
