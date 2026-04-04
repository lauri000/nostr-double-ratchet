# nostr-double-ratchet (Rust)

Rust now ships as two crates:

| Crate | Description |
|---|---|
| [nostr-double-ratchet](./crates/nostr-double-ratchet) | Domain core with `SessionManager`, `Session`, `Invite`, `DeviceRoster`, and snapshots |
| [nostr-double-ratchet-nostr](./crates/nostr-double-ratchet-nostr) | Nostr adapter for events, invite URLs, invite responses, and roster events |

## Core Rules

- `SessionManager` is the only supported high-level API.
- `Session` and `Invite` remain public for lower-level device-to-device usage.
- The core is synchronous and ownership-driven.
- Time and randomness are explicit through `ProtocolContext`.
- The core carries no relay runtime, storage abstraction, or background coordination.

## Important Types

Core crate:

- `SessionManager`
- `Session`
- `Invite`
- `MessageEnvelope`
- `InviteResponseEnvelope`
- `DeviceRoster`
- `AuthorizedDevice`
- `ProtocolContext`
- `SessionManagerSnapshot`
- `SessionState`

Adapter crate:

- `nostr_double_ratchet_nostr::nostr::message_event`
- `nostr_double_ratchet_nostr::nostr::parse_message_event`
- `nostr_double_ratchet_nostr::nostr::invite_url`
- `nostr_double_ratchet_nostr::nostr::parse_invite_url`
- `nostr_double_ratchet_nostr::nostr::invite_response_event`
- `nostr_double_ratchet_nostr::nostr::parse_invite_response_event`
- `nostr_double_ratchet_nostr::nostr::roster_unsigned_event`
- `nostr_double_ratchet_nostr::nostr::parse_roster_event`

## Tests

```bash
cargo test --manifest-path rust/Cargo.toml
cargo clippy --manifest-path rust/Cargo.toml --workspace --tests -- -D warnings
```
