use std::collections::BTreeSet;

use nostr::{Event, Keys, PublicKey};
use nostr_double_ratchet_runtime::{
    AuthorizedDevice, DevicePubkey, DeviceRoster, GroupIncomingEvent, Invite, NdrRuntime,
    OwnerPubkey, RuntimeEffect, UnixSeconds, MESSAGE_EVENT_KIND,
};

#[derive(Clone)]
struct Published {
    event: Event,
    target_device_id: Option<String>,
}

fn runtime(owner: &Keys, device: &Keys) -> NdrRuntime {
    let device_id = device.public_key().to_hex();
    let invite = Invite::create_new(device.public_key(), Some(device_id.clone()), None)
        .expect("local invite");
    let runtime = NdrRuntime::new(
        device.public_key(),
        device.secret_key().to_secret_bytes(),
        device_id,
        owner.public_key(),
        None,
        Some(invite),
    );
    let _ = runtime.init().expect("runtime init");
    runtime
}

fn owner(public_key: PublicKey) -> OwnerPubkey {
    OwnerPubkey::from_bytes(public_key.to_bytes())
}

fn device(public_key: PublicKey) -> DevicePubkey {
    DevicePubkey::from_bytes(public_key.to_bytes())
}

fn roster(devices: &[&Keys], created_at: u64) -> DeviceRoster {
    DeviceRoster::new(
        UnixSeconds(created_at),
        devices
            .iter()
            .map(|keys| AuthorizedDevice::new(device(keys.public_key()), UnixSeconds(created_at)))
            .collect(),
    )
}

fn published_from_effects(effects: Vec<RuntimeEffect>, signer: &Keys) -> Vec<Published> {
    effects
        .into_iter()
        .filter_map(|event| match event {
            RuntimeEffect::PublishUnsigned(unsigned) if unsigned.pubkey == signer.public_key() => {
                unsigned.sign_with_keys(signer).ok().map(|event| Published {
                    event,
                    target_device_id: None,
                })
            }
            RuntimeEffect::PublishSigned(event) => Some(Published {
                event,
                target_device_id: None,
            }),
            RuntimeEffect::PublishSignedForInnerEvent {
                event,
                target_device_id,
                ..
            } => Some(Published {
                event,
                target_device_id,
            }),
            _ => None,
        })
        .collect()
}

fn deliver(events: &[Published], to: &NdrRuntime) -> Vec<GroupIncomingEvent> {
    deliver_with_outcome_effects(events, to).0
}

fn deliver_with_outcome_effects(
    events: &[Published],
    to: &NdrRuntime,
) -> (Vec<GroupIncomingEvent>, Vec<RuntimeEffect>) {
    let mut group_events = Vec::new();
    let mut group_effects = Vec::new();
    for published in events {
        let outer = to
            .group_handle_outer_event(&published.event)
            .expect("handle group outer");
        if outer.consumed || !outer.events.is_empty() || !outer.effects.is_empty() {
            group_events.extend(outer.events);
            group_effects.extend(outer.effects);
            continue;
        }

        let Ok(effects) = to.process_received_event(published.event.clone()) else {
            continue;
        };
        for event in effects {
            if let RuntimeEffect::EmitDecrypted {
                sender,
                sender_device,
                content,
                ..
            } = event
            {
                let outcome = to
                    .group_handle_incoming_payload_outcome(
                        content.as_bytes(),
                        sender,
                        sender_device,
                    )
                    .expect("handle group pairwise payload");
                group_events.extend(outcome.events);
                group_effects.extend(outcome.effects);
            }
        }
    }
    (group_events, group_effects)
}

#[test]
fn sender_key_group_create_syncs_to_linked_runtime_device() {
    let alice_owner = Keys::generate();
    let alice_primary_device = Keys::generate();
    let alice_linked_device = Keys::generate();
    let bob_owner = Keys::generate();
    let bob_device = Keys::generate();
    let charlie_owner = Keys::generate();
    let charlie_device = Keys::generate();

    let alice_primary = runtime(&alice_owner, &alice_primary_device);
    let alice_linked = runtime(&alice_owner, &alice_linked_device);
    let bob = runtime(&bob_owner, &bob_device);
    let charlie = runtime(&charlie_owner, &charlie_device);

    alice_primary.with_group_context(|core, _| {
        core.apply_local_roster(roster(
            &[&alice_primary_device, &alice_linked_device],
            3_000_000_010,
        ));
        core.observe_device_invite(
            owner(alice_owner.public_key()),
            alice_linked.local_invite().expect("linked invite"),
        )
        .expect("observe linked invite");
        core.observe_peer_roster(
            owner(bob_owner.public_key()),
            roster(&[&bob_device], 3_000_000_011),
        );
        core.observe_device_invite(
            owner(bob_owner.public_key()),
            bob.local_invite().expect("bob invite"),
        )
        .expect("observe bob invite");
        core.observe_peer_roster(
            owner(charlie_owner.public_key()),
            roster(&[&charlie_device], 3_000_000_012),
        );
        core.observe_device_invite(
            owner(charlie_owner.public_key()),
            charlie.local_invite().expect("charlie invite"),
        )
        .expect("observe charlie invite");
    });
    alice_linked.with_group_context(|core, _| {
        core.apply_local_roster(roster(
            &[&alice_primary_device, &alice_linked_device],
            3_000_000_010,
        ));
    });

    let initial_alice_devices =
        alice_primary.known_device_identity_pubkeys_for_owner(alice_owner.public_key());
    assert!(
        initial_alice_devices.contains(&alice_linked_device.public_key()),
        "primary runtime should know linked device after setup; devices={:?}",
        initial_alice_devices
            .iter()
            .map(PublicKey::to_hex)
            .collect::<Vec<_>>()
    );

    let seed_bob = alice_primary
        .send_text(bob_owner.public_key(), "seed bob".to_string(), None)
        .expect("seed bob");
    let seed_bob = published_from_effects(seed_bob.effects, &alice_primary_device);
    deliver(&seed_bob, &bob);
    deliver(&seed_bob, &alice_linked);

    let seed_charlie = alice_primary
        .send_text(charlie_owner.public_key(), "seed charlie".to_string(), None)
        .expect("seed charlie");
    let seed_charlie = published_from_effects(seed_charlie.effects, &alice_primary_device);
    deliver(&seed_charlie, &charlie);
    deliver(&seed_charlie, &alice_linked);

    let alice_devices =
        alice_primary.known_device_identity_pubkeys_for_owner(alice_owner.public_key());
    assert!(
        alice_devices.contains(&alice_linked_device.public_key()),
        "primary runtime should still know linked device in local roster; devices={:?}",
        alice_devices
            .iter()
            .map(PublicKey::to_hex)
            .collect::<Vec<_>>()
    );
    let linked_session_count = alice_primary.with_group_context(|core, _| {
        core.snapshot()
            .users
            .into_iter()
            .find(|user| user.owner_pubkey == owner(alice_owner.public_key()))
            .and_then(|user| {
                user.devices
                    .into_iter()
                    .find(|record| record.device_pubkey == device(alice_linked_device.public_key()))
                    .map(|record| {
                        usize::from(record.active_session.is_some())
                            + record.inactive_sessions.len()
                    })
            })
            .unwrap_or_default()
    });
    assert!(
        (1..=2).contains(&linked_session_count),
        "direct sender-copy fanout may refresh a one-way linked-device bootstrap, but should keep the session set bounded; count={linked_session_count}"
    );

    let created = alice_primary
        .create_group(
            "Alice Bob Charlie".to_string(),
            vec![bob_owner.public_key(), charlie_owner.public_key()],
        )
        .expect("create group");
    let group_events = published_from_effects(created.effects.clone(), &alice_primary_device);
    let linked_subscribed_authors = alice_linked
        .get_all_message_push_author_pubkeys()
        .into_iter()
        .collect::<BTreeSet<_>>();
    let linked_device_hex = alice_linked_device.public_key().to_hex();
    let linked_target_count = group_events
        .iter()
        .filter(|published| published.target_device_id.as_deref() == Some(&linked_device_hex))
        .count();
    assert!(
        linked_target_count >= 2,
        "group create should publish metadata and sender-key distribution to linked device; targets={:?}",
        group_events
            .iter()
            .map(|published| published.target_device_id.clone())
            .collect::<Vec<_>>()
    );

    let _ = deliver(&group_events, &bob);
    let _ = deliver(&group_events, &charlie);
    let linked_visible_events = group_events
        .iter()
        .filter(|published| {
            u32::from(published.event.kind.as_u16()) == MESSAGE_EVENT_KIND
                && linked_subscribed_authors.contains(&published.event.pubkey)
        })
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        !linked_visible_events.is_empty(),
        "group local-sibling sync must include at least one event from an author already visible to the linked device subscription; subscribed={:?} event_authors={:?}",
        linked_subscribed_authors
            .iter()
            .map(PublicKey::to_hex)
            .collect::<Vec<_>>(),
        group_events
            .iter()
            .map(|published| published.event.pubkey.to_hex())
            .collect::<Vec<_>>()
    );
    let linked_events = deliver(&linked_visible_events, &alice_linked);

    assert!(
        linked_events
            .iter()
            .any(|event| matches!(event, GroupIncomingEvent::MetadataUpdated(snapshot) if snapshot.group_id == created.outcome.group.group_id)),
        "linked device should observe group metadata"
    );
    assert!(
        alice_linked
            .group_snapshots()
            .iter()
            .any(|snapshot| snapshot.group_id == created.outcome.group.group_id),
        "linked runtime should retain the group snapshot"
    );
}

#[test]
fn sender_key_existing_member_fanouts_key_to_member_added_after_first_send() {
    let alice_owner = Keys::generate();
    let alice_device = Keys::generate();
    let bob_owner = Keys::generate();
    let bob_device = Keys::generate();
    let carol_owner = Keys::generate();
    let carol_device = Keys::generate();

    let alice = runtime(&alice_owner, &alice_device);
    let bob = runtime(&bob_owner, &bob_device);
    let carol = runtime(&carol_owner, &carol_device);

    alice.with_group_context(|core, _| {
        core.observe_peer_roster(
            owner(bob_owner.public_key()),
            roster(&[&bob_device], 3_100_000_010),
        );
        core.observe_device_invite(
            owner(bob_owner.public_key()),
            bob.local_invite().expect("bob invite"),
        )
        .expect("observe bob invite");
        core.observe_peer_roster(
            owner(carol_owner.public_key()),
            roster(&[&carol_device], 3_100_000_011),
        );
        core.observe_device_invite(
            owner(carol_owner.public_key()),
            carol.local_invite().expect("carol invite"),
        )
        .expect("observe carol invite");
    });
    bob.with_group_context(|core, _| {
        core.observe_peer_roster(
            owner(alice_owner.public_key()),
            roster(&[&alice_device], 3_100_000_012),
        );
        core.observe_device_invite(
            owner(alice_owner.public_key()),
            alice.local_invite().expect("alice invite"),
        )
        .expect("observe alice invite");
        core.observe_peer_roster(
            owner(carol_owner.public_key()),
            roster(&[&carol_device], 3_100_000_013),
        );
        core.observe_device_invite(
            owner(carol_owner.public_key()),
            carol.local_invite().expect("carol invite"),
        )
        .expect("observe carol invite");
    });
    carol.with_group_context(|core, _| {
        core.observe_peer_roster(
            owner(alice_owner.public_key()),
            roster(&[&alice_device], 3_100_000_014),
        );
        core.observe_device_invite(
            owner(alice_owner.public_key()),
            alice.local_invite().expect("alice invite"),
        )
        .expect("observe alice invite");
        core.observe_peer_roster(
            owner(bob_owner.public_key()),
            roster(&[&bob_device], 3_100_000_015),
        );
        core.observe_device_invite(
            owner(bob_owner.public_key()),
            bob.local_invite().expect("bob invite"),
        )
        .expect("observe bob invite");
    });

    let alice_seed_carol = alice
        .send_text(
            carol_owner.public_key(),
            "seed carol from alice".to_string(),
            None,
        )
        .expect("alice seeds carol");
    let alice_seed_carol = published_from_effects(alice_seed_carol.effects, &alice_device);
    deliver(&alice_seed_carol, &carol);

    let bob_seed_carol = bob
        .send_text(
            carol_owner.public_key(),
            "seed carol from bob".to_string(),
            None,
        )
        .expect("bob seeds carol");
    let bob_seed_carol = published_from_effects(bob_seed_carol.effects, &bob_device);
    deliver(&bob_seed_carol, &carol);

    let created = alice
        .create_group("Alice Bob".to_string(), vec![bob_owner.public_key()])
        .expect("create group");
    let group_id = created.outcome.group.group_id.clone();
    let create_events = published_from_effects(created.effects, &alice_device);
    let _ = deliver(&create_events, &bob);

    let bob_first = bob
        .send_group_message(&group_id, b"bob before carol".to_vec(), None)
        .expect("bob first group send");
    assert!(
        published_from_effects(bob_first.effects, &bob_device)
            .iter()
            .any(|published| published.event.pubkey != bob_device.public_key()),
        "bob's first group send should create a sender-key outer event"
    );

    let added = alice
        .add_group_members(&group_id, vec![carol_owner.public_key()])
        .expect("add carol");
    let add_events = published_from_effects(added.effects.clone(), &alice_device);
    let carol_add_events = deliver(&add_events, &carol);
    assert!(
        carol_add_events
            .iter()
            .any(|event| matches!(event, GroupIncomingEvent::MetadataUpdated(snapshot) if snapshot.group_id == group_id)),
        "new member should receive added group metadata from admin"
    );
    let (_, bob_reaction_effects) = deliver_with_outcome_effects(&add_events, &bob);
    let bob_reaction_events = published_from_effects(bob_reaction_effects, &bob_device);
    assert!(
        !bob_reaction_events.is_empty(),
        "existing sender-key member should publish its current sender-key distribution to a newly added member"
    );
    let carol_distribution_events = deliver(&bob_reaction_events, &carol);
    assert!(
        carol_distribution_events
            .iter()
            .any(|event| matches!(event, GroupIncomingEvent::MetadataUpdated(snapshot) if snapshot.group_id == group_id)),
        "new member should install existing member's sender-key distribution"
    );

    let bob_after_add = bob
        .send_group_message(&group_id, b"bob after carol".to_vec(), None)
        .expect("bob sends after carol is added");
    let bob_after_add_events = published_from_effects(bob_after_add.effects, &bob_device);
    let carol_events = deliver(&bob_after_add_events, &carol);

    assert!(
        carol_events.iter().any(|event| matches!(
            event,
            GroupIncomingEvent::Message(message)
                if message.group_id == group_id
                    && message.sender_owner == owner(bob_owner.public_key())
                    && message.body == b"bob after carol".to_vec()
        )),
        "new member must decrypt later messages from an existing member that already had a sender key"
    );
}

#[test]
fn group_member_removal_reaches_existing_member_on_subscribed_pairwise_author() {
    let alice_owner = Keys::generate();
    let alice_device = Keys::generate();
    let bob_owner = Keys::generate();
    let bob_device = Keys::generate();
    let charlie_owner = Keys::generate();
    let charlie_device = Keys::generate();

    let alice = runtime(&alice_owner, &alice_device);
    let bob = runtime(&bob_owner, &bob_device);
    let charlie = runtime(&charlie_owner, &charlie_device);

    alice.with_group_context(|core, _| {
        core.observe_peer_roster(
            owner(bob_owner.public_key()),
            roster(&[&bob_device], 3_200_000_010),
        );
        core.observe_device_invite(
            owner(bob_owner.public_key()),
            bob.local_invite().expect("bob invite"),
        )
        .expect("observe bob invite");
        core.observe_peer_roster(
            owner(charlie_owner.public_key()),
            roster(&[&charlie_device], 3_200_000_011),
        );
        core.observe_device_invite(
            owner(charlie_owner.public_key()),
            charlie.local_invite().expect("charlie invite"),
        )
        .expect("observe charlie invite");
    });
    bob.with_group_context(|core, _| {
        core.observe_peer_roster(
            owner(alice_owner.public_key()),
            roster(&[&alice_device], 3_200_000_012),
        );
        core.observe_device_invite(
            owner(alice_owner.public_key()),
            alice.local_invite().expect("alice invite"),
        )
        .expect("observe alice invite");
    });
    charlie.with_group_context(|core, _| {
        core.observe_peer_roster(
            owner(alice_owner.public_key()),
            roster(&[&alice_device], 3_200_000_013),
        );
        core.observe_device_invite(
            owner(alice_owner.public_key()),
            alice.local_invite().expect("alice invite"),
        )
        .expect("observe alice invite");
    });

    let created = alice
        .create_group(
            "Alice Bob Charlie".to_string(),
            vec![bob_owner.public_key(), charlie_owner.public_key()],
        )
        .expect("create group");
    let group_id = created.outcome.group.group_id.clone();
    let create_events = published_from_effects(created.effects, &alice_device);
    let _ = deliver(&create_events, &bob);
    let _ = deliver(&create_events, &charlie);

    let removal = alice
        .remove_group_member(&group_id, charlie_owner.public_key())
        .expect("remove charlie");
    assert_eq!(removal.snapshot.members.len(), 2);

    let removal_events = published_from_effects(removal.effects, &alice_device);
    let bob_device_hex = bob_device.public_key().to_hex();
    let bob_subscribed_authors = bob
        .get_all_message_push_author_pubkeys()
        .into_iter()
        .collect::<BTreeSet<_>>();
    let bob_visible_removal_events = removal_events
        .iter()
        .filter(|published| {
            published.target_device_id.as_deref() == Some(&bob_device_hex)
                && bob_subscribed_authors.contains(&published.event.pubkey)
        })
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        !bob_visible_removal_events.is_empty(),
        "removal metadata for Bob must be signed by a message author Bob already subscribes to; subscribed={:?} event_authors={:?} targets={:?}",
        bob_subscribed_authors
            .iter()
            .map(PublicKey::to_hex)
            .collect::<Vec<_>>(),
        removal_events
            .iter()
            .map(|published| published.event.pubkey.to_hex())
            .collect::<Vec<_>>(),
        removal_events
            .iter()
            .map(|published| published.target_device_id.clone())
            .collect::<Vec<_>>()
    );

    let bob_events = deliver(&bob_visible_removal_events, &bob);
    assert!(
        bob_events.iter().any(|event| matches!(
            event,
            GroupIncomingEvent::MetadataUpdated(snapshot)
                if snapshot.group_id == group_id
                    && snapshot.members.len() == 2
                    && !snapshot.members.contains(&owner(charlie_owner.public_key()))
        )),
        "Bob must apply Charlie's removal from Alice's metadata snapshot"
    );
}
