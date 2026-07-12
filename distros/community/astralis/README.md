# Astralis

The flagship Astrid distribution — a curated bundle of capsules for the complete AI assistant experience.

## What is this?

Astralis is a **distro manifest** — a `Distro.toml` file that declares which capsules to install, their versions, and how they connect. It is not code. It is metadata that `astrid init` reads to set up a working environment.

## Quick start

```bash
cargo install astrid
astrid init
```

`astrid init` fetches this manifest, prompts you to select providers (e.g. which LLM backend), and installs everything.

## What's included

| Category | Capsules |
|----------|----------|
| **Uplinks** | cli, registry |
| **LLM providers** | openai-compat (select during init) |
| **Core** | react, session, identity, router, prompt-builder, context-engine, hook-bridge |
| **Tools** | shell, http, fs |
| **Extensions** | skills, memory |

## Customising

Edit `Distro.toml` to add or remove capsules, change versions, or add new LLM providers. The format follows semver constraints — see the [Astrid documentation](https://github.com/unicity-astrid/astrid) for the full manifest reference.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), at your option.
