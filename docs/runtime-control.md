# Unicity AOS and Astrid control boundary

Unicity AOS is the customer-facing operating system. Astrid is the portable
runtime it bundles and controls. The two communicate through a versioned,
authenticated local control protocol; Unicity never receives an in-process
kernel handle, the runtime signing key, or direct access to the runtime audit
store.

## Process and state layout

```text
Unicity AOS CLI and HTTP server
            |
            | authenticated local control protocol
            v
astrid-daemon + kernel
```

Unicity AOS owns `~/.unicity-os/`. When it starts its bundled runtime, it sets
`ASTRID_HOME=~/.unicity-os/runtime`. Standalone Astrid remains compatible with
`~/.astrid`, `.astrid`, and an explicitly supplied `ASTRID_HOME`.

## Ownership

| Surface | Owner |
| --- | --- |
| Public HTTP, web UI, streaming, installer, product update, distribution selection, model/provider and session UX | Unicity AOS |
| `unicity` CLI and optional `aos` convenience alias | Unicity AOS |
| Daemon lifecycle, capability enforcement, sandboxing, generic capsule operations, WIT, SDKs, and runtime diagnostics | Astrid |
| Local signed, hash-linked runtime enforcement records | Astrid Audit |
| Blockchain anchoring, reporting, retention, search, and customer audit workflows | Unicity Audit |

Astrid exposes no TCP or HTTP listener in the target architecture. Public and
product HTTP routes belong to Unicity AOS. Runtime operators use the Astrid
CLI or the same local control protocol.

## Control protocol rules

- The protocol is versioned and authenticated over a local endpoint.
- Requests carry a scoped caller identity; the daemon remains the source of
  authorization truth and stamps the effective principal on kernel work.
- Unicity may select composition and request lower limits, but cannot raise
  runtime capability, resource, or policy ceilings.
- The product server does not shell out to `astrid` and does not call old
  product commands. Both CLIs use a shared control client.
- Runtime audit receipts are exported as immutable, signed records. Unicity
  verifies and anchors receipts asynchronously; a blockchain outage cannot
  affect runtime authorization.

## Migration order

1. Extract a shared Astrid control-client crate from the existing local IPC
   path, with compatibility tests for daemon lifecycle and generic capsule
   operations.
2. Introduce `unicity` as a client of that protocol. Move chat, run, TUI,
   onboarding, product updates, distro selection, model/provider selection,
   and sessions into it.
3. Move every existing HTTP route behind a Unicity server that uses the same
   client. Replace direct kernel, event-bus, runtime-key, and audit-store
   access with scoped control operations.
4. Remove Astrid's HTTP listener only after the Unicity route and control
   protocol compatibility suites cover the migrated behaviour.

## Acceptance checks

- An Astrid Runtime install works without Unicity AOS.
- A fresh Unicity AOS install starts its bundled runtime under the product
  runtime home and pins its exact runtime artifact digest and ABI range.
- A product credential cannot act as unrestricted runtime administration.
- Approval and elicitation replies prove the responder capability at the
  runtime boundary.
- Runtime audit receipt verification and Unicity blockchain anchoring are
  independently testable.
