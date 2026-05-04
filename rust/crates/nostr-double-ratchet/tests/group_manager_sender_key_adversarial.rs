mod support;

use nostr::UnsignedEvent;
use nostr_double_ratchet::{
    DevicePubkey, GroupIncomingEvent, GroupPairwiseCommand, GroupPayloadCodec,
    GroupPayloadEncodeContext, GroupProtocol, GroupSenderKeyHandleResult, GroupSenderKeyMessage,
    Result, SenderKeyDistribution, SessionManager, UnixSeconds,
};
use nostr_double_ratchet_nostr::{
    group_codec::JsonGroupPayloadCodecV1, NostrGroupManager as GroupManager,
};
use support::{
    context, manager_device, manager_observe_invite_response, manager_public_device_invite,
    manager_receive_delivery, roster_for, session_manager, snapshot,
};

fn observe_matching_invite_responses(
    manager: &mut SessionManager,
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
    for response in responses
        .iter()
        .filter(|response| response.recipient == recipient)
    {
        manager_observe_invite_response(manager, &mut ctx, response)?;
    }
    Ok(())
}

fn sender_key_message_from_envelope(
    envelope: &nostr_double_ratchet::GroupSenderKeyMessageEnvelope,
) -> GroupSenderKeyMessage {
    GroupSenderKeyMessage {
        group_id: envelope.group_id.clone(),
        sender_event_pubkey: envelope.sender_event_pubkey,
        key_id: envelope.key_id,
        message_number: envelope.message_number,
        created_at: envelope.created_at,
        ciphertext: envelope.ciphertext.clone(),
    }
}

#[test]
fn sender_key_distribution_inner_pubkey_is_ignored_for_authenticated_sender_identity() -> Result<()>
{
    let alice = manager_device(40, 140);
    let bob = manager_device(41, 141);
    let carol = manager_device(42, 142);
    let codec = JsonGroupPayloadCodecV1;
    let mut alice_manager = session_manager(&alice);
    let mut bob_manager = session_manager(&bob);
    let mut alice_groups = GroupManager::new(alice.owner_pubkey);
    let mut bob_groups = GroupManager::new(bob.owner_pubkey);

    bob_manager.observe_peer_roster(alice.owner_pubkey, roster_for(&[&alice], 1_900_110_000));
    alice_manager.observe_peer_roster(bob.owner_pubkey, roster_for(&[&bob], 1_900_110_001));
    alice_manager.observe_device_invite(
        bob.owner_pubkey,
        manager_public_device_invite(&mut bob_manager, &bob, 1_900_110_002, 1_900_110_002)?,
    )?;

    let created = alice_groups.create_group_with_protocol(
        &mut alice_manager,
        &mut context(1_900_110_003, 1_900_110_003),
        "Inner pubkey ignored".to_string(),
        vec![bob.owner_pubkey],
        GroupProtocol::sender_key_v1(),
    )?;
    observe_matching_invite_responses(
        &mut bob_manager,
        &created.prepared.remote.invite_responses,
        1_900_110_004,
        1_900_110_004,
    )?;

    for delivery in &created.prepared.remote.deliveries {
        let received = manager_receive_delivery(
            &mut bob_manager,
            &mut context(1_900_110_005, 1_900_110_005),
            alice.owner_pubkey,
            delivery,
        )?
        .expect("group setup delivery");
        let payload = match codec.decode_pairwise_command(&received.payload)? {
            Some(GroupPairwiseCommand::SenderKeyDistribution { .. }) => {
                let mut event = serde_json::from_slice::<UnsignedEvent>(&received.payload)?;
                event.pubkey = carol.device_pubkey.to_nostr()?;
                serde_json::to_vec(&event)?
            }
            _ => received.payload,
        };
        bob_groups.handle_pairwise_payload(
            received.owner_pubkey,
            received.device_pubkey,
            &payload,
        )?;
    }

    let sent = alice_groups.send_message(
        &mut alice_manager,
        &mut context(1_900_110_006, 1_900_110_006),
        &created.group.group_id,
        b"authenticated sender wins".to_vec(),
    )?;
    let result = bob_groups.handle_sender_key_message(sender_key_message_from_envelope(
        &sent.remote.sender_key_messages[0],
    ))?;

    assert!(matches!(
        result,
        GroupSenderKeyHandleResult::Event(GroupIncomingEvent::Message(message))
            if message.sender_owner == alice.owner_pubkey
                && message.sender_device == Some(alice.device_pubkey)
                && message.body == b"authenticated sender wins".to_vec()
    ));
    Ok(())
}

#[test]
fn non_member_sender_key_distribution_is_rejected_without_mutating_group_state() -> Result<()> {
    let alice = manager_device(43, 143);
    let bob = manager_device(44, 144);
    let carol = manager_device(45, 145);
    let codec = JsonGroupPayloadCodecV1;
    let mut alice_manager = session_manager(&alice);
    let mut bob_manager = session_manager(&bob);
    let mut alice_groups = GroupManager::new(alice.owner_pubkey);
    let mut bob_groups = GroupManager::new(bob.owner_pubkey);

    bob_manager.observe_peer_roster(alice.owner_pubkey, roster_for(&[&alice], 1_900_111_000));
    alice_manager.observe_peer_roster(bob.owner_pubkey, roster_for(&[&bob], 1_900_111_001));
    alice_manager.observe_device_invite(
        bob.owner_pubkey,
        manager_public_device_invite(&mut bob_manager, &bob, 1_900_111_002, 1_900_111_002)?,
    )?;
    let created = alice_groups.create_group_with_protocol(
        &mut alice_manager,
        &mut context(1_900_111_003, 1_900_111_003),
        "Reject non-member key".to_string(),
        vec![bob.owner_pubkey],
        GroupProtocol::sender_key_v1(),
    )?;
    observe_matching_invite_responses(
        &mut bob_manager,
        &created.prepared.remote.invite_responses,
        1_900_111_004,
        1_900_111_004,
    )?;

    let metadata_delivery = created
        .prepared
        .remote
        .deliveries
        .iter()
        .find_map(|delivery| {
            let received = manager_receive_delivery(
                &mut bob_manager,
                &mut context(1_900_111_005, 1_900_111_005),
                alice.owner_pubkey,
                delivery,
            )
            .ok()
            .flatten()?;
            matches!(
                codec
                    .decode_pairwise_command(&received.payload)
                    .ok()
                    .flatten(),
                Some(GroupPairwiseCommand::MetadataSnapshot { .. })
            )
            .then_some(received)
        })
        .expect("metadata delivery");
    bob_groups.handle_pairwise_payload(
        metadata_delivery.owner_pubkey,
        metadata_delivery.device_pubkey,
        &metadata_delivery.payload,
    )?;
    let before = snapshot(&bob_groups.snapshot());

    let forged_distribution = SenderKeyDistribution {
        group_id: created.group.group_id.clone(),
        key_id: 99,
        sender_event_pubkey: DevicePubkey::from_bytes([99; 32]),
        chain_key: [9; 32],
        iteration: 0,
        created_at: UnixSeconds(1_900_111_006),
    };
    let forged_payload = codec.encode_pairwise_command(
        GroupPayloadEncodeContext {
            local_device_pubkey: carol.device_pubkey,
            created_at: UnixSeconds(1_900_111_006),
        },
        &GroupPairwiseCommand::SenderKeyDistribution {
            distribution: forged_distribution,
        },
    )?;

    let result = bob_groups.handle_pairwise_payload(
        carol.owner_pubkey,
        carol.device_pubkey,
        &forged_payload,
    );

    assert!(result.is_err());
    assert_eq!(snapshot(&bob_groups.snapshot()), before);
    assert!(bob_groups.known_sender_event_pubkeys().is_empty());
    Ok(())
}
