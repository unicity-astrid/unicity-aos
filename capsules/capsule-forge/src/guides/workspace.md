# Workspace and source location

## Never assume one machine layout

Do not hardcode a developer's home directory, repository path, plugin cache,
or runtime install tree. Discover the current environment from the working
directory, repository instructions, CLI help, AOS configuration, and the
authority actually available to the agent.

Installed runtime files and plugin caches are deployment outputs, not source
workspaces. Do not develop a capsule inside them.

## Select the source home

Use the first applicable rule:

1. **Existing repository:** follow its instructions and established capsule or
   plugin layout. In the AOS Community Edition monorepo, first-party capsules
   live under `capsules/capsule-<name>/` and participate in the workspace and
   release contract.
2. **User supplied destination:** use it exactly, subject to the current
   filesystem boundary.
3. **Standalone project:** scaffold under the current working directory with
   `aos capsule new <name>`, or choose an explicit parent with
   `aos capsule new <name> --path <parent>`.
4. **Proactive candidate with no durable owner yet:** create it in an isolated,
   disposable scratch directory or worktree. Build and evaluate it there. Ask
   where it should live before moving, committing, installing, or publishing
   it if that destination would materially change the user's project.

For a shared or dirty repository, preserve existing changes. Create a fresh
worktree from the requested base before feature work when repository policy or
the user asks for isolation.

## Source, artifact, install, and state are different places

Keep these boundaries explicit:

```text
source project
  -> build output: dist/<name>.capsule
  -> installed capsule mirror and content-addressed component bytes
  -> principal-scoped runtime state, config, secrets, logs, and KV
```

- The source project is owned by the user or repository.
- `dist/*.capsule` is an installable build output.
- The AOS CLI chooses the platform-specific install directories.
- A capsule sees virtual paths and principal-scoped stores, not arbitrary host
  paths.

Never hand-copy a raw `.wasm` into the runtime tree. Installation records and
verifies content identity.

## Portable paths inside a capsule

Use AOS VFS schemes in the manifest and SDK:

- `home://` resolves to the calling principal's home.
- `cwd://` resolves to the capsule/workspace root supplied by the runtime.
- broad workspace-confined access may be represented by `"*"`; prefer a narrow
  prefix.

Do not embed `/Users/...`, `/home/...`, Windows drive letters, or shell-expanded
home paths in capsule code. Ask the VFS and AOS services for the world the
principal is allowed to see.

## Repository ownership and generated mirrors

Identify the canonical source before editing. A host plugin may vendor a
snapshot of a first-party skill, while the durable runtime copy lives in a
capsule. Release artifacts and installed mirrors are generated outputs unless
the repository explicitly says otherwise.

When multiple repositories are involved, make each change in a branch from
that repository's remote main. Do not silently edit a dirty neighboring repo
to keep a generated mirror in sync; use the documented generation or release
path.

## Decision checklist

Before writing:

- What repository or user-space scope owns the source?
- Is the current checkout shared or dirty?
- Is this a durable implementation or an isolated candidate?
- Does the chosen directory exist on other supported platforms?
- Am I editing source rather than a cache or installed runtime?
- Who may authorize moving, installing, granting, or publishing the result?

If only the final ownership is unclear, continue safely in scratch rather than
blocking the design. Preserve the candidate and ask at the first irreversible
or externally visible boundary.
