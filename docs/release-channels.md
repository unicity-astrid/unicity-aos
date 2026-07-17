# Signed AOS release channels

AOS resolves direct installs through signed `stable`, `dev`, and `nightly`
channel pointers. The default is `stable`:

```sh
curl --proto '=https' --tlsv1.2 -fsSL https://aos.unicity.ai/install.sh | sh
curl --proto '=https' --tlsv1.2 -fsSL https://aos.unicity.ai/install.sh | sh -s -- --channel dev
```

An exact release is a separate, mutually exclusive operation:

```sh
sh install.sh --version 2026.1.0
sh install.sh --version 2026.1.0-nightly.20260717.g<40-character-source-commit>
```

An exact nightly pin is deliberate and bypasses the moving channel pointer. It
still authenticates the exact tag-bound release metadata and archive, but it
does not update or consult the locally accepted nightly channel generation.

There is no GitHub `releases/latest` fallback. If a selected channel has not
been published, is expired, has an invalid signature, or conflicts with locally
accepted state, installation stops before writing `AOS_HOME`.

## Trust and metadata

Every immutable release publishes:

- `unicity-aos-<version>-<target>.tar.gz` and its Sigstore bundle;
- `unicity-aos-<version>-release.toml` and its Sigstore bundle; and
- BLAKE3 and SHA-256 checksum manifests.

The strict release document records its tag, source commit, exact release
workflow identity, four target assets and digests, compatibility pins, and the
two release-readiness gates. The exact accepted identity is:

```text
https://github.com/unicity-aos/aos-ce/.github/workflows/release.yml@refs/tags/<version>
```

The channel pointer is a strict TOML document with a channel name, monotonically
increasing generation, publication and expiry times, the immutable release
metadata digest, and the same four target records. Its exact accepted identity
is:

```text
https://github.com/unicity-aos/aos-ce/.github/workflows/promote-channel.yml@refs/heads/main
```

Product versions use `YYYY.MINOR.PATCH`: the year is calendar-based, while
minor and patch are canonical SemVer numbers rather than months.

Stable and dev point only to canonical `YYYY.MINOR.PATCH` releases. Nightly is
a deterministic prerelease of the reviewed product version:

```text
YYYY.MINOR.PATCH-nightly.YYYYMMDD.g<40-character-source-commit>
```

The installer authenticates the channel first, authenticates and hashes the
referenced immutable release metadata second, then authenticates and hashes the
selected target archive. It stores each accepted pointer and bundle together in
an immutable generation directory under `~/.aos/update/channels/`, then
atomically activates that generation. An installation lock serializes product
replacement and channel acceptance. Inactive generation directories are safe
after an interrupted install. A lower generation, or different bytes at the
same generation, is rejected. A rollback is therefore a new, higher generation
that points to an older immutable release; retained channel history is
append-only and never replaced.

Astrid Runtime 0.9.4 predates Astrid's immutable signed release metadata. Its
compatibility entry consequently records `release-metadata-available = false`
and empty source/asset/digest fields. It cannot be promoted by inventing those
values. A future runtime pin must name the signed Astrid metadata asset, source
commit, and BLAKE3 digest.

## Promotion operations

`.github/workflows/promote-channel.yml` is manual-only and must run from `main`.
It authenticates an already-published immutable AOS release, requires both
readiness gates, verifies that the tag resolves to the recorded source commit,
and requires every new promotion to advance the authenticated transaction
floor. An exact same-generation rerun is accepted only when it reuses the exact
signed transaction bytes left by an interrupted attempt. The workflow signs a
new pointer before its publication job.

Create these GitHub environments with Joshua as the required reviewer and
prevent administrator bypass:

- `release`
- `aos-channel-stable`
- `aos-channel-dev`
- `aos-channel-nightly`

Add an `AOS_RELEASE_ADMIN_TOKEN` secret to the protected `release` environment.
It needs repository Administration write permission for the one-time immutable
release bootstrap and Administration read permission for release preflight.
The bootstrap uses its scoped `GITHUB_TOKEN` with Contents write permission to
create the three channel releases; the administrator token is never used to
publish release assets.

The YAML environment name alone is not an approval policy; repository
environment settings and tag rules are part of the release boundary. Protect
calendar-version tags from force updates and deletion. Before the first product
release, run `.github/workflows/bootstrap-channels.yml` once from `main`. Through
the protected `release` environment it creates all three empty, published,
mutable channel prereleases while repository release immutability is disabled,
then enables repository immutable releases and verifies that the three earlier
channel containers remain mutable. GitHub applies the setting only to future
releases. Promotion refuses a missing, draft, non-prerelease, or immutable
channel container; it never creates one on demand.

The release workflow requires repository immutable releases to be enabled,
refuses every conflicting release record for its tag, assembles the complete
signed asset set as a write-once draft, and publishes only after that upload
succeeds. It then verifies that GitHub marked the product release immutable.
The channel publication job retains an immutable transaction and its exact
generation-named pointer and bundle before replacing the signed current pointer.
Publishing the current bundle first makes readers racing the two asset updates
fail closed.

### Interrupted publication recovery

A failed immutable release upload may leave a draft release. Rerunning the tag
workflow through the protected `release` environment publishes that draft only
when it has never been published and the existing draft independently passes
the complete asset contract: exact inventory, tagged source commit and
compatibility, release metadata, checksums, capsule contents, and every Sigstore
signature. Fresh keyless signatures are not expected to reproduce prior bundle
bytes. The workflow never changes an existing asset. If the draft is incomplete,
delete only the never-published draft after confirming the signed tag still
resolves to the intended commit, then rerun the release workflow. A release that
was ever published, downloaded, or made non-draft again must keep its tag and
bytes; issue a new product version instead of deleting, retagging, or replacing
it.

Channel promotion is transaction-first. A completed transaction asset is the
recovery boundary for its generation. Rerunning the same generation unpacks and
authenticates those exact bytes, repairs either missing immutable history asset,
then replaces the mutable current pair. Assets left by an interrupted GitHub
upload in `starter` or `open` state are removed automatically before retry.
Uploaded assets must be nonempty and unique. An uploaded transaction or history
asset with conflicting bytes fails closed and requires incident review; it is
never clobbered. A malformed or mismatched mutable current pair is recovered
only when its pointer exactly matches an authenticated transaction.

Merging this foundation creates no channel container, release, or pointer. The
protected bootstrap is an explicit one-time operation, and the current false
readiness flags continue to block product release and promotion workflows.

The scheduled nightly orchestrator is inert unless the repository variable
`AOS_NIGHTLY_RELEASES_ENABLED` is exactly `true`. When enabled, it tags the exact
`main` commit with the deterministic nightly version and explicitly dispatches
the tag-bound release workflow. The release still waits at the protected
`release` environment, and a successful release only requests promotion through
the protected `aos-channel-nightly` environment. Its ephemeral product-version
overlay is never committed to `main`.

Every AOS release, including nightly, uses the exact Astrid release metadata and
source commit pinned by `release/runtime-compatibility.toml`. AOS never follows
Astrid nightly implicitly. Changing the runtime is a reviewed product input, not
a consequence of either project's schedule.

Stable promotion is allowed only when the currently authenticated dev pointer
already identifies the exact same immutable product release and archives. It
does not rebuild or reinterpret dev bytes.

Homebrew remains stable-only. Its formula updater should consume the signed
`stable` pointer, never `dev`, `nightly`, or an arbitrary published version. A
channel rollback protects new direct installs; an already-upgraded Homebrew
installation normally needs a forward patch release rather than a version
downgrade.

## Branch and retention policy

`main` is the product development trunk. There is no permanent `develop`,
`stable`, or channel branch. After a stable yearly minor release, create
`release/YYYY.MINOR` only when that supported line needs a patch. Patch fixes
land there and are forward-ported to `main`; a release still requires a
deliberate signed tag and protected approval.

Stable and dev releases and their channel history are retained permanently.
Nightly releases and nightly transaction/history assets are retained for 90
days, except that the release, transaction, and history pair referenced by the
current nightly pointer are never deleted. Cleanup is a separate reviewed
maintenance operation and must be installed before the channel approaches
GitHub's per-release asset limit; the release train has no destructive
garbage-collection permission. GitHub release tags are never reused.
