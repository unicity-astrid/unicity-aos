# Scaffold, build, install, test, diagnose, and release

## Scaffold

From the intended parent directory:

```bash
aos capsule new my-capsule
```

Or select a portable explicit parent:

```bash
aos capsule new my-capsule --path <parent>
```

`scaffold_capsule { "name": "my-capsule" }` returns the same core project as a
path-to-content map when the agent needs to write through governed tools.

Use `--force` only after inspecting an existing target. Installing the Rust
WASM target is a toolchain mutation; in noninteractive work use the explicit
CLI option or install it through the environment's normal toolchain workflow.

## Fast local loop

```bash
cargo fmt --check
cargo build
cargo test
aos capsule check
aos capsule build
```

The scaffold's Cargo config owns the target, so do not add a conflicting
`--target`. Plain Cargo proves the Rust/WASM compile but does not produce an
installable archive. `aos capsule build` emits `dist/<name>.capsule` and
packages the manifest, components, WIT, and other supported generic assets.

Run unit tests on pure parsing, policy, and transformation functions on the
host where possible. Add component or integration tests for WIT/IPC behavior.
Test a denied path for every meaningful authority boundary.

## Manifest and wiring checks

Before install:

1. Call `validate_manifest` with the full manifest.
2. Run `aos capsule check` for Rust macro versus manifest tool wiring.
3. Inspect the archive identity and authority delta.
4. Confirm required imports exist in the intended installation.

Treat `suggest_capabilities` as a draft. Remove false matches, narrow scopes,
and compare against actual host calls.

## Install and verify

```bash
aos capsule install ./dist/my-capsule.capsule
aos capsule list
aos status
```

Use `--workspace` only when the capsule intentionally belongs to the selected
workspace rather than the user's AOS installation. Let the CLI resolve
platform-specific install paths; never hand-copy raw WASM.

Installation may prompt for declared environment values and secrets. `--yes`
is for an explicitly noninteractive flow with supplied variables/defaults, not
a way to invent missing secret values.

A running daemon is nudged to load or upgrade a successful install. Editing
source still requires rebuild and reinstall; verify the running capsule rather
than assuming live activation succeeded.

## Grant and exercise

Installed does not mean granted to every principal. An integrated client may
elicit access on first tool use. Approval persists the capsule grant; decline,
cancel, absent elicitation support, or error denies the call. Distribution or
operator workflows can pre-grant through their explicit authority path.

After grant:

- call each exposed tool with a valid request;
- test invalid and adversarial arguments;
- confirm per-principal state and secrets do not cross users;
- observe logs and resource behavior;
- test the extension in the harness that will actually use it.

## Diagnose

Use `capsule_doctor { "name": "..." }` when a capsule is installed but tools
are absent or imports are unsatisfied. Also inspect:

- `aos capsule show <name>` and `aos capsule list --verbose` where supported;
- exact package/component filenames and installed version;
- publish/subscribe rows and generated handler names;
- required import providers;
- guest and daemon logs through the AOS/runtime diagnostic surfaces;
- the active principal and workspace selection.

A receive timeout is an empty successful poll, not a host error. A missing tool
is usually manifest/describe wiring, not evidence that the model ignored it.

## Upgrade and removal

An upgrade must preserve or deliberately migrate principal-scoped state and
configuration. Keep upgrade hooks idempotent, test from the prior supported
version, and compare capability/ACL deltas before replacement.

Removal can break dependents. Inspect the dependency tree before forcing it.
Purging configuration and secrets is more destructive than removing executable
bytes; make that intent explicit.

## First-party monorepo work

For a first-party AOS capsule:

- work in a branch/worktree from current remote main;
- add the capsule to the Cargo workspace and distribution/release contracts if
  it is genuinely new;
- use workspace dependency pins;
- update existing Unreleased changelog sections rather than creating a
  duplicate heading;
- run capsule unit tests plus release-contract and asset validation;
- do not mix a feature with a release version bump;
- do not publish or promote stable/dev/nightly channels without explicit
  release authority.

Dev and nightly validation are not permission to publish crates or move stable
channel pointers.

## Completion evidence

Hand off:

- source location and artifact identity;
- tests and checks actually run;
- exact authority delta;
- install/grant state by principal;
- known limitations or untested platform paths;
- whether changes are local, committed, pushed, reviewed, or released.

Keep “built,” “installed,” “granted,” “running,” and “released” as separate
claims.
