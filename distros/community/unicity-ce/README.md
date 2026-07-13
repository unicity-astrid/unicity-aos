# Unicity CE

The flagship Unicity CE distribution — a curated bundle of
capsules for the complete agent operating system experience.

## What is this?

Unicity CE is a **distro manifest** — a `Distro.toml` file that declares which
capsules to install, their versions, and how they connect. It is not code. It is
product metadata that `unicity init` reads to set up a working environment.

## Quick start

```bash
unicity init
```

`unicity init` fetches this manifest, prompts you to select providers (for example,
which LLM backend), and installs everything.

## What's included

| Category | Capsules |
|----------|----------|
| **Uplinks** | cli, registry |
| **LLM providers** | openai-compat (select during init) |
| **Core** | react, session, identity, router, prompt-builder, context-engine, hook-bridge |
| **Tools** | shell, http, fs |
| **Extensions** | skills, memory |

## Customising

Edit `Distro.toml` to add or remove capsules, change versions, or add new LLM
providers. The format follows semver constraints; the product release documents the
supported runtime compatibility range.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), at your option.
