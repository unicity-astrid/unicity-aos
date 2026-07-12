# astrid-capsule-http

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![MSRV: 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](https://www.rust-lang.org)

**HTTP fetch tool for [Astrid OS](https://github.com/unicity-astrid/astrid) agents.**

In the OS model, this capsule is the network stack's user-space interface. It gives agents native HTTP access without shelling out to `curl`, with SSRF protection enforced at the host layer.

## Tool

**`fetch_url`** - Fetches a URL over HTTP or HTTPS. Returns a JSON object with `status`, `headers`, `body`, and an optional `truncated` flag.

- **URL validation:** Rejects empty URLs and non-http(s) schemes (case-insensitive)
- **Method allowlist:** GET, POST, PUT, DELETE, PATCH, HEAD, OPTIONS. TRACE, CONNECT, and others are rejected.
- **Response truncation:** 200KB soft limit with UTF-8 boundary-safe truncation. The host enforces a 10MB hard cap.
- **Error handling:** HTTP error statuses (4xx/5xx) are returned as data so the agent can reason about them. Only infrastructure failures (DNS, timeout, SSRF block) produce errors.

## Security

Headers are passed unfiltered to the host. The host's SSRF layer blocks private/local IPs at DNS resolution time, but header injection to public endpoints is within the threat model accepted by `net = ["*"]`. Full response headers (including `Set-Cookie`) enter the LLM context window by design.

## Development

```bash
cargo build --target wasm32-unknown-unknown --release
cargo test
```

## License

Dual-licensed under [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE).

Copyright (c) 2025-2026 Joshua J. Bouw and Unicity Labs.
