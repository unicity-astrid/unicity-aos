# aos-users

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![MSRV: 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](https://www.rust-lang.org)

**Within-principal user identity store for [Unicity AOS](https://github.com/unicity-aos/aos-ce).**

This capsule owns the cross-platform user-identity surface that used to live inside the kernel (`astrid-core::identity`, `astrid-storage::identity`, the `identity` host fn). It maps platform-specific IDs (Discord, Telegram, Nostr, web passkeys, …) onto canonical `AstridUserId`s — without making the kernel grow new types every time a frontend lands.

Kernel-side principal tenancy is unchanged. This capsule covers identity *within* one principal: every record lives in the capsule's per-principal KV scope, and different principals have independent stores.

Implements the [`astrid:users@1.0.0`](https://github.com/astrid-runtime/wit/blob/main/interfaces/users.wit) interface.

## Topics

Each operation is a request/response pair on the IPC bus, correlated by the requester-supplied `correlation-id`. Responses publish to a fixed topic; consumers filter by `correlation-id`.

| Request topic | Response topic | Purpose |
|---|---|---|
| `users.v1.resolve.request` | `users.v1.resolve.response` | Platform identity → `AstridUser` |
| `users.v1.link.request`    | `users.v1.link.response`    | Upsert a platform link |
| `users.v1.unlink.request`  | `users.v1.unlink.response`  | Remove a platform link |
| `users.v1.create.request`  | `users.v1.create.response`  | Create a new user |
| `users.v1.links.request`   | `users.v1.links.response`   | List every link for one user |
| `users.v1.get.request`     | `users.v1.get.response`     | Fetch one user by UUID |
| `users.v1.delete.request`  | `users.v1.delete.response`  | Delete user + cascade links |
| `users.v1.list.request`    | `users.v1.list.response`    | List every user in this principal |

Every request carries a `source` envelope:

```json
{
  "channel": "discord",
  "user-id": "00000000-0000-4000-8000-000000000000",
  "correlation-id": "9d8a7f3e-..."
}
```

`channel` identifies the originating uplink (free-form string), `user-id` is the requester's own `AstridUserId` when known, and `correlation-id` is the token the requester filters the response topic by. Multi-tenant uplinks (one runtime principal, many end-users) generate a fresh correlation per inflight request and route each response back to the originating end-user.

## Why a capsule instead of a host fn

Identity-as-domain is business logic: what counts as a link, how platforms map, how pairing works, how recovery flows. The kernel principle is to route events and enforce capabilities; everything domain-specific belongs in capsule-space. Until now identity lived in the kernel because nobody had written this capsule yet. Tracked in [astrid-runtime/astrid#747](https://github.com/astrid-runtime/astrid/issues/747).

## Storage layout

KV keys share the legacy kernel store's scheme so a future cutover can locate records by the same paths. Value shapes follow the WIT contract rather than the kernel's Rust serialization — `public_key` is a byte list (not base64), timestamps are millisecond-precision RFC 3339, and the kernel's redundant `principal` field is dropped (the capsule's per-principal KV scope already encodes it). Pre-launch there are no production records to migrate; any future migration tool transforms kernel records at cutover.

| Key | Value |
|---|---|
| `user/{uuid}` | JSON `AstridUser` |
| `link/{platform}/{platform_user_id}` | JSON `FrontendLink` |
| `name/{display_name}` | UTF-8 UUID string (best-effort lookup index — last writer wins) |

`platform` and `platform_user_id` are validated to reject `/` and `\0`. Without that gate, a caller passing `platform = "../user"` could collide with `user/{uuid}` keys through the link path.

## Development

```bash
rustup target add wasm32-unknown-unknown
cargo build --release --target wasm32-unknown-unknown
cargo test --lib --target $(rustc -vV | sed -n 's/host: //p')
```

Tests run on the host target (host-target `cargo test` won't link a runner for `wasm32-unknown-unknown`). They exercise pure-Rust logic — KV store operations, validation, request deserialization, JSON projection. IPC dispatch is exercised by the kernel's integration suite against the built WASM.

## License

Dual-licensed under [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE).

Copyright (c) 2025-2026 Joshua J. Bouw and Unicity Labs.
