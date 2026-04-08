mod support;

use nostr_double_ratchet::{
    GroupIncomingEvent, GroupManager, GroupManagerSnapshot, GroupPreparedSend, OwnerPubkey,
    Result,
};
use support::{
    context, manager_device, manager_observe_invite_response, manager_public_device_invite,
    manager_receive_delivery, roster_for, session_manager, snapshot,
};

fn observe_matching_invite_responses(
    manager: &mut nostr_double_ratchet::SessionManager,
    responses: &[nostr_double_ratchet::InviteResponseEnvelope],
    seed: u64,
    now_secs: u64,
) -> Result<()> {
    let recipient = manager
        .snapshot()
        .local_invite
        .expect("local invite must exist before filtering responses")
        .inviter_ephemeral_public_key;
    let mut ctx = context(seed, now_secs);
    for response in responses.iter().filter(|response| response.recipient == recipient) {
        manager_observe_invite_response(manager, &mut ctx, response)?;
    }
    Ok(())
}

fn deliver_group_events(
    manager: &mut nostr_double_ratchet::SessionManager,
    groups: &mut GroupManager,
    sender_owner: OwnerPubkey,
    prepared: &GroupPreparedSend,
    target_device: nostr_double_ratchet::DevicePubkey,
    seed: u64,
    now_secs: u64,
) -> Result<Vec<GroupIncomingEvent>> {
    let mut ctx = context(seed, now_secs);
    let mut events = Vec::new();

    for delivery in prepared
        .deliveries
        .iter()
        .filter(|delivery| delivery.device_pubkey == target_device)
    {
        if let Some(received) =
            manager_receive_delivery(manager, &mut ctx, sender_owner, delivery)?
        {
            if let Some(event) = groups.handle_incoming(received.owner_pubkey, &received.payload)? {
                events.push(event);
            }
        }
    }

    Ok(events)
}

#[test]
fn create_group_creates_local_state_and_snapshot_roundtrip() -> Result<()> {
    let alice = manager_device(1, 11);
    let bob = manager_device(2, 21);
    let mut manager = session_manager(&alice);
    let mut groups = GroupManager::new(alice.owner_pubkey);

    let mut create_ctx = context(1, 1_900_000_000);
    let created =
        groups.create_group(&mut manager, &mut create_ctx, "Friends".to_string(), vec![bob.owner_pubkey])?;

    assert!(!created.group.group_id.is_empty());
    assert_eq!(created.group.name, "Friends");
    assert_eq!(created.group.created_by, alice.owner_pubkey);
    assert_eq!(created.group.members, vec![alice.owner_pubkey, bob.owner_pubkey]);
    assert_eq!(created.group.admins, vec![alice.owner_pubkey]);
    assert_eq!(created.group.revision, 1);
    assert_eq!(created.prepared.group_id, created.group.group_id);
    assert_eq!(
        created.prepared.relay_gaps,
        vec![nostr_double_ratchet::RelayGap::MissingRoster {
            owner_pubkey: bob.owner_pubkey,
        }]
    );

    let group_snapshot = groups.snapshot();
    let restored = GroupManager::from_snapshot(serde_json::from_str::<GroupManagerSnapshot>(
        &snapshot(&group_snapshot),
    )?)?;
    assert_eq!(restored.group(&created.group.group_id), Some(created.group.clone()));
    assert_eq!(restored.groups(), vec![created.group]);
    Ok(())
}

#[test]
fn add_members_bootstraps_new_member_with_current_group_state() -> Result<()> {
    let alice = manager_device(3, 31);
    let bob = manager_device(4, 41);

    let mut alice_manager = session_manager(&alice);
    let mut bob_manager = session_manager(&bob);
    let mut alice_groups = GroupManager::new(alice.owner_pubkey);
    let mut bob_groups = GroupManager::new(bob.owner_pubkey);

    let mut create_ctx = context(2, 1_900_000_100);
    let created =
        alice_groups.create_group(&mut alice_manager, &mut create_ctx, "Crew".to_string(), vec![])?;

    bob_manager.observe_peer_roster(alice.owner_pubkey, roster_for(&[&alice], 10));
    alice_manager.observe_peer_roster(bob.owner_pubkey, roster_for(&[&bob], 11));
    alice_manager.observe_device_invite(
        bob.owner_pubkey,
        manager_public_device_invite(&mut bob_manager, &bob, 3, 1_900_000_101)?,
    )?;

    let mut add_ctx = context(4, 1_900_000_102);
    let prepared =
        alice_groups.add_members(&mut alice_manager, &mut add_ctx, &created.group.group_id, vec![bob.owner_pubkey])?;

    observe_matching_invite_responses(&mut bob_manager, &prepared.invite_responses, 5, 1_900_000_103)?;
    let events = deliver_group_events(
        &mut bob_manager,
        &mut bob_groups,
        alice.owner_pubkey,
        &prepared,
        bob.device_pubkey,
        6,
        1_900_000_104,
    )?;

    assert_eq!(events.len(), 1);
    match &events[0] {
        GroupIncomingEvent::MetadataUpdated(snapshot) => {
            assert_eq!(snapshot.group_id, created.group.group_id);
            assert_eq!(snapshot.name, "Crew");
            assert_eq!(snapshot.revision, 2);
            let members: std::collections::BTreeSet<_> = snapshot.members.iter().copied().collect();
            assert_eq!(
                members,
                std::collections::BTreeSet::from([alice.owner_pubkey, bob.owner_pubkey])
            );
            assert_eq!(snapshot.admins, vec![alice.owner_pubkey]);
        }
        other => panic!("expected metadata update, got {other:?}"),
    }

    assert_eq!(
        bob_groups.group(&created.group.group_id).expect("group created on new member").revision,
        2
    );
    Ok(())
}

#[test]
fn create_and_send_message_fan_out_to_remote_member_and_local_sibling() -> Result<()> {
    let alice1 = manager_device(5, 51);
    let alice2 = manager_device(5, 52);
    let bob = manager_device(6, 61);

    let mut alice1_manager = session_manager(&alice1);
    let mut alice2_manager = session_manager(&alice2);
    let mut bob_manager = session_manager(&bob);

    let mut alice1_groups = GroupManager::new(alice1.owner_pubkey);
    let mut alice2_groups = GroupManager::new(alice2.owner_pubkey);
    let mut bob_groups = GroupManager::new(bob.owner_pubkey);

    alice1_manager.apply_local_roster(roster_for(&[&alice1, &alice2], 20));
    alice2_manager.apply_local_roster(roster_for(&[&alice1, &alice2], 20));
    bob_manager.observe_peer_roster(alice1.owner_pubkey, roster_for(&[&alice1], 20));
    alice1_manager.observe_device_invite(
        alice1.owner_pubkey,
        manager_public_device_invite(&mut alice2_manager, &alice2, 20, 1_900_000_200)?,
    )?;
    alice1_manager.observe_peer_roster(bob.owner_pubkey, roster_for(&[&bob], 21));
    alice1_manager.observe_device_invite(
        bob.owner_pubkey,
        manager_public_device_invite(&mut bob_manager, &bob, 21, 1_900_000_201)?,
    )?;

    let mut create_ctx = context(7, 1_900_000_202);
    let created = alice1_groups.create_group(
        &mut alice1_manager,
        &mut create_ctx,
        "Sidecar".to_string(),
        vec![bob.owner_pubkey],
    )?;

    observe_matching_invite_responses(&mut alice2_manager, &created.prepared.invite_responses, 8, 1_900_000_203)?;
    observe_matching_invite_responses(&mut bob_manager, &created.prepared.invite_responses, 9, 1_900_000_204)?;

    let alice2_events = deliver_group_events(
        &mut alice2_manager,
        &mut alice2_groups,
        alice1.owner_pubkey,
        &created.prepared,
        alice2.device_pubkey,
        10,
        1_900_000_205,
    )?;
    let bob_events = deliver_group_events(
        &mut bob_manager,
        &mut bob_groups,
        alice1.owner_pubkey,
        &created.prepared,
        bob.device_pubkey,
        11,
        1_900_000_206,
    )?;
    assert!(matches!(alice2_events.as_slice(), [GroupIncomingEvent::MetadataUpdated(_)]));
    assert!(matches!(bob_events.as_slice(), [GroupIncomingEvent::MetadataUpdated(_)]));

    let mut send_ctx = context(12, 1_900_000_207);
    let sent = alice1_groups.send_message(
        &mut alice1_manager,
        &mut send_ctx,
        &created.group.group_id,
        b"hello-group".to_vec(),
    )?;

    let alice2_messages = deliver_group_events(
        &mut alice2_manager,
        &mut alice2_groups,
        alice1.owner_pubkey,
        &sent,
        alice2.device_pubkey,
        13,
        1_900_000_208,
    )?;
    let bob_messages = deliver_group_events(
        &mut bob_manager,
        &mut bob_groups,
        alice1.owner_pubkey,
        &sent,
        bob.device_pubkey,
        14,
        1_900_000_209,
    )?;

    assert_eq!(alice2_messages.len(), 1);
    assert_eq!(bob_messages.len(), 1);
    match &alice2_messages[0] {
        GroupIncomingEvent::Message(message) => {
            assert_eq!(message.group_id, created.group.group_id);
            assert_eq!(message.sender_owner, alice1.owner_pubkey);
            assert_eq!(message.body, b"hello-group".to_vec());
            assert_eq!(message.revision, 1);
        }
        other => panic!("expected group message, got {other:?}"),
    }
    match &bob_messages[0] {
        GroupIncomingEvent::Message(message) => {
            assert_eq!(message.group_id, created.group.group_id);
            assert_eq!(message.sender_owner, alice1.owner_pubkey);
            assert_eq!(message.body, b"hello-group".to_vec());
            assert_eq!(message.revision, 1);
        }
        other => panic!("expected group message, got {other:?}"),
    }

    assert_eq!(
        alice2_groups.group(&created.group.group_id).expect("local sibling has group").revision,
        1
    );
    assert_eq!(
        bob_groups.group(&created.group.group_id).expect("remote member has group").revision,
        1
    );
    Ok(())
}

#[test]
fn send_message_merges_relay_gaps_from_members_without_transport_state() -> Result<()> {
    let alice = manager_device(7, 71);
    let bob = manager_device(8, 81);
    let carol = manager_device(9, 91);
    let mut manager = session_manager(&alice);
    let mut groups = GroupManager::new(alice.owner_pubkey);

    let mut create_ctx = context(15, 1_900_000_300);
    let created = groups.create_group(
        &mut manager,
        &mut create_ctx,
        "Gaps".to_string(),
        vec![bob.owner_pubkey, carol.owner_pubkey],
    )?;
    assert_eq!(created.prepared.relay_gaps.len(), 2);

    manager.observe_peer_roster(bob.owner_pubkey, roster_for(&[&bob], 31));
    let mut send_ctx = context(16, 1_900_000_301);
    let prepared =
        groups.send_message(&mut manager, &mut send_ctx, &created.group.group_id, b"gap".to_vec())?;

    let expected = vec![
        nostr_double_ratchet::RelayGap::MissingRoster {
            owner_pubkey: carol.owner_pubkey,
        },
        nostr_double_ratchet::RelayGap::MissingDeviceInvite {
            owner_pubkey: bob.owner_pubkey,
            device_pubkey: bob.device_pubkey,
        },
    ];
    assert_eq!(prepared.relay_gaps, expected);
    Ok(())
}
