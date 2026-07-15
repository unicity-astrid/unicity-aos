# Changelog

## [2026.1.0] - Unreleased

### Added

- The `aos` product command and product-owned `~/.unicity-os` state boundary.
- A pinned Unicity CE distribution manifest over Astrid Runtime 0.9.4, emplaced
  as the bundled runtime's operator-enforced distro.
- Reproducible macOS and Linux release bundles with primary BLAKE3 and
  Homebrew-compatible SHA-256 checksum manifests, Sigstore bundles, GitHub
  build-provenance attestations, and explicit runtime/WIT compatibility
  metadata.
- An idempotent product installer and updater that preserve runtime state while
  replacing the coordinated AOS and Astrid executable set.
- Schema-3 runtime-import receipts with canonical `blake3:<hex>` content digests
  and fail-closed rejection of pre-release SHA-256 receipts.
- A signed release path for the 18 installable `astrid-capsule-*` artifacts
  selected by Community Edition, with exact source/manifest identity checks,
  archive safety validation, BLAKE3 checksums, SHA-256 compatibility checksums,
  Sigstore bundles, and provenance.
- Host-target unit-test coverage for the capsule workspace.
- Homebrew formula updates initiated by the tap's authenticated stable-release
  poll, eliminating the cross-repository dispatch credential.

### Changed

- Parse AOS-owned commands with Clap-generated validation and help while
  preserving byte-for-byte delegation of inherited runtime commands.
- Initialize against the operator-enforced Community Edition manifest and ask
  the runtime to grant its installed capsule set to the resolved target
  principal.
- Present product-facing capsule copy consistently as Unicity AOS while
  preserving stable Astrid Runtime crate, WIT, topic, artifact, and ABI names.
