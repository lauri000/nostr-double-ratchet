# nostr-double-ratchet

End-to-end encrypted messaging primitives for Nostr, implemented in TypeScript and Rust.

## Current Rust Direction

The Rust side is now a single-threaded protocol core with an explicit Nostr codec boundary.

It is centered on:

- explicit ownership
- synchronous function call/return flow
- inspectable state snapshots
- no runtime/pubsub/event-driven core
- no hidden clock or randomness in state transitions

The main Rust entry point is `NdrState`, which owns peer session books, AppKeys timelines, and
group metadata state. Raw `nostr::Event` and invite URL handling live under
`codec::nostr`.

## Rust Architecture

The Rust core has two layers:

1. Domain modules:
   - `ids`
   - `Session`
   - `peer_book`
   - `Invite`
   - `AppKeys`
   - group metadata helpers

2. High-level owned state:
   - `NdrState`

3. Thin adapters:
   - `codec::nostr`

`NdrState` keeps the full Rust-side model in memory and exposes direct methods like:

- `upsert_peer_session(...)`
- `send_text(...)`
- `receive_direct_message(...)`
- `apply_local_app_keys(...)`
- `apply_peer_app_keys(...)`
- `create_group(...)`
- `apply_group_metadata(...)`
- `snapshot()`

## Repository Layout

- `ts/`: TypeScript implementation
- `rust/crates/nostr-double-ratchet/`: Rust core library
- `formal/`: TLA+ models for protocol rules and invariants

## Development

```bash
# Rust
cargo test --manifest-path rust/Cargo.toml

# TypeScript
pnpm -C ts test:once
```

## Security Notes

- 1:1 payloads use Double Ratchet over NIP-44.
- Outer Nostr events are signed and verified.
- AppKeys snapshots are treated as an authorization timeline, not a plain set.
- Inner rumor `pubkey` is not trusted for sender attribution.
- Inner rumors are unsigned, preserving plausible deniability.

For Rust-specific usage, see [rust/crates/nostr-double-ratchet/README.md](./rust/crates/nostr-double-ratchet/README.md).
