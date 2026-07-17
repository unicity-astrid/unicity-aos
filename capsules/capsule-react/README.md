# aos-react

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![MSRV: 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](https://www.rust-lang.org)

**The ReAct loop coordinator for [Unicity AOS](https://github.com/unicity-aos/aos-ce).**

In the OS model, this capsule is the shell. It drives the reasoning-and-action cycle that makes an agent an agent: fetch context, think, act, observe, repeat.

## State machine

```text
Idle -> AwaitingIdentity -> AwaitingPromptBuild -> Streaming -> AwaitingTools -> Streaming -> ... -> Idle
```

The react loop contains no inference logic. It is pure control flow that coordinates five other capsules over the IPC event bus:

1. **Session** - fetch conversation history, persist turn results
2. **Identity** - build the system prompt from workspace config
3. **Prompt Builder** - merge plugin contributions into the final prompt
4. **Provider** (e.g. Anthropic) - stream LLM responses
5. **Tool Router** - dispatch tool calls and collect results

## What it does not do

This capsule does not call LLMs, execute tools, build prompts, or store messages. It orchestrates the capsules that do. Replacing this capsule with a different orchestration strategy (debate, MCTS, chain-of-verification) changes the agent's behaviour without touching any other capsule.

## Persistence

Ephemeral per-turn state is stored in the capsule KV store:
- `react.turn.{session_id}` - current turn state
- `react.req2sess.{request_id}` - request-to-session correlation
- `react.call2sess.{call_id}` - tool call-to-session correlation

This is control flow state, not conversation history. History lives in the session capsule.

## Development

```bash
rustup target add wasm32-unknown-unknown
cargo build --target wasm32-unknown-unknown --release
```

## License

Dual-licensed under [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE).

Copyright (c) 2025-2026 Joshua J. Bouw and Unicity Labs.
