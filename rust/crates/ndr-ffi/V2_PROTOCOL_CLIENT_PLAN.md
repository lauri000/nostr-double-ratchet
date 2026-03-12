# V2 ProtocolClient Plan (ndr-ffi + nostr-double-ratchet)

## Status

Draft for incremental implementation.

## Scope (Current Step)

Define the new v2 FFI-facing interface shape and the matching core runtime layout.

This step is planning/documentation only.

## High-Level API Shape

```text
ProtocolClient
├─ app_keys: AppKeysApi
├─ sessions: SessionsApi            (deferred for now)
└─ runtime: RuntimeApi
   ├─ nostr methods
   └─ storage methods
```

For initial v2 implementation, `SessionsApi` is omitted/stubbed.
`RuntimeApi` is the single host callback ingress/egress boundary.

## AppKeysApi (v2)

`AppKeysApi` is a focused API for device key lifecycle.

```text
AppKeysApi
├─ start()
├─ add_device_key(...)
├─ update_device_key(...)
├─ remove_device_key(...)
├─ trust_device_key(...)
├─ revoke_device_key(...)
├─ list_device_keys(...)
└─ get_device_key(...)
```

### Behavioral intent

- `start()` initializes local runtime state needed for app key handling.
- `add/update/remove` mutate tracked device-key records.
- `trust/revoke` mutate trust state without deleting historical record.
- `list/get` are read paths over current normalized state.

## RuntimeApi (v2)

`RuntimeApi` is the host-driven ingress/egress boundary for all external runtime events (Nostr + storage).

```text
RuntimeApi
├─ start()
├─ Nostr methods
│  ├─ on_subscription_opened(request_id, sub_id)
│  ├─ on_subscription_closed(sub_id)
│  ├─ on_nostr_event(sub_id, event)
│  └─ on_publish_result(correlation_id, result)   // optional
├─ Storage methods
│  ├─ on_storage_read_result(correlation_id, result)
│  ├─ on_storage_write_result(correlation_id, result)
│  └─ on_storage_delete_result(correlation_id, result)
└─ drain_effects()
```

### Behavioral intent

- All runtime callbacks are host -> Rust ingress (`on_*` methods).
- `drain_effects()` is Rust -> host egress (pull-based effect queue).
- Nostr `sub_id` values stay in runtime/router layer, not in domain structs.
- Storage completion callbacks are normalized by runtime and routed to domain targets.

## Target Core Runtime Layout (nostr-double-ratchet v2)

```text
nostr-double-ratchet
├─ app_keys_manager: AppKeysManager
├─ session_manager: SessionManager
├─ subscription_router: SubscriptionRouter
└─ effect_queue: VecDeque<HostEffect>
```

### Roles

- `AppKeysManager`: owns app-keys domain logic and state transitions.
- `SessionManager`: owns session domain logic (later step).
- `SubscriptionRouter`: maps transport callbacks (`sub_id`, request ids) to logical targets.
- `effect_queue`: single outbound queue consumed by `drain_effects()`.

## Execution Model (v2 initial)

- Fully serialized event handling (single mutable runtime owner).
- `RuntimeApi` is the only mutable ingress for host callbacks.
- All host callbacks enqueue/route into serialized processing.
- Domain emits explicit effects; runtime stores them in `effect_queue`.
- Host drains effects explicitly; no direct re-entrant host callbacks from domain.

## Implementation Slices

1. Create `ndr-ffi` surface types under the `v2` namespace:
   - `ProtocolClient`
   - `AppKeysApi`
   - `RuntimeApi`
2. Define runtime effect protocol:
   - `HostEffect` categories (nostr + storage + app-facing)
   - `drain_effects()` behavior and ordering guarantees
3. Build runtime shell first (component 1):
   - runtime queue + correlation id tracking
   - nostr callback stubs
   - storage callback stubs
4. Wire `AppKeysApi` to `nostr-double-ratchet` v2 `AppKeysManager` (component 2).
5. Add `subscription_router` and logical routing maps (component 3).
6. Add storage request/response routing through runtime (component 4).
7. Defer `SessionsApi` and `SessionManager` integration to a later slice (component 5+).

## Component-By-Component Order

1. `RuntimeApi` skeleton and effect queue contract.
2. `AppKeysApi` to `v2::AppKeysManager` state transitions.
3. Nostr routing (`sub_id`/request id maps).
4. Storage routing (correlation id/result handling).
5. `SessionsApi` placeholder -> real integration.

## v2 Dependency Boundary

`ndr-ffi/src/v2/*` must not depend on old transport/storage integration contracts.

Specifically:

- Do not use legacy `NostrPubSub`.
- Do not use legacy `StorageAdapter`.
- Do not instantiate managers through old adapter-based constructors.

Instead, v2 wiring should be:

- runtime receives normalized host callbacks,
- runtime routes explicit inputs to v2 managers,
- managers return explicit effects/intents,
- runtime enqueues host effects for `drain_effects()`.

## Runtime <-> AppKeysManager Plan (No Legacy Adapters)

1. Add v2 input/effect contracts in `nostr-double-ratchet/src/v2`:
   - `AppKeysInput`
   - `AppKeysEffect`
2. Refactor `AppKeysManager` to process explicit inputs and return explicit effects:
   - no hidden transport/storage side effects
   - no direct host callback dependencies
3. Runtime owns I/O lifecycle:
   - maps subscription/publish/storage correlation ids
   - feeds callback results back as inputs
   - emits host-facing effects via queue
4. `on_nostr_event` runtime flow:
   - resolve route by `sub_id`
   - map to `AppKeysInput::NostrEvent`
   - apply manager
   - enqueue resulting host effects
5. `AppKeysApi` methods call manager through runtime-owned serialized context:
   - `add/update/remove/trust/revoke/list/get`
   - each method emits effects through the same runtime queue path

## Non-Goals (This Slice)

- No session-manager rewrite yet.
- No concurrent/sharded execution.
- No attempt to preserve old `ndr-ffi` object graph in v2.

## Notes

- v1 APIs stay intact during v2 bring-up.
- v2 should live under explicit `/v2` modules/folders in both crates.
- New code should prefer explicit input/effect routing so later sharding is possible.
