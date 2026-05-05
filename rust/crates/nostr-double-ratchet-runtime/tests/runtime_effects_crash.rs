use std::sync::Arc;

use nostr::{Event, Keys};
use nostr_double_ratchet_runtime::{
    nostr_codec, AppKeys, DeviceEntry, InMemoryStorage, NdrRuntime, RuntimeEffect, StorageAdapter,
    INVITE_EVENT_KIND, INVITE_RESPONSE_KIND, MESSAGE_EVENT_KIND,
};
use proptest::prelude::*;

fn runtime(device: &Keys, owner: nostr::PublicKey, storage: Arc<dyn StorageAdapter>) -> NdrRuntime {
    NdrRuntime::new(
        device.public_key(),
        device.secret_key().to_secret_bytes(),
        device.public_key().to_hex(),
        owner,
        Some(storage),
        None,
    )
}

fn published_events(effects: &[RuntimeEffect]) -> Vec<Event> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            RuntimeEffect::PublishSigned(event)
            | RuntimeEffect::PublishSignedForInnerEvent { event, .. } => Some(event.clone()),
            RuntimeEffect::PublishUnsigned(unsigned) => {
                let keys = Keys::generate();
                unsigned.clone().sign_with_keys(&keys).ok()
            }
            _ => None,
        })
        .collect()
}

fn first_invite(effects: &[RuntimeEffect]) -> Event {
    published_events(effects)
        .into_iter()
        .find(|event| event.kind.as_u16() as u32 == INVITE_EVENT_KIND)
        .expect("invite event")
}

fn published_event_ids(effects: &[RuntimeEffect]) -> Vec<String> {
    published_events(effects)
        .into_iter()
        .map(|event| event.id.to_string())
        .collect()
}

fn apply_persist_effects(runtime: &NdrRuntime, effects: &[RuntimeEffect]) {
    for effect in effects {
        if let RuntimeEffect::PersistRuntimeState { key, value } = effect {
            runtime
                .persist_runtime_state(key, value.clone())
                .expect("persist runtime state");
        }
    }
}

fn prepared_send_replay_ids_for_body(body: String) -> (Vec<String>, Vec<String>) {
    let alice_owner = Keys::generate();
    let alice_device = Keys::generate();
    let bob_owner = Keys::generate();
    let bob_device = Keys::generate();
    let alice_storage = Arc::new(InMemoryStorage::new()) as Arc<dyn StorageAdapter>;
    let bob_storage = Arc::new(InMemoryStorage::new()) as Arc<dyn StorageAdapter>;

    let alice = runtime(
        &alice_device,
        alice_owner.public_key(),
        alice_storage.clone(),
    );
    let bob = runtime(&bob_device, bob_owner.public_key(), bob_storage);
    let _alice_init = alice.init().expect("alice init");
    prepare_alice_to_bob(
        &alice,
        &bob,
        bob_owner.public_key(),
        bob_device.public_key(),
    );
    let send = alice
        .send_text(bob_owner.public_key(), body, None)
        .expect("send");
    let sent_ids = published_event_ids(&send.effects);
    apply_persist_effects(&alice, &send.effects);
    drop(alice);

    let restarted = runtime(&alice_device, alice_owner.public_key(), alice_storage);
    let replay = restarted.reload_from_storage().expect("reload after crash");
    (sent_ids, published_event_ids(&replay))
}

fn prepare_alice_to_bob(
    alice: &NdrRuntime,
    bob: &NdrRuntime,
    bob_owner: nostr::PublicKey,
    bob_device: nostr::PublicKey,
) {
    let bob_init = bob.init().expect("bob init");
    let bob_invite = first_invite(&bob_init);
    alice
        .process_received_event(bob_invite)
        .expect("alice observes bob invite");
    alice
        .ingest_app_keys_snapshot(
            bob_owner,
            AppKeys::new(vec![DeviceEntry::new(bob_device, 1)]),
            1,
        )
        .expect("alice observes bob app keys");
}

#[test]
fn prepared_direct_send_persists_before_publish_and_replays_same_event_after_restart() {
    let alice_owner = Keys::generate();
    let alice_device = Keys::generate();
    let bob_owner = Keys::generate();
    let bob_device = Keys::generate();
    let alice_storage = Arc::new(InMemoryStorage::new()) as Arc<dyn StorageAdapter>;
    let bob_storage = Arc::new(InMemoryStorage::new()) as Arc<dyn StorageAdapter>;

    let alice = runtime(
        &alice_device,
        alice_owner.public_key(),
        alice_storage.clone(),
    );
    let bob = runtime(&bob_device, bob_owner.public_key(), bob_storage);
    let _alice_init = alice.init().expect("alice init");
    prepare_alice_to_bob(
        &alice,
        &bob,
        bob_owner.public_key(),
        bob_device.public_key(),
    );
    let bob_app_keys = bob
        .ingest_app_keys_snapshot(
            alice_owner.public_key(),
            AppKeys::new(vec![DeviceEntry::new(alice_device.public_key(), 1)]),
            1,
        )
        .expect("bob observes alice app keys");
    apply_persist_effects(&bob, &bob_app_keys);

    let send = alice
        .send_text(bob_owner.public_key(), "crash replay".to_string(), None)
        .expect("send");
    let persist_index = send
        .effects
        .iter()
        .position(|effect| matches!(effect, RuntimeEffect::PersistRuntimeState { .. }))
        .expect("persist effect");
    let publish_index = send
        .effects
        .iter()
        .position(|effect| {
            matches!(
                effect,
                RuntimeEffect::PublishSigned(_) | RuntimeEffect::PublishSignedForInnerEvent { .. }
            )
        })
        .expect("publish effect");
    assert!(
        persist_index < publish_index,
        "runtime must persist prepared publish before asking the app to publish"
    );

    let sent_ids = published_event_ids(&send.effects);
    assert!(
        !sent_ids.is_empty(),
        "direct send should produce signed publish effects"
    );
    apply_persist_effects(&alice, &send.effects);

    let restarted = runtime(&alice_device, alice_owner.public_key(), alice_storage);
    let replay = restarted
        .reload_from_storage()
        .expect("reload after crash before publish");
    let replayed_ids = published_event_ids(&replay);
    assert!(
        sent_ids.iter().any(|id| replayed_ids.contains(id)),
        "restart should replay the exact prepared event id without advancing the ratchet; sent={sent_ids:?} replayed={replayed_ids:?}"
    );
}

#[test]
fn inbound_decrypt_persists_before_emitting_decrypted_payload() {
    let alice_owner = Keys::generate();
    let alice_device = Keys::generate();
    let bob_owner = Keys::generate();
    let bob_device = Keys::generate();
    let alice_storage = Arc::new(InMemoryStorage::new()) as Arc<dyn StorageAdapter>;
    let bob_storage = Arc::new(InMemoryStorage::new()) as Arc<dyn StorageAdapter>;

    let alice = runtime(&alice_device, alice_owner.public_key(), alice_storage);
    let bob = runtime(&bob_device, bob_owner.public_key(), bob_storage);
    let _alice_init = alice.init().expect("alice init");
    prepare_alice_to_bob(
        &alice,
        &bob,
        bob_owner.public_key(),
        bob_device.public_key(),
    );

    let send = alice
        .send_text(
            bob_owner.public_key(),
            "persist before emit".to_string(),
            None,
        )
        .expect("send");
    let mut saw_decrypted = false;
    for event in published_events(&send.effects) {
        let effects = match bob.process_received_event(event) {
            Ok(effects) => effects,
            Err(_) => continue,
        };
        if !effects
            .iter()
            .any(|effect| matches!(effect, RuntimeEffect::EmitDecrypted { .. }))
        {
            continue;
        }
        let persist_index = effects
            .iter()
            .position(|effect| matches!(effect, RuntimeEffect::PersistRuntimeState { .. }))
            .expect("persist effect");
        let emit_index = effects
            .iter()
            .position(|effect| matches!(effect, RuntimeEffect::EmitDecrypted { .. }))
            .expect("decrypted effect");
        assert!(
            persist_index < emit_index,
            "receive state must be durable before app-visible decrypted delivery"
        );
        saw_decrypted = true;
    }
    assert!(saw_decrypted, "receiver should decrypt one message event");
}

#[test]
fn sender_key_outer_before_distribution_is_persisted_pending_work() {
    let alice_owner = Keys::generate();
    let alice_device = Keys::generate();
    let bob_owner = Keys::generate();
    let bob_device = Keys::generate();
    let alice_storage = Arc::new(InMemoryStorage::new()) as Arc<dyn StorageAdapter>;
    let bob_storage = Arc::new(InMemoryStorage::new()) as Arc<dyn StorageAdapter>;

    let alice = runtime(&alice_device, alice_owner.public_key(), alice_storage);
    let bob = runtime(&bob_device, bob_owner.public_key(), bob_storage);
    let _alice_init = alice.init().expect("alice init");
    prepare_alice_to_bob(
        &alice,
        &bob,
        bob_owner.public_key(),
        bob_device.public_key(),
    );
    let create = alice
        .create_group(
            "pending sender key".to_string(),
            vec![bob_owner.public_key()],
        )
        .expect("create group");
    let group_id = create.group.group_id.clone();

    let send = alice
        .send_group_message(&group_id, b"pending outer".to_vec(), None)
        .expect("group send");
    let outer = published_events(&send.effects)
        .into_iter()
        .find(|event| nostr_codec::parse_group_sender_key_message_event(event).is_ok())
        .expect("sender-key outer");
    let pending = bob.group_handle_outer_event(&outer);
    assert!(pending.events.is_empty());
    assert!(
        pending
            .effects
            .iter()
            .any(|effect| matches!(effect, RuntimeEffect::PersistRuntimeState { .. })),
        "unknown sender-key outer event should be stored for retry after distribution"
    );
}

#[test]
fn inbound_message_before_invite_response_is_persisted_and_drained_after_response() {
    let alice_owner = Keys::generate();
    let alice_device = Keys::generate();
    let bob_owner = Keys::generate();
    let bob_device = Keys::generate();
    let alice_storage = Arc::new(InMemoryStorage::new()) as Arc<dyn StorageAdapter>;
    let bob_storage = Arc::new(InMemoryStorage::new()) as Arc<dyn StorageAdapter>;

    let alice = runtime(&alice_device, alice_owner.public_key(), alice_storage);
    let bob = runtime(&bob_device, bob_owner.public_key(), bob_storage.clone());
    let _alice_init = alice.init().expect("alice init");
    prepare_alice_to_bob(
        &alice,
        &bob,
        bob_owner.public_key(),
        bob_device.public_key(),
    );

    let send = alice
        .send_text(
            bob_owner.public_key(),
            "message before invite response".to_string(),
            None,
        )
        .expect("first-contact send");
    let events = published_events(&send.effects);
    let invite_response = events
        .iter()
        .find(|event| event.kind.as_u16() as u32 == INVITE_RESPONSE_KIND)
        .expect("invite response")
        .clone();
    let message = events
        .iter()
        .find(|event| event.kind.as_u16() as u32 == MESSAGE_EVENT_KIND)
        .expect("message")
        .clone();

    let pending = bob
        .process_received_event(message.clone())
        .expect("message before invite response should fail closed");
    assert!(
        pending
            .iter()
            .any(|effect| matches!(effect, RuntimeEffect::PersistRuntimeState { .. })),
        "unknown first-contact message must be durably retained for retry"
    );
    assert!(
        !pending
            .iter()
            .any(|effect| matches!(effect, RuntimeEffect::EmitDecrypted { .. })),
        "message must not be app-visible before the authenticated invite response is processed"
    );
    apply_persist_effects(&bob, &pending);
    drop(bob);

    let restarted_bob = runtime(&bob_device, bob_owner.public_key(), bob_storage);
    let startup = restarted_bob
        .reload_from_storage()
        .expect("reload pending message");
    assert!(
        !startup
            .iter()
            .any(|effect| matches!(effect, RuntimeEffect::EmitDecrypted { .. })),
        "pending message should remain hidden until the invite response arrives"
    );

    let drained = restarted_bob
        .process_received_event(invite_response)
        .expect("invite response should drain pending message");
    assert!(
        drained
            .iter()
            .any(|effect| matches!(effect, RuntimeEffect::EmitDecrypted { .. })),
        "processing the invite response must retry and decrypt the retained message"
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(8))]

    #[test]
    fn prepared_direct_send_replay_preserves_event_id_for_generated_plaintext(
        body in "[a-zA-Z0-9 .,!?:;_-]{1,64}"
    ) {
        prop_assume!(!body.trim().is_empty());
        let (sent_ids, replayed_ids) = prepared_send_replay_ids_for_body(body);
        prop_assert!(!sent_ids.is_empty());
        prop_assert!(
            sent_ids.iter().any(|id| replayed_ids.contains(id)),
            "replay must include the same prepared event id; sent={sent_ids:?} replayed={replayed_ids:?}"
        );
    }
}
