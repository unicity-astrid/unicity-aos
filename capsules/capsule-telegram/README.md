# capsule-telegram

Telegram Bot uplink capsule for [Unicity AOS](https://github.com/unicity-aos/aos-ce) — bridges the Telegram Bot API to the kernel IPC bus, giving your Unicity AOS agent a Telegram chat interface.

## Features

- **Streaming responses** — edits messages in real-time as the agent streams text, with throttled edits to respect Telegram rate limits
- **Markdown → Telegram HTML** — converts LLM markdown output (bold, italic, code blocks, links, headings) to Telegram's supported HTML subset
- **Approval flow** — renders approval requests as inline keyboard buttons (Allow Once / Allow Session / Deny)
- **Elicitation support** — select fields shown as inline keyboards, text/secret fields as prompts
- **Session management** — one session per Telegram chat, persisted in KV store across restarts
- **Access control** — optional allowlist of Telegram user IDs
- **Message chunking** — HTML-aware splitting for messages exceeding Telegram's 4096-byte limit
- **Bot commands** — `/start`, `/help`, `/reset`, `/cancel`

## Packaging Status

This capsule is not packaged or shipped in the current Unicity CE distribution.
Its `wasm32-wasip1` target is not yet supported by the capsule packager, so the
CLI cannot currently produce an installable `.capsule` artifact from this
directory. Do not pass the raw WASM output to `aos capsule install`.

For development, compile it directly from an `aos-ce` checkout:

```bash
cd capsules/capsule-telegram
cargo build --release
```

This produces a raw development WASM artifact only. Packaging support must land
before this capsule can be distributed or installed through AOS.

If an older development copy is already installed, remove it explicitly with:

```bash
aos capsule remove aos-telegram --purge
```

## Bot Setup

### Create a Telegram Bot

1. Open [@BotFather](https://t.me/BotFather) in Telegram
2. Send `/newbot` and follow the prompts
3. Copy the bot token

When packaging support lands, configuration will request:
- **Bot Token** — the token from BotFather
- **Allowed User IDs** — comma-separated Telegram user IDs (leave empty to allow all users)

Configuration is preserved across reinstalls.

> **Tip:** To find your Telegram user ID, message [@userinfobot](https://t.me/userinfobot).

## Architecture

```
Telegram Bot API                Runtime Kernel IPC Bus
  │                                │
  │  getUpdates (HTTP long poll)   │
  │◄───────────────────────────────│
  │                                │
  │  user message                  │  user.v1.prompt
  │───────────────────────────────►│──────────────────►  ReAct Loop
  │                                │
  │                                │  agent.v1.response
  │  sendMessage / editMessage  ◄──│◄──────────────────  ReAct Loop
  │◄───────────────────────────────│
  │                                │
  │                                │  astrid.v1.approval
  │  inline keyboard            ◄──│◄──────────────────  Approval Gate
  │◄───────────────────────────────│
  │                                │
  │  callback_query                │  astrid.v1.approval.response.*
  │───────────────────────────────►│──────────────────►  Approval Gate
```

The capsule runs a single `#[astrid::run]` loop that alternates between:
1. **Polling Telegram** — `getUpdates` with a 1-second timeout
2. **Polling IPC** — processing agent responses, approvals, and elicitations

All state (sessions, update offset) is persisted in the kernel KV store.

## Configuration

| Variable | Type | Required | Description |
|----------|------|----------|-------------|
| `bot_token` | secret | yes | Telegram Bot API token from @BotFather |
| `allowed_user_ids` | string | no | Comma-separated Telegram user IDs. Empty = unrestricted. |

## IPC Topics

### Subscribes To (receives)
- `agent.v1.response` — agent text responses (streaming + final)
- `agent.v1.stream.delta` — streaming text deltas
- `astrid.v1.approval` — approval requests
- `astrid.v1.elicit.*` — elicitation requests
- `astrid.v1.response.*` — system responses

### Publishes To (sends)
- `user.v1.prompt` — user input from Telegram
- `astrid.v1.approval.response.*` — approval decisions
- `astrid.v1.elicit.response.*` — elicitation values

## Development

```bash
# Build for WASM
cargo build --target wasm32-wasip1 --release

# Run tests (native)
cargo test
```

## License

MIT OR Apache-2.0
