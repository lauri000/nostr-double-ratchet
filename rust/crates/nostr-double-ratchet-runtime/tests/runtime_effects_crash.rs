use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use nostr::{Event, Keys};
use nostr_double_ratchet_runtime::{
    nostr_codec, AppKeys, DeviceEntry, Error as NdrError, InMemoryStorage, NdrRuntime,
    RuntimeEffect, StorageAdapter, INVITE_EVENT_KIND, INVITE_RESPONSE_KIND, MESSAGE_EVENT_KIND,
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

#[derive(Clone)]
struct FailNextPutStorage {
    inner: InMemoryStorage,
    fail_next_put: Arc<AtomicBool>,
}

impl FailNextPutStorage {
    fn new() -> Self {
        Self {
            inner: InMemoryStorage::new(),
            fail_next_put: Arc::new(AtomicBool::new(false)),
        }
    }

    fn fail_next_put(&self) {
        self.fail_next_put.store(true, Ordering::SeqCst);
    }
}

impl StorageAdapter for FailNextPutStorage {
    fn get(&self, key: &str) -> nostr_double_ratchet_runtime::Result<Option<String>> {
        self.inner.get(key)
    }

    fn put(&self, key: &str, value: String) -> nostr_double_ratchet_runtime::Result<()> {
        if self.fail_next_put.swap(false, Ordering::SeqCst) {
            return Err(NdrError::Storage("injected write failure".to_string()));
        }
        self.inner.put(key, value)
    }

    fn del(&self, key: &str) -> nostr_double_ratchet_runtime::Result<()> {
        self.inner.del(key)
    }

    fn list(&self, prefix: &str) -> nostr_double_ratchet_runtime::Result<Vec<String>> {
        self.inner.list(prefix)
    }
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
    bob.ingest_app_keys_snapshot(
        alice_owner.public_key(),
        AppKeys::new(vec![DeviceEntry::new(alice_device.public_key(), 1)]),
        1,
    )
    .expect("bob observes alice app keys");

    let send = alice
        .send_text(bob_owner.public_key(), "crash replay".to_string(), None)
        .expect("send");
    assert!(
        send.effects
            .iter()
            .all(|effect| !matches!(effect, RuntimeEffect::EmitDecrypted { .. })),
        "direct send should not emit app-visible decrypted payloads"
    );

    let sent_ids = published_event_ids(&send.effects);
    assert!(
        !sent_ids.is_empty(),
        "direct send should produce signed publish effects"
    );
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
fn prepared_direct_send_storage_failure_returns_no_publish_and_rolls_back_live_state() {
    let alice_owner = Keys::generate();
    let alice_device = Keys::generate();
    let bob_owner = Keys::generate();
    let bob_device = Keys::generate();
    let alice_storage = Arc::new(FailNextPutStorage::new());
    let alice_storage_dyn = alice_storage.clone() as Arc<dyn StorageAdapter>;
    let bob_storage = Arc::new(InMemoryStorage::new()) as Arc<dyn StorageAdapter>;

    let alice = runtime(&alice_device, alice_owner.public_key(), alice_storage_dyn);
    let bob = runtime(&bob_device, bob_owner.public_key(), bob_storage);
    let _alice_init = alice.init().expect("alice init");
    prepare_alice_to_bob(
        &alice,
        &bob,
        bob_owner.public_key(),
        bob_device.public_key(),
    );

    alice_storage.fail_next_put();
    let failed = alice.send_text(
        bob_owner.public_key(),
        "must not publish after failed persist".to_string(),
        None,
    );
    assert!(failed.is_err(), "storage failure must fail the send");
    assert!(
        alice.prepared_publish_effects().is_empty(),
        "failed persistence must roll back staged prepared publishes"
    );

    let retry = alice
        .send_text(
            bob_owner.public_key(),
            "must publish after retry".to_string(),
            None,
        )
        .expect("retry after failed persist");
    assert!(
        !published_event_ids(&retry.effects).is_empty(),
        "retry after rollback should produce publish effects"
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
    let bob = runtime(&bob_device, bob_owner.public_key(), bob_storage.clone());
    let _alice_init = alice.init().expect("alice init");
    prepare_alice_to_bob(
        &alice,
        &bob,
        bob_owner.public_key(),
        bob_device.public_key(),
    );
    bob.ingest_app_keys_snapshot(
        alice_owner.public_key(),
        AppKeys::new(vec![DeviceEntry::new(alice_device.public_key(), 1)]),
        1,
    )
    .expect("bob verifies alice device roster");

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
        let restarted_bob = runtime(&bob_device, bob_owner.public_key(), bob_storage);
        let replay = restarted_bob
            .reload_from_storage()
            .expect("reload after app-visible decrypted delivery before app ack");
        assert!(
            replay
                .iter()
                .any(|effect| matches!(effect, RuntimeEffect::EmitDecrypted { .. })),
            "receive state and pending app-visible delivery must already be durable"
        );
        saw_decrypted = true;
        break;
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
    let pending = bob
        .group_handle_outer_event(&outer)
        .expect("queue pending group outer");
    assert!(pending.events.is_empty());
    assert!(
        pending.effects.is_empty(),
        "unknown sender-key outer should be persisted internally without app persistence effects"
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
        !pending
            .iter()
            .any(|effect| matches!(effect, RuntimeEffect::EmitDecrypted { .. })),
        "message must not be app-visible before the authenticated invite response is processed"
    );
    assert!(
        pending
            .iter()
            .any(|effect| matches!(effect, RuntimeEffect::FetchBackfill)),
        "retained inbound messages should request protocol backfill"
    );
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

    let after_response = restarted_bob
        .process_received_event(invite_response)
        .expect("invite response should retain pending message until AppKeys verify owner claim");
    assert!(
        !after_response
            .iter()
            .any(|effect| matches!(effect, RuntimeEffect::EmitDecrypted { .. })),
        "invite response owner claims must not make retained messages app-visible before AppKeys verification"
    );
    assert!(
        after_response
            .iter()
            .any(|effect| matches!(effect, RuntimeEffect::FetchBackfill)),
        "unverified invite response owner claims should request AppKeys backfill"
    );

    let drained = restarted_bob
        .ingest_app_keys_snapshot(
            alice_owner.public_key(),
            AppKeys::new(vec![DeviceEntry::new(alice_device.public_key(), 1)]),
            2,
        )
        .expect("AppKeys should verify owner claim and drain pending message");
    assert!(
        drained
            .iter()
            .any(|effect| matches!(effect, RuntimeEffect::EmitDecrypted { .. })),
        "verified AppKeys must retry and decrypt the retained message"
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
