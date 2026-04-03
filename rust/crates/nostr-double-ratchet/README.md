# nostr-double-ratchet

Rust domain library for Double Ratchet messaging on Nostr.

The Rust side is intentionally synchronous and ownership-driven:

- no CLI crate
- no FFI crate
- no pubsub/runtime layer
- no storage adapters or background orchestration in the core

State lives in one place, under your control, and every transition happens through normal
function calls and return values.

## Architecture

There are two layers:

1. Domain core:
   - `ids`
   - `Session`
   - `PeerBook`
   - `Invite`
   - `AppKeys`
   - group metadata helpers

2. Explicit orchestration and adapters:
   - `NdrState`
   - `codec::nostr`

`NdrState` owns:

- local owner/device identity metadata
- local AppKeys timeline
- peer session books
- peer AppKeys timelines
- group state

It exposes synchronous methods such as:

- `upsert_peer_session(...)`
- `send_text(...)`
- `receive_direct_message(...)`
- `apply_local_app_keys(...)`
- `apply_peer_app_keys(...)`
- `create_group(...)`
- `apply_group_metadata(...)`
- `snapshot()`

No hidden subscriptions, callbacks, queues, or async tasks are involved.

## Basic Usage

```rust
use rand::{rngs::StdRng, SeedableRng};
use nostr_double_ratchet::{
    codec::nostr,
    DeviceId, DevicePubkey, IncomingDirectMessageEnvelope, IncomingInviteResponseEnvelope,
    NdrState, ProtocolContext, UnixMillis, UnixSeconds,
};

let alice_secret = [1u8; 32];
let bob_secret = [2u8; 32];

let mut rng = StdRng::seed_from_u64(7);
let mut ctx = ProtocolContext::new(
    UnixSeconds(1_700_000_000),
    UnixMillis(1_700_000_000_000),
    &mut rng,
);

let mut alice = NdrState::single_device(
    DevicePubkey::from_bytes(nostr::Keys::new(
        nostr::SecretKey::from_slice(&alice_secret)?
    ).public_key().to_bytes()),
    Some(DeviceId::new("alice-device")),
);
let mut bob = NdrState::single_device(
    DevicePubkey::from_bytes(nostr::Keys::new(
        nostr::SecretKey::from_slice(&bob_secret)?
    ).public_key().to_bytes()),
    Some(DeviceId::new("bob-device")),
);

let invite = alice.create_invite(&mut ctx, None)?;
let acceptance = bob.accept_invite(&mut ctx, &invite, bob_secret)?;
let response_event = nostr::invite_response_event(&acceptance.response)?;
let response = nostr::parse_invite_response_event(&response_event)?;
alice.process_invite_response(&mut ctx, &invite, &response, alice_secret)?;

let outbound = bob.send_text(&mut ctx, alice.owner_pubkey(), "hello".to_string())?;
let dm_event = nostr::direct_message_event(&outbound.envelope)?;
let incoming = nostr::parse_direct_message_event(&dm_event)?;
let received = alice
    .receive_direct_message(&mut ctx, bob.owner_pubkey(), None, &incoming)?
    .expect("message");

assert_eq!(received.rumor.content, "hello");
# Ok::<(), nostr_double_ratchet::Error>(())
```

## Snapshot-Driven Inspection

Use `snapshot()` when you want a stable, serializable view of the full Rust-side state:

- local owner/device keys
- authorized devices from AppKeys
- peer session books
- peer AppKeys views
- groups

This is the preferred way to inspect state instead of inferring it from emitted events.

## Security Properties

- 1:1 payloads are encrypted with Double Ratchet and NIP-44.
- Outer Nostr events are signature-verified.
- AppKeys snapshots are ordered by `created_at`, stale snapshots are ignored, and same-second
  snapshots are merged monotonically.
- Inner rumor `pubkey` is not treated as a trusted sender identity source.
- Inner rumors remain unsigned, preserving plausible deniability.

## Testing

```bash
cargo test -p nostr-double-ratchet --manifest-path rust/Cargo.toml
```
