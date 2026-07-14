# Importing standalone runtime state

Unicity AOS keeps its product state under `~/.unicity-os`. It does not take
ownership of `~/.astrid`, and it never changes a standalone Astrid Runtime
installation in place.

To copy compatible state deliberately, stop the standalone runtime and run:

```sh
aos migrate runtime --from "$HOME/.astrid"
```

On an interactive first run, `aos` can offer the same import. It never imports
without confirmation.

## What is copied

The importer copies persistent runtime state only:

- user configuration, keys, secrets, databases, WIT content, principal homes,
  shared libraries, trusted distribution keys, system capsules, and CLI
  history;
- content-addressed `.wasm` files from `bin/`;
- the known `etc/` configuration surface: runtime, MCP, gateway and HTTP
  configuration; layout version; group, invite, pairing and gateway revocation
  state; principal profiles; and system hooks.

The `etc/` list is deliberately explicit because it contains authorization and
identity policy. If a newer runtime introduces an unknown configuration or
top-level state path, the import stops and names that path instead of silently
dropping it.

Live sockets, readiness files, logs, copy-on-write working trees, and the old
runtime executables are not copied. AOS uses the runtime executable bundled
with its own release.

## Integrity and recovery

The source directory remains unchanged, so the standalone installation is the
rollback path until the operator chooses to remove it.

AOS builds the imported runtime in a private staging directory. Every copied
file is recorded with its path, byte length, and SHA-256 digest in a versioned
receipt. AOS validates the staged tree before replacing its empty bundled
runtime home. File data, directory entries, and the receipt are flushed before
the transaction is considered complete on platforms that support directory
synchronization.

The product runtime's pre-import directory is retained as a transaction backup
until the validated receipt is durable. If the process is interrupted, the next
import either completes a fully validated replacement or restores that backup
before retrying. A receipt whose files no longer match their recorded hashes is
rejected.

The receipt is product state at:

```text
~/.unicity-os/migrations/astrid-home-v1.json
```

Keep the standalone runtime stopped throughout the import. The importer refuses
to proceed while its system socket is present, when either root is a symlink,
when the source and product roots overlap, when the AOS runtime home already
contains user state, or when any source file would require following a symlink.

Only one import may run for an AOS home at a time. Concurrent attempts fail
before staging or replacing runtime state, and a process crash releases the
operating-system lock so the recovery path can run on the next attempt.
