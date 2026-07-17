# aos-skills

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![MSRV: 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](https://www.rust-lang.org)

**The skills loader for [Unicity AOS](https://github.com/unicity-aos/aos-ce).**

In the OS model, this capsule is the package manager's index. It discovers skill definitions from the workspace and global directories so the agent knows what commands are available.

## Tools

**`list_skills`** - Scans a directory for skill subdirectories containing `SKILL.md` files. Searches the workspace first (takes priority on duplicate IDs), then the global directory (`global://` prefix). Returns a JSON array of skill ID, name, and description.

**`read_skill`** - Reads the `SKILL.md` content for a specific skill ID. Checks the workspace first, falls back to global.

## SKILL.md format

Skills are discovered by the presence of a `SKILL.md` file with YAML-like frontmatter:

```markdown
---
name: my-skill
description: Does a thing
---

# My Skill

Skill content here...
```

The frontmatter parser extracts `name` and `description` fields. It is not a general YAML parser - it handles simple `key: value` pairs only.

## Security

- **Path traversal protection:** `dir_path` rejects `..`, null bytes, and unknown URL schemes. Skill IDs reject dot-prefixed names, slashes, and null bytes.
- **Workspace wins:** If a workspace skill ID exists (even with broken frontmatter), the global version is blocked. This prevents a global skill from shadowing a workspace override.

## Development

```bash
rustup target add wasm32-unknown-unknown
cargo build --target wasm32-unknown-unknown --release
cargo test
```

## License

Dual-licensed under [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE).

Copyright (c) 2025-2026 Joshua J. Bouw and Unicity Labs.
