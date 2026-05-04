use std::sync::Arc;

use nostr::{Event, EventBuilder, Keys, Kind, PublicKey, Tag, Timestamp};
use nostr_double_ratchet_runtime::{
    nostr_codec, AppKeys, DeviceEntry, InMemoryStorage, NdrRuntime, RuntimeEffect, StorageAdapter,
    INVITE_EVENT_KIND, MESSAGE_EVENT_KIND,
};
use proptest::prelude::*;

fn runtime(device: &Keys, owner: PublicKey, storage: Arc<dyn StorageAdapter>) -> NdrRuntime {
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
                unsigned.clone().sign_with_keys(&Keys::generate()).ok()
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

fn apply_persist_effects(runtime: &NdrRuntime, effects: &[RuntimeEffect]) {
    for effect in effects {
        if let RuntimeEffect::PersistRuntimeState { key, value } = effect {
            runtime
                .persist_runtime_state(key, value.clone())
                .expect("persist runtime state");
        }
    }
}

fn observe_app_keys(runtime: &NdrRuntime, owner: PublicKey, device: PublicKey) {
    runtime
        .ingest_app_keys_snapshot(owner, AppKeys::new(vec![DeviceEntry::new(device, 1)]), 1)
        .expect("ingest app keys");
}

fn prepare_bidirectional_pairwise(
    alice: &NdrRuntime,
    alice_owner: PublicKey,
    alice_device: PublicKey,
    bob: &NdrRuntime,
    bob_owner: PublicKey,
    bob_device: PublicKey,
) {
    let alice_invite = first_invite(&alice.init().expect("alice init"));
    let bob_invite = first_invite(&bob.init().expect("bob init"));

    alice
        .process_received_event(bob_invite)
        .expect("alice observes bob invite");
    bob.process_received_event(alice_invite)
        .expect("bob observes alice invite");
    observe_app_keys(alice, bob_owner, bob_device);
    observe_app_keys(bob, alice_owner, alice_device);
}

fn process_setup_events_and_return_message(
    receiver: &NdrRuntime,
    effects: &[RuntimeEffect],
) -> Event {
    let mut message = None;
    for event in published_events(effects) {
        if nostr_codec::parse_message_event(&event).is_ok() {
            message = Some(event);
            continue;
        }
        let effects = receiver
            .process_received_event(event)
            .unwrap_or_else(|_| Vec::new());
        apply_persist_effects(receiver, &effects);
    }
    message.expect("message event")
}

fn emit_decrypted(
    effects: &[RuntimeEffect],
) -> Vec<(PublicKey, Option<PublicKey>, String, String)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            RuntimeEffect::EmitDecrypted {
                sender,
                conversation_owner,
                content,
                event_id,
                ..
            } => Some((
                *sender,
                *conversation_owner,
                content.clone(),
                event_id.clone().unwrap_or_default(),
            )),
            _ => None,
        })
        .collect()
}

#[test]
fn inner_pubkey_p_and_recipient_owner_do_not_affect_runtime_sender_identity() {
    let alice_owner = Keys::generate();
    let alice_device = Keys::generate();
    let bob_owner = Keys::generate();
    let bob_device = Keys::generate();
    let forged_owner = Keys::generate();
    let alice_storage = Arc::new(InMemoryStorage::new()) as Arc<dyn StorageAdapter>;
    let bob_storage = Arc::new(InMemoryStorage::new()) as Arc<dyn StorageAdapter>;
    let alice = runtime(&alice_device, alice_owner.public_key(), alice_storage);
    let bob = runtime(&bob_device, bob_owner.public_key(), bob_storage);
    prepare_bidirectional_pairwise(
        &alice,
        alice_owner.public_key(),
        alice_device.public_key(),
        &bob,
        bob_owner.public_key(),
        bob_device.public_key(),
    );

    let forged_inner = EventBuilder::new(Kind::from(14), "forged hints ignored")
        .tags(vec![
            Tag::parse(["p", forged_owner.public_key().to_hex().as_str()]).expect("p tag"),
            Tag::parse([
                "recipient-owner",
                forged_owner.public_key().to_hex().as_str(),
            ])
            .expect("recipient-owner tag"),
            Tag::parse(["pubkey", forged_owner.public_key().to_hex().as_str()])
                .expect("misleading tag"),
        ])
        .custom_created_at(Timestamp::from(1_900_100_000))
        .build(forged_owner.public_key());
    let send = bob
        .send_event(alice_owner.public_key(), forged_inner)
        .expect("send forged inner hints");

    let mut decrypted = Vec::new();
    for event in published_events(&send.effects) {
        let effects = alice
            .process_received_event(event)
            .unwrap_or_else(|_| Vec::new());
        apply_persist_effects(&alice, &effects);
        decrypted.extend(emit_decrypted(&effects));
    }

    let (sender, conversation_owner, content, _) = decrypted
        .into_iter()
        .find(|(_, _, content, _)| content.contains("forged hints ignored"))
        .expect("decrypted forged-hint message");
    assert_eq!(sender, bob_owner.public_key());
    assert_eq!(conversation_owner, None);
    assert!(content.contains(&forged_owner.public_key().to_hex()));
}

#[test]
fn duplicate_received_event_reemits_only_until_decrypted_delivery_is_acked() {
    let alice_owner = Keys::generate();
    let alice_device = Keys::generate();
    let bob_owner = Keys::generate();
    let bob_device = Keys::generate();
    let alice_storage = Arc::new(InMemoryStorage::new()) as Arc<dyn StorageAdapter>;
    let bob_storage = Arc::new(InMemoryStorage::new()) as Arc<dyn StorageAdapter>;
    let alice = runtime(&alice_device, alice_owner.public_key(), alice_storage);
    let bob = runtime(&bob_device, bob_owner.public_key(), bob_storage);
    prepare_bidirectional_pairwise(
        &alice,
        alice_owner.public_key(),
        alice_device.public_key(),
        &bob,
        bob_owner.public_key(),
        bob_device.public_key(),
    );

    let send = bob
        .send_text(
            alice_owner.public_key(),
            "dedupe delivery".to_string(),
            None,
        )
        .expect("send");
    let message = process_setup_events_and_return_message(&alice, &send.effects);

    let first = alice
        .process_received_event(message.clone())
        .expect("first receive");
    apply_persist_effects(&alice, &first);
    let first_emit = emit_decrypted(&first);
    assert_eq!(first_emit.len(), 1);

    let second = alice
        .process_received_event(message.clone())
        .expect("duplicate receive while delivery is pending");
    let second_emit = emit_decrypted(&second);
    assert_eq!(second_emit.len(), 1);
    assert_eq!(second_emit[0].3, first_emit[0].3);

    let ack = alice
        .ack_decrypted_delivery(&first_emit[0].3)
        .expect("ack decrypted");
    apply_persist_effects(&alice, &ack);
    let after_ack = alice.process_received_event(message);
    assert!(
        after_ack
            .map(|effects| emit_decrypted(&effects).is_empty())
            .unwrap_or(true),
        "acked duplicate relay events must not produce another app-visible delivery"
    );
}

#[test]
fn persisted_inbound_decrypt_replays_after_restart_until_app_ack() {
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
    prepare_bidirectional_pairwise(
        &alice,
        alice_owner.public_key(),
        alice_device.public_key(),
        &bob,
        bob_owner.public_key(),
        bob_device.public_key(),
    );

    let send = bob
        .send_text(
            alice_owner.public_key(),
            "crash-safe inbound".to_string(),
            None,
        )
        .expect("send");
    let message = process_setup_events_and_return_message(&alice, &send.effects);
    let first = alice.process_received_event(message).expect("receive");
    apply_persist_effects(&alice, &first);
    let first_emit = emit_decrypted(&first);
    assert_eq!(first_emit.len(), 1);
    drop(alice);

    let restarted = runtime(
        &alice_device,
        alice_owner.public_key(),
        alice_storage.clone(),
    );
    let replay = restarted.reload_from_storage().expect("reload");
    let replay_emit = emit_decrypted(&replay);
    assert_eq!(replay_emit.len(), 1);
    assert_eq!(replay_emit[0].3, first_emit[0].3);
    assert!(replay_emit[0].2.contains("crash-safe inbound"));

    let ack = restarted
        .ack_decrypted_delivery(&replay_emit[0].3)
        .expect("ack replayed decrypted delivery");
    apply_persist_effects(&restarted, &ack);
    drop(restarted);

    let acked_restart = runtime(&alice_device, alice_owner.public_key(), alice_storage);
    let after_ack = acked_restart
        .reload_from_storage()
        .expect("reload after ack");
    assert!(emit_decrypted(&after_ack).is_empty());
}

#[test]
fn malformed_outer_message_event_fails_closed_without_state_effects() {
    let alice_owner = Keys::generate();
    let alice_device = Keys::generate();
    let bob_device = Keys::generate();
    let alice = runtime(
        &alice_device,
        alice_owner.public_key(),
        Arc::new(InMemoryStorage::new()) as Arc<dyn StorageAdapter>,
    );
    let malformed = EventBuilder::new(Kind::from(MESSAGE_EVENT_KIND as u16), "not ndr")
        .custom_created_at(Timestamp::from(1_900_100_100))
        .sign_with_keys(&bob_device)
        .expect("signed malformed event");

    let result = alice.process_received_event(malformed);

    assert!(result.is_err());
}

#[test]
fn delayed_app_keys_and_invite_gap_survives_restart_and_drains_once() {
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
    let bob_invite = first_invite(&bob.init().expect("bob init"));

    let queued = alice
        .send_text(
            bob_owner.public_key(),
            "queued until protocol data".to_string(),
            None,
        )
        .expect("queued send");
    assert!(published_events(&queued.effects).is_empty());
    apply_persist_effects(&alice, &queued.effects);
    drop(alice);

    let restarted = runtime(
        &alice_device,
        alice_owner.public_key(),
        alice_storage.clone(),
    );
    let reload = restarted.reload_from_storage().expect("reload");
    assert!(published_events(&reload).is_empty());

    let after_app_keys = restarted
        .ingest_app_keys_snapshot(
            bob_owner.public_key(),
            AppKeys::new(vec![DeviceEntry::new(bob_device.public_key(), 1)]),
            1,
        )
        .expect("app keys");
    assert!(
        published_events(&after_app_keys).is_empty(),
        "invite is still missing, so the queued send must not publish"
    );

    let after_invite = restarted
        .process_received_event(bob_invite)
        .expect("invite drains queued send");
    let published = published_events(&after_invite);
    assert!(
        published
            .iter()
            .any(|event| nostr_codec::parse_message_event(event).is_ok()),
        "valid invite and AppKeys should retry and publish the queued send"
    );
    apply_persist_effects(&restarted, &after_invite);
    for event in &published {
        let ack = restarted
            .ack_prepared_publish(&event.id.to_string())
            .expect("ack prepared publish");
        apply_persist_effects(&restarted, &ack);
    }
    drop(restarted);

    let after_ack = runtime(&alice_device, alice_owner.public_key(), alice_storage);
    let replay = after_ack.reload_from_storage().expect("reload after ack");
    assert!(
        published_events(&replay).is_empty(),
        "acked queued send must drain once and not replay"
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(8))]

    #[test]
    fn decrypted_delivery_ack_restart_sequence_is_idempotent(
        body in "[a-zA-Z0-9 .,!?:;_-]{1,48}",
        duplicate_before_ack in any::<bool>(),
        restart_before_ack in any::<bool>(),
    ) {
        prop_assume!(!body.trim().is_empty());
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
        prepare_bidirectional_pairwise(
            &alice,
            alice_owner.public_key(),
            alice_device.public_key(),
            &bob,
            bob_owner.public_key(),
            bob_device.public_key(),
        );

        let send = bob
            .send_text(alice_owner.public_key(), body.clone(), None)
            .expect("send");
        let message = process_setup_events_and_return_message(&alice, &send.effects);
        let first = alice.process_received_event(message.clone()).expect("receive");
        apply_persist_effects(&alice, &first);
        let first_emit = emit_decrypted(&first);
        prop_assert_eq!(first_emit.len(), 1);
        prop_assert!(first_emit[0].2.contains(&body));

        if duplicate_before_ack {
            let duplicate = alice
                .process_received_event(message.clone())
                .expect("duplicate before ack");
            let duplicate_emit = emit_decrypted(&duplicate);
            prop_assert_eq!(duplicate_emit.len(), 1);
            prop_assert_eq!(duplicate_emit[0].3.as_str(), first_emit[0].3.as_str());
        }

        let active = if restart_before_ack {
            drop(alice);
            let restarted = runtime(
                &alice_device,
                alice_owner.public_key(),
                alice_storage.clone(),
            );
            let replay = restarted.reload_from_storage().expect("reload before ack");
            let replay_emit = emit_decrypted(&replay);
            prop_assert_eq!(replay_emit.len(), 1);
            prop_assert_eq!(replay_emit[0].3.as_str(), first_emit[0].3.as_str());
            restarted
        } else {
            alice
        };

        let ack = active
            .ack_decrypted_delivery(&first_emit[0].3)
            .expect("ack");
        apply_persist_effects(&active, &ack);
        let after_ack_duplicate = active.process_received_event(message);
        prop_assert!(
            after_ack_duplicate
                .map(|effects| emit_decrypted(&effects).is_empty())
                .unwrap_or(true)
        );
        drop(active);

        let after_ack_restart = runtime(&alice_device, alice_owner.public_key(), alice_storage);
        let replay = after_ack_restart.reload_from_storage().expect("reload after ack");
        prop_assert!(emit_decrypted(&replay).is_empty());
    }
}
