# nostr-double-ratchet (Rust)

Rust now ships as two crates:

| Crate | Description |
|---|---|
| [nostr-double-ratchet](./crates/nostr-double-ratchet) | Domain core with `SessionManager`, `GroupManager`, `RosterEditor`, `Session`, `Invite`, `DeviceRoster`, and snapshots |
| [nostr-double-ratchet-nostr](./crates/nostr-double-ratchet-nostr) | Nostr adapter for events, invite URLs, invite responses, and roster events |

## Core Rules

- `SessionManager` is the owner/device orchestration API.
- `GroupManager` is the pairwise-fanout group layer above `SessionManager`.
- `RosterEditor` is the supported roster-CRUD helper outside `SessionManager`.
- `Session` and `Invite` remain public for lower-level device-to-device usage.
- The core is synchronous and ownership-driven.
- Time and randomness are explicit through `ProtocolContext`.
- The core carries no relay runtime, storage abstraction, or background coordination.

## Important Types

Core crate:

- `SessionManager`
- `GroupManager`
- `RosterEditor`
- `Session`
- `Invite`
- `MessageEnvelope`
- `InviteResponseEnvelope`
- `DeviceRoster`
- `AuthorizedDevice`
- `ProtocolContext`
- `SessionManagerSnapshot`
- `GroupManagerSnapshot`
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

## Recommended Model

- Use `Session` + `Invite` for direct device-to-device integrations.
- Use `SessionManager` for owner/device orchestration.
- Use `GroupManager` on top of `SessionManager` for pairwise-fanout group chat.
- When using `SessionManager`, always create a separate local device key, even if the roster has only one device.
- Build and edit the authoritative roster with `RosterEditor`, then apply it with `SessionManager::apply_local_roster(...)`.

## Tests

```bash
cargo test --manifest-path rust/Cargo.toml
cargo clippy --manifest-path rust/Cargo.toml --workspace --tests -- -D warnings
```

See [./crates/nostr-double-ratchet/TUTORIAL.md](./crates/nostr-double-ratchet/TUTORIAL.md) for
the user-facing integration tutorial.
See [./crates/nostr-double-ratchet/ARCHITECTURE.md](./crates/nostr-double-ratchet/ARCHITECTURE.md)
for the current owner/device architecture, event-authority split, and end-to-end usage flow.
