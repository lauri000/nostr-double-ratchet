# nostr-double-ratchet (Rust)

Rust now consists of a single crate:

| Crate | Description |
|-------|-------------|
| [nostr-double-ratchet](./crates/nostr-double-ratchet) | Synchronous domain core with explicit state and a thin Nostr codec |

## Design Direction

The Rust side is being rewritten around one rule:

- state must be obvious

That means:

- no async runtime in the core
- no event bus
- no pubsub abstraction
- no hidden storage layer
- no background workers

The main high-level API is `NdrState`, which owns local identity state, peer state, AppKeys
timelines, and groups. Calls mutate owned state directly and return concrete results.

## Important Types

- `NdrState`: top-level owned state container
- `ProtocolContext`: explicit per-call time/randomness input
- `Session`: low-level 1:1 double-ratchet primitive
- `PeerBook`: deterministic peer-device session selection
- `Invite`: invite/bootstrap primitive
- `AppKeys`: multi-device authorization timeline
- `codec::nostr`: event and invite URL conversion layer

## Tests

```bash
cargo test --manifest-path rust/Cargo.toml
```

For more detail, see [crates/nostr-double-ratchet/README.md](./crates/nostr-double-ratchet/README.md).
