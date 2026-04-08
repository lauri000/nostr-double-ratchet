# nostr-double-ratchet

Hard-forked Double Ratchet primitives for new applications.

The Rust side is now split into:

- `nostr-double-ratchet`: minimal synchronous domain core
- `nostr-double-ratchet-nostr`: Nostr event and invite adapter

## Rust Shape

The core crate keeps only:

- `Session`: low-level device-to-device ratchet
- `Invite`: low-level bootstrap primitive
- `SessionManager`: the only high-level multi-device API
- `GroupManager`: pairwise-fanout group-state layer above `SessionManager`
- `RosterEditor`: standalone helper for device-roster CRUD
- typed ids, rosters, snapshots, explicit errors, and `ProtocolContext`

The core does not expose:

- `NdrState`
- `PeerBook`
- `AppKeys`
- `Rumor`
- `DirectMessageContent`
- Nostr event or invite URL codecs

`SessionManager` is the supported owner/device application API. `GroupManager` sits above it when
you want pairwise-fanout groups without moving group semantics into the app. `Session` and
`Invite` stay public for direct device-to-device integrations that do not want roster or
multi-device orchestration.

## Architecture

The current fork is intentionally split into a pure domain layer and a Nostr adapter layer.

```mermaid
flowchart LR
  App["Application code"] --> Adapter["nostr-double-ratchet-nostr"]
  Adapter --> Core["nostr-double-ratchet"]

  Core --> SM["SessionManager"]
  Core --> GM["GroupManager"]
  Core --> RE["RosterEditor"]
  Core --> Low["Session / Invite"]
  Core --> Types["DeviceRoster / MessageEnvelope / InviteResponseEnvelope / snapshots"]

  Adapter --> Nostr["Nostr events / invite URLs / roster events"]
```

The core crate has no relay runtime, storage abstraction, background workers, or FFI surface.
Apps own transport, persistence, and scheduling.

## Typical Use

```mermaid
flowchart TD
  Edit["Edit full roster snapshot with RosterEditor"] --> Roster["Apply / observe local and peer rosters"]
  Roster --> Invite["Observe peer invites"]
  Invite --> Prepare["SessionManager.prepare_send(...)"]
  Prepare --> Group["optional GroupManager fanout / handle_incoming"]
  Prepare --> Deliveries["message deliveries"]
  Prepare --> Responses["invite responses"]
  Prepare --> Gaps["relay gaps"]
  Deliveries --> Encode["adapter encodes Nostr events"]
  Responses --> Encode
  Encode --> Publish["application publishes to relays"]
  Publish --> Receive["application receives relay events"]
  Receive --> Parse["adapter parses events"]
  Parse --> Apply["SessionManager.receive(...) / observe_invite_response(...)"]
```

## Session vs SessionManager

```mermaid
flowchart TD
  A["Use Session + Invite"] --> A1["direct pairwise device-to-device"]
  B["Use SessionManager"] --> B1["owner/device model"]
  C["Use GroupManager + SessionManager"] --> C1["pairwise-fanout groups"]
  B1 --> B2["always create a separate local device key"]
  B2 --> B3["even when the local roster has only one device"]
```

Recommended rule:
- direct D2D should use `Session` / `Invite` directly
- `SessionManager` should always be given a distinct local device secret key
- `GroupManager` should be layered above `SessionManager`, not inside it
- device-roster CRUD should happen outside `SessionManager`, then be applied as full snapshots

## Repository Layout

- `rust/crates/nostr-double-ratchet/`: domain core
- `rust/crates/nostr-double-ratchet-nostr/`: Nostr adapter
- `ts/`: TypeScript implementation
- `formal/`: protocol models

## Development

```bash
cargo test --manifest-path rust/Cargo.toml
cargo clippy --manifest-path rust/Cargo.toml --workspace --tests -- -D warnings
pnpm -C ts test:once
```

## Notes

- Core payloads are opaque `Vec<u8>`.
- Device authorization in the core is modeled as `DeviceRoster`.
- Nostr event kinds, URL formats, and roster-event translation live only in the adapter crate.
- State transitions are synchronous and explicit. No runtime, pubsub, storage layer, or background
  workers are built into the core.

See [rust/README.md](./rust/README.md) for the Rust workspace overview.
See [rust/crates/nostr-double-ratchet/TUTORIAL.md](./rust/crates/nostr-double-ratchet/TUTORIAL.md)
for the user-facing integration tutorial.
See [rust/crates/nostr-double-ratchet/ARCHITECTURE.md](./rust/crates/nostr-double-ratchet/ARCHITECTURE.md)
for the current owner/device architecture and usage flow.
