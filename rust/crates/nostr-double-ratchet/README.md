# nostr-double-ratchet

Minimal synchronous Double Ratchet core.

This crate is the domain layer only. It does not expose Nostr events, invite URLs, relay code,
storage, or background orchestration.

## Public API

High-level API:

- `SessionManager`

Lower-level primitives:

- `Session`
- `Invite`

Core data types:

- `MessageEnvelope`
- `InviteResponseEnvelope`
- `DeviceRoster`
- `AuthorizedDevice`
- `ProtocolContext`
- `SessionManagerSnapshot`
- `SessionState`
- `DeviceId`
- `DevicePubkey`
- `OwnerPubkey`
- `UnixSeconds`

## Design

- Payloads are opaque `Vec<u8>`.
- State transitions are explicit call/return operations.
- `ProtocolContext` carries time and randomness per call.
- Device authorization is represented by `DeviceRoster`.
- `SessionManager` owns multi-device Sesame-style state for decentralized relay input, but stays
  pure domain logic.

No compatibility layer is kept for the removed APIs.

## Basic `SessionManager` Flow

1. Construct a manager for the local device.
2. Publish the local public invite with `ensure_local_invite(...)`.
3. Apply the local roster and observe peer rosters.
4. Observe peer device invites.
5. Call `prepare_send(...)` to produce:
   - `deliveries`
   - `invite_responses`
   - `relay_gaps`
6. Feed invite responses and incoming messages back with:
   - `observe_invite_response(...)`
   - `receive(...)`
7. Persist and restore with `snapshot()` / `SessionManager::from_snapshot(...)`.

## Lower-Level Device-to-Device Usage

If you do not want the multi-device manager, use:

- `Invite` to bootstrap a pairwise session
- `Session::new_initiator(...)`
- `Session::new_responder(...)`
- `Session::plan_send(...)` / `apply_send(...)`
- `Session::plan_receive(...)` / `apply_receive(...)`

Those APIs operate only on opaque byte payloads and `MessageEnvelope`.

## Nostr Integration

Use the sibling adapter crate for Nostr conversions:

- [nostr-double-ratchet-nostr](../nostr-double-ratchet-nostr)

That crate translates:

- Nostr DM events <-> `MessageEnvelope`
- invite URLs/events <-> `Invite`
- invite response events <-> `InviteResponseEnvelope`
- roster events <-> `DeviceRoster`

## Testing

```bash
cargo test -p nostr-double-ratchet --manifest-path rust/Cargo.toml
```
