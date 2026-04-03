mod support;

use nostr_double_ratchet::{
    AppKeysSnapshotDecision, DirectMessageContent, IncomingInviteResponseEnvelope, Invite,
    OwnerPubkey, Result, UnixSeconds,
};
use support::{
    app_keys_for, delivery_by_target, manager_device, manager_observe_invite_response,
    manager_public_device_invite, manager_receive_delivery, manager_user_snapshot,
    observe_device_invites, prepared_targets, received_contents, restore_manager, send_message,
    session_manager, ManagerDevice,
};

fn establish_active_pair(
    alice_manager: &mut nostr_double_ratchet::SessionManager,
    alice: &ManagerDevice,
    bob_manager: &mut nostr_double_ratchet::SessionManager,
    bob: &ManagerDevice,
    base_secs: u64,
) -> Result<()> {
    alice_manager.observe_peer_app_keys(
        bob.owner_pubkey,
        app_keys_for(&[bob], base_secs),
        UnixSeconds(base_secs),
    );
    bob_manager.observe_peer_app_keys(
        alice.owner_pubkey,
        app_keys_for(&[alice], base_secs),
        UnixSeconds(base_secs),
    );

    let bob_invite = manager_public_device_invite(bob_manager, bob, base_secs + 1, base_secs + 1)?;
    let alice_invite =
        manager_public_device_invite(alice_manager, alice, base_secs + 2, base_secs + 2)?;
    alice_manager.observe_device_invite(bob.owner_pubkey, bob_invite)?;
    bob_manager.observe_device_invite(alice.owner_pubkey, alice_invite)?;

    let mut alice_send_ctx = support::context(base_secs + 3, base_secs + 3);
    let first =
        alice_manager.prepare_send_text(&mut alice_send_ctx, bob.owner_pubkey, "bootstrap".into())?;
    let mut bob_observe_ctx = support::context(base_secs + 4, base_secs + 4);
    manager_observe_invite_response(bob_manager, &mut bob_observe_ctx, &first.invite_responses[0])?;
    let mut bob_receive_ctx = support::context(base_secs + 5, base_secs + 5);
    manager_receive_delivery(
        bob_manager,
        &mut bob_receive_ctx,
        alice.owner_pubkey,
        &first.deliveries[0],
    )?;

    let mut bob_send_ctx = support::context(base_secs + 6, base_secs + 6);
    let reply =
        bob_manager.prepare_send_text(&mut bob_send_ctx, alice.owner_pubkey, "reply".into())?;
    let mut alice_receive_ctx = support::context(base_secs + 7, base_secs + 7);
    manager_receive_delivery(
        alice_manager,
        &mut alice_receive_ctx,
        bob.owner_pubkey,
        &reply.deliveries[0],
    )?;
    Ok(())
}

fn public_local_invite(
    manager: &mut nostr_double_ratchet::SessionManager,
    device: &ManagerDevice,
    seed: u64,
    now_secs: u64,
) -> Result<Invite> {
    manager_public_device_invite(manager, device, seed, now_secs)
}

fn incoming_response_from_public_invite(
    public_invite: &Invite,
    invitee: &ManagerDevice,
    owner_pubkey: Option<OwnerPubkey>,
    seed: u64,
    now_secs: u64,
) -> Result<(nostr_double_ratchet::Session, IncomingInviteResponseEnvelope)> {
    let mut accept_ctx = support::context(seed, now_secs);
    let (session, response) = public_invite.accept_with_owner(
        &mut accept_ctx,
        invitee.device_pubkey,
        invitee.secret_key,
        Some(invitee.device_id.clone()),
        owner_pubkey,
    )?;
    Ok((session, support::incoming_invite_response(&response)?))
}

#[test]
fn snapshot_roundtrip_preserves_pending_bootstrap_state() -> Result<()> {
    let alice = manager_device(41, 211, "alice-1");
    let bob = manager_device(42, 221, "bob-1");
    let mut alice_manager = session_manager(&alice, 60 * 60);
    let mut bob_manager = session_manager(&bob, 60 * 60);

    alice_manager.observe_peer_app_keys(
        bob.owner_pubkey,
        app_keys_for(&[&bob], 100),
        UnixSeconds(100),
    );
    let bob_invite = manager_public_device_invite(&mut bob_manager, &bob, 101, 101)?;
    alice_manager.observe_device_invite(bob.owner_pubkey, bob_invite)?;

    let snapshot = alice_manager.snapshot();
    let mut restored = restore_manager(&snapshot, alice.secret_key, 60 * 60)?;

    let mut send_ctx = support::context(102, 102);
    let prepared = restored.prepare_send_text(&mut send_ctx, bob.owner_pubkey, "pending".into())?;
    assert_eq!(prepared.deliveries.len(), 1);
    assert_eq!(prepared.invite_responses.len(), 1);

    let mut bob_observe_ctx = support::context(103, 103);
    manager_observe_invite_response(&mut bob_manager, &mut bob_observe_ctx, &prepared.invite_responses[0])?;
    let mut bob_receive_ctx = support::context(104, 104);
    let received = manager_receive_delivery(
        &mut bob_manager,
        &mut bob_receive_ctx,
        alice.owner_pubkey,
        &prepared.deliveries[0],
    )?
    .expect("bob should receive pending bootstrap delivery");
    assert_eq!(received.rumor.content, "pending");
    Ok(())
}

#[test]
fn snapshot_roundtrip_preserves_established_active_sessions_without_new_bootstrap() -> Result<()> {
    let alice = manager_device(43, 231, "alice-1");
    let bob = manager_device(44, 241, "bob-1");
    let mut alice_manager = session_manager(&alice, 60 * 60);
    let mut bob_manager = session_manager(&bob, 60 * 60);

    establish_active_pair(&mut alice_manager, &alice, &mut bob_manager, &bob, 110)?;

    let mut alice_manager = restore_manager(&alice_manager.snapshot(), alice.secret_key, 60 * 60)?;
    let mut bob_manager = restore_manager(&bob_manager.snapshot(), bob.secret_key, 60 * 60)?;

    let mut send_ctx = support::context(118, 118);
    let prepared =
        alice_manager.prepare_send_text(&mut send_ctx, bob.owner_pubkey, "after-restore".into())?;
    assert_eq!(prepared.deliveries.len(), 1);
    assert!(prepared.invite_responses.is_empty());

    let mut receive_ctx = support::context(119, 119);
    let received = manager_receive_delivery(
        &mut bob_manager,
        &mut receive_ctx,
        alice.owner_pubkey,
        &prepared.deliveries[0],
    )?
    .expect("bob should receive established-session message");
    assert_eq!(received.rumor.content, "after-restore");
    Ok(())
}

#[test]
fn post_restore_delayed_invite_response_establishes_sendable_session_once() -> Result<()> {
    let alice = manager_device(45, 251, "alice-1");
    let bob = manager_device(46, 252, "bob-1");
    let mut alice_manager = session_manager(&alice, 60 * 60);

    alice_manager.observe_peer_app_keys(
        bob.owner_pubkey,
        app_keys_for(&[&bob], 120),
        UnixSeconds(120),
    );
    let public_alice_invite = public_local_invite(&mut alice_manager, &alice, 121, 121)?;
    let (mut bob_session, incoming_response) = incoming_response_from_public_invite(
        &public_alice_invite,
        &bob,
        Some(bob.owner_pubkey),
        122,
        122,
    )?;

    let snapshot = alice_manager.snapshot();
    let mut restored = restore_manager(&snapshot, alice.secret_key, 60 * 60)?;

    let mut observe_ctx = support::context(123, 123);
    let observed = restored.observe_invite_response(&mut observe_ctx, &incoming_response)?;
    assert!(observed.is_some());

    let sent = send_message(
        &mut bob_session,
        &mut support::context(124, 124),
        DirectMessageContent::Text("first-from-bob".into()),
    )?;
    let mut receive_ctx = support::context(125, 125);
    let received = restored
        .receive_direct_message(&mut receive_ctx, bob.owner_pubkey, &sent.incoming)?
        .expect("alice should decrypt first post-response message");
    assert_eq!(received.rumor.content, "first-from-bob");

    let mut send_ctx = support::context(126, 126);
    let prepared = restored.prepare_send_text(&mut send_ctx, bob.owner_pubkey, "alice-reply".into())?;
    assert_eq!(prepared.deliveries.len(), 1);
    assert!(prepared.invite_responses.is_empty());

    let event = nostr_double_ratchet::codec::nostr::direct_message_event(&prepared.deliveries[0].envelope)?;
    let received_reply = support::receive_event(&mut bob_session, &mut support::context(127, 127), &event)?;
    assert_eq!(received_reply.content, "alice-reply");
    Ok(())
}

#[test]
fn post_restore_receive_then_reply_uses_existing_session_without_invite_response() -> Result<()> {
    let alice = manager_device(47, 253, "alice-1");
    let bob = manager_device(48, 254, "bob-1");
    let mut alice_manager = session_manager(&alice, 60 * 60);
    let mut bob_manager = session_manager(&bob, 60 * 60);

    establish_active_pair(&mut alice_manager, &alice, &mut bob_manager, &bob, 130)?;

    let mut bob_send_ctx = support::context(138, 138);
    let delayed =
        bob_manager.prepare_send_text(&mut bob_send_ctx, alice.owner_pubkey, "delayed".into())?;

    let mut restored = restore_manager(&alice_manager.snapshot(), alice.secret_key, 60 * 60)?;

    let mut receive_ctx = support::context(139, 139);
    let received = manager_receive_delivery(
        &mut restored,
        &mut receive_ctx,
        bob.owner_pubkey,
        &delayed.deliveries[0],
    )?
    .expect("alice should receive delayed message");
    assert_eq!(received.rumor.content, "delayed");

    let mut reply_ctx = support::context(140, 140);
    let reply = restored.prepare_send_text(&mut reply_ctx, bob.owner_pubkey, "reply-after-restore".into())?;
    assert!(reply.invite_responses.is_empty());

    let mut bob_receive_ctx = support::context(141, 141);
    let bob_received = manager_receive_delivery(
        &mut bob_manager,
        &mut bob_receive_ctx,
        alice.owner_pubkey,
        &reply.deliveries[0],
    )?
    .expect("bob should receive reply");
    assert_eq!(bob_received.rumor.content, "reply-after-restore");
    Ok(())
}

#[test]
fn peer_device_added_after_conversation_bootstraps_only_new_device() -> Result<()> {
    let alice = manager_device(49, 151, "alice-1");
    let bob1 = manager_device(50, 152, "bob-1");
    let bob2 = manager_device(50, 153, "bob-2");
    let mut alice_manager = session_manager(&alice, 60 * 60);
    let mut bob1_manager = session_manager(&bob1, 60 * 60);
    let mut bob2_manager = session_manager(&bob2, 60 * 60);

    establish_active_pair(&mut alice_manager, &alice, &mut bob1_manager, &bob1, 150)?;

    assert_eq!(
        alice_manager.observe_peer_app_keys(
            bob1.owner_pubkey,
            app_keys_for(&[&bob1, &bob2], 160),
            UnixSeconds(160),
        ),
        AppKeysSnapshotDecision::Advanced
    );
    let bob2_invite = manager_public_device_invite(&mut bob2_manager, &bob2, 161, 161)?;
    alice_manager.observe_device_invite(bob1.owner_pubkey, bob2_invite)?;

    let mut send_ctx = support::context(162, 162);
    let prepared = alice_manager.prepare_send_text(&mut send_ctx, bob1.owner_pubkey, "fanout-new-device".into())?;
    assert_eq!(prepared.deliveries.len(), 2);
    assert_eq!(prepared.invite_responses.len(), 1);

    let targets = prepared_targets(&prepared);
    assert!(targets.contains(&(bob1.owner_pubkey, bob1.device_pubkey)));
    assert!(targets.contains(&(bob2.owner_pubkey, bob2.device_pubkey)));

    let mut bob1_receive_ctx = support::context(163, 163);
    let bob1_received = manager_receive_delivery(
        &mut bob1_manager,
        &mut bob1_receive_ctx,
        alice.owner_pubkey,
        &delivery_by_target(&prepared, bob1.owner_pubkey, bob1.device_pubkey),
    )?
    .expect("bob1 should receive follow-up");
    assert_eq!(bob1_received.rumor.content, "fanout-new-device");

    let mut bob2_observe_ctx = support::context(164, 164);
    manager_observe_invite_response(&mut bob2_manager, &mut bob2_observe_ctx, &prepared.invite_responses[0])?;
    let mut bob2_receive_ctx = support::context(165, 165);
    let bob2_received = manager_receive_delivery(
        &mut bob2_manager,
        &mut bob2_receive_ctx,
        alice.owner_pubkey,
        &delivery_by_target(&prepared, bob2.owner_pubkey, bob2.device_pubkey),
    )?
    .expect("bob2 should receive new-device bootstrap");
    assert_eq!(bob2_received.rumor.content, "fanout-new-device");
    Ok(())
}

#[test]
fn local_sibling_added_after_restore_receives_self_fanout_without_rebootstrapping_peer_devices(
) -> Result<()> {
    let alice1 = manager_device(51, 154, "alice-1");
    let alice2 = manager_device(51, 155, "alice-2");
    let bob = manager_device(52, 156, "bob-1");
    let mut alice1_manager = session_manager(&alice1, 60 * 60);
    let mut alice2_manager = session_manager(&alice2, 60 * 60);
    let mut bob_manager = session_manager(&bob, 60 * 60);

    establish_active_pair(&mut alice1_manager, &alice1, &mut bob_manager, &bob, 170)?;

    let mut restored = restore_manager(&alice1_manager.snapshot(), alice1.secret_key, 60 * 60)?;
    restored.apply_local_app_keys(app_keys_for(&[&alice1, &alice2], 171), UnixSeconds(171));
    let alice2_invite = manager_public_device_invite(&mut alice2_manager, &alice2, 172, 172)?;
    restored.observe_device_invite(alice1.owner_pubkey, alice2_invite)?;

    let mut send_ctx = support::context(173, 173);
    let prepared = restored.prepare_send_text(&mut send_ctx, bob.owner_pubkey, "self-fanout".into())?;
    assert_eq!(prepared.deliveries.len(), 2);
    assert_eq!(prepared.invite_responses.len(), 1);

    let mut bob_receive_ctx = support::context(174, 174);
    let bob_received = manager_receive_delivery(
        &mut bob_manager,
        &mut bob_receive_ctx,
        alice1.owner_pubkey,
        &delivery_by_target(&prepared, bob.owner_pubkey, bob.device_pubkey),
    )?
    .expect("bob should receive direct message");
    assert_eq!(bob_received.rumor.content, "self-fanout");

    let mut alice2_observe_ctx = support::context(175, 175);
    manager_observe_invite_response(&mut alice2_manager, &mut alice2_observe_ctx, &prepared.invite_responses[0])?;
    let mut alice2_receive_ctx = support::context(176, 176);
    let sibling_received = manager_receive_delivery(
        &mut alice2_manager,
        &mut alice2_receive_ctx,
        alice1.owner_pubkey,
        &delivery_by_target(&prepared, alice1.owner_pubkey, alice2.device_pubkey),
    )?
    .expect("alice2 should receive sibling copy");
    assert_eq!(sibling_received.rumor.content, "self-fanout");
    Ok(())
}

#[test]
fn removed_then_readded_device_reuses_surviving_session_before_prune() -> Result<()> {
    let alice = manager_device(53, 157, "alice-1");
    let bob = manager_device(54, 158, "bob-1");
    let mut alice_manager = session_manager(&alice, 60 * 60);
    let mut bob_manager = session_manager(&bob, 60 * 60);

    establish_active_pair(&mut alice_manager, &alice, &mut bob_manager, &bob, 180)?;

    assert_eq!(
        alice_manager.observe_peer_app_keys(bob.owner_pubkey, app_keys_for(&[], 181), UnixSeconds(181)),
        AppKeysSnapshotDecision::Advanced
    );
    assert_eq!(
        alice_manager.observe_peer_app_keys(
            bob.owner_pubkey,
            app_keys_for(&[&bob], 182),
            UnixSeconds(182),
        ),
        AppKeysSnapshotDecision::Advanced
    );

    let mut send_ctx = support::context(183, 183);
    let prepared =
        alice_manager.prepare_send_text(&mut send_ctx, bob.owner_pubkey, "reused".into())?;
    assert_eq!(prepared.deliveries.len(), 1);
    assert!(prepared.invite_responses.is_empty());

    let mut bob_receive_ctx = support::context(184, 184);
    let received = manager_receive_delivery(
        &mut bob_manager,
        &mut bob_receive_ctx,
        alice.owner_pubkey,
        &prepared.deliveries[0],
    )?
    .expect("bob should receive reused-session message");
    assert_eq!(received.rumor.content, "reused");
    Ok(())
}

#[test]
fn equal_timestamp_appkeys_merge_after_restore_unlocks_disjoint_devices() -> Result<()> {
    let alice = manager_device(55, 159, "alice-1");
    let bob1 = manager_device(56, 160, "bob-1");
    let bob2 = manager_device(56, 161, "bob-2");
    let mut alice_manager = session_manager(&alice, 60 * 60);
    let mut bob1_manager = session_manager(&bob1, 60 * 60);
    let mut bob2_manager = session_manager(&bob2, 60 * 60);

    alice_manager.observe_peer_app_keys(
        bob1.owner_pubkey,
        app_keys_for(&[&bob1], 190),
        UnixSeconds(190),
    );
    alice_manager.observe_device_invite(
        bob1.owner_pubkey,
        manager_public_device_invite(&mut bob1_manager, &bob1, 191, 191)?,
    )?;

    let mut restored = restore_manager(&alice_manager.snapshot(), alice.secret_key, 60 * 60)?;
    assert_eq!(
        restored.observe_peer_app_keys(
            bob1.owner_pubkey,
            app_keys_for(&[&bob2], 190),
            UnixSeconds(190),
        ),
        AppKeysSnapshotDecision::MergedEqualTimestamp
    );
    let bob1_local_invite = manager_public_device_invite(&mut bob1_manager, &bob1, 191, 191)?;
    let bob2_local_invite = manager_public_device_invite(&mut bob2_manager, &bob2, 192, 192)?;
    restored.observe_device_invite(
        bob1.owner_pubkey,
        bob2_local_invite.clone(),
    )?;

    let snapshot = restored.snapshot();
    let bob_user = manager_user_snapshot(&snapshot, bob1.owner_pubkey);
    assert_eq!(bob_user.devices.len(), 2);

    let mut send_ctx = support::context(193, 193);
    let prepared =
        restored.prepare_send_text(&mut send_ctx, bob1.owner_pubkey, "merged".into())?;
    assert_eq!(prepared.deliveries.len(), 2);
    assert_eq!(prepared.invite_responses.len(), 2);

    for response in &prepared.invite_responses {
        if response.recipient == bob1_local_invite.inviter_ephemeral_public_key {
            manager_observe_invite_response(
                &mut bob1_manager,
                &mut support::context(194, 194),
                response,
            )?;
        } else if response.recipient == bob2_local_invite.inviter_ephemeral_public_key {
            manager_observe_invite_response(
                &mut bob2_manager,
                &mut support::context(195, 195),
                response,
            )?;
        }
    }

    let mut bob1_receive_ctx = support::context(196, 196);
    let bob1_received = manager_receive_delivery(
        &mut bob1_manager,
        &mut bob1_receive_ctx,
        alice.owner_pubkey,
        &delivery_by_target(&prepared, bob1.owner_pubkey, bob1.device_pubkey),
    )?
    .expect("bob1 should receive merged fanout");
    let mut bob2_receive_ctx = support::context(197, 197);
    let bob2_received = manager_receive_delivery(
        &mut bob2_manager,
        &mut bob2_receive_ctx,
        alice.owner_pubkey,
        &delivery_by_target(&prepared, bob2.owner_pubkey, bob2.device_pubkey),
    )?
    .expect("bob2 should receive merged fanout");
    assert_eq!(received_contents(&[bob1_received, bob2_received]), vec!["merged", "merged"]);
    Ok(())
}

#[test]
fn restored_stale_device_can_receive_delayed_message_before_prune_then_is_removed_after_prune(
) -> Result<()> {
    let alice = manager_device(57, 162, "alice-1");
    let bob = manager_device(58, 163, "bob-1");
    let mut alice_manager = session_manager(&alice, 10);
    let mut bob_manager = session_manager(&bob, 10);

    establish_active_pair(&mut alice_manager, &alice, &mut bob_manager, &bob, 200)?;

    let mut bob_send_ctx = support::context(208, 208);
    let delayed =
        bob_manager.prepare_send_text(&mut bob_send_ctx, alice.owner_pubkey, "late".into())?;

    alice_manager.observe_peer_app_keys(bob.owner_pubkey, app_keys_for(&[], 209), UnixSeconds(209));
    let mut restored = restore_manager(&alice_manager.snapshot(), alice.secret_key, 10)?;

    let mut receive_ctx = support::context(210, 210);
    let received = manager_receive_delivery(
        &mut restored,
        &mut receive_ctx,
        bob.owner_pubkey,
        &delayed.deliveries[0],
    )?
    .expect("restored stale device should receive delayed message");
    assert_eq!(received.rumor.content, "late");

    let report = restored.prune_stale_records(UnixSeconds(220));
    assert_eq!(report.removed_devices, vec![(bob.owner_pubkey, bob.device_pubkey)]);
    let snapshot = restored.snapshot();
    let bob_user = manager_user_snapshot(&snapshot, bob.owner_pubkey);
    assert!(bob_user.devices.is_empty());
    Ok(())
}

#[test]
fn multi_device_transcript_survives_roundtrip_of_both_managers() -> Result<()> {
    let alice1 = manager_device(59, 164, "alice-1");
    let alice2 = manager_device(59, 165, "alice-2");
    let bob1 = manager_device(60, 166, "bob-1");
    let bob2 = manager_device(60, 167, "bob-2");

    let mut alice1_manager = session_manager(&alice1, 60 * 60);
    let mut alice2_manager = session_manager(&alice2, 60 * 60);
    let mut bob1_manager = session_manager(&bob1, 60 * 60);
    let mut bob2_manager = session_manager(&bob2, 60 * 60);

    alice1_manager.apply_local_app_keys(app_keys_for(&[&alice1, &alice2], 230), UnixSeconds(230));
    bob1_manager.apply_local_app_keys(app_keys_for(&[&bob1, &bob2], 230), UnixSeconds(230));

    alice1_manager.observe_peer_app_keys(
        bob1.owner_pubkey,
        app_keys_for(&[&bob1, &bob2], 231),
        UnixSeconds(231),
    );
    bob1_manager.observe_peer_app_keys(
        alice1.owner_pubkey,
        app_keys_for(&[&alice1, &alice2], 231),
        UnixSeconds(231),
    );

    let bob1_invite = manager_public_device_invite(&mut bob1_manager, &bob1, 232, 232)?;
    let bob2_invite = manager_public_device_invite(&mut bob2_manager, &bob2, 233, 233)?;
    let alice1_invite = manager_public_device_invite(&mut alice1_manager, &alice1, 234, 234)?;
    let alice2_invite = manager_public_device_invite(&mut alice2_manager, &alice2, 235, 235)?;
    observe_device_invites(
        &mut alice1_manager,
        bob1.owner_pubkey,
        &[bob1_invite.clone(), bob2_invite.clone()],
    )?;
    observe_device_invites(
        &mut bob1_manager,
        alice1.owner_pubkey,
        &[alice1_invite.clone(), alice2_invite.clone()],
    )?;
    alice1_manager.observe_device_invite(
        alice1.owner_pubkey,
        alice2_invite.clone(),
    )?;
    bob1_manager.observe_device_invite(
        bob1.owner_pubkey,
        bob2_invite.clone(),
    )?;

    let mut alice_send_ctx = support::context(238, 238);
    let first = alice1_manager.prepare_send_text(&mut alice_send_ctx, bob1.owner_pubkey, "m1".into())?;
    for response in &first.invite_responses {
        let recipient = response.recipient;
        if recipient == bob1_invite.inviter_ephemeral_public_key {
            manager_observe_invite_response(&mut bob1_manager, &mut support::context(239, 239), response)?;
        } else if recipient == bob2_invite.inviter_ephemeral_public_key {
            manager_observe_invite_response(&mut bob2_manager, &mut support::context(240, 240), response)?;
        } else if recipient == alice2_invite.inviter_ephemeral_public_key {
            manager_observe_invite_response(&mut alice2_manager, &mut support::context(241, 241), response)?;
        }
    }

    let bob1_received = manager_receive_delivery(
        &mut bob1_manager,
        &mut support::context(242, 242),
        alice1.owner_pubkey,
        &delivery_by_target(&first, bob1.owner_pubkey, bob1.device_pubkey),
    )?
    .expect("bob1 should receive m1");
    let bob2_received = manager_receive_delivery(
        &mut bob2_manager,
        &mut support::context(243, 243),
        alice1.owner_pubkey,
        &delivery_by_target(&first, bob2.owner_pubkey, bob2.device_pubkey),
    )?
    .expect("bob2 should receive m1");
    let alice2_received = manager_receive_delivery(
        &mut alice2_manager,
        &mut support::context(244, 244),
        alice1.owner_pubkey,
        &delivery_by_target(&first, alice1.owner_pubkey, alice2.device_pubkey),
    )?
    .expect("alice2 should receive m1");

    let mut alice1_manager = restore_manager(&alice1_manager.snapshot(), alice1.secret_key, 60 * 60)?;
    let mut bob1_manager = restore_manager(&bob1_manager.snapshot(), bob1.secret_key, 60 * 60)?;
    let mut alice2_manager = restore_manager(&alice2_manager.snapshot(), alice2.secret_key, 60 * 60)?;
    let mut bob2_manager = restore_manager(&bob2_manager.snapshot(), bob2.secret_key, 60 * 60)?;

    let mut bob_send_ctx = support::context(245, 245);
    let second = bob1_manager.prepare_send_text(&mut bob_send_ctx, alice1.owner_pubkey, "m2".into())?;
    for response in &second.invite_responses {
        let recipient = response.recipient;
        if recipient == alice1_invite.inviter_ephemeral_public_key {
            manager_observe_invite_response(&mut alice1_manager, &mut support::context(246, 246), response)?;
        } else if recipient == alice2_invite.inviter_ephemeral_public_key {
            manager_observe_invite_response(&mut alice2_manager, &mut support::context(247, 247), response)?;
        } else if recipient == bob2_invite.inviter_ephemeral_public_key {
            manager_observe_invite_response(&mut bob2_manager, &mut support::context(248, 248), response)?;
        }
    }

    let alice1_received = manager_receive_delivery(
        &mut alice1_manager,
        &mut support::context(249, 249),
        bob1.owner_pubkey,
        &delivery_by_target(&second, alice1.owner_pubkey, alice1.device_pubkey),
    )?
    .expect("alice1 should receive m2");
    let alice2_received_2 = manager_receive_delivery(
        &mut alice2_manager,
        &mut support::context(250, 250),
        bob1.owner_pubkey,
        &delivery_by_target(&second, alice1.owner_pubkey, alice2.device_pubkey),
    )?
    .expect("alice2 should receive m2");
    let bob2_received_2 = manager_receive_delivery(
        &mut bob2_manager,
        &mut support::context(251, 251),
        bob1.owner_pubkey,
        &delivery_by_target(&second, bob1.owner_pubkey, bob2.device_pubkey),
    )?
    .expect("bob2 should receive m2");

    assert_eq!(
        received_contents(&[bob1_received, bob2_received, alice2_received]),
        vec!["m1", "m1", "m1"]
    );
    assert_eq!(
        received_contents(&[alice1_received, alice2_received_2, bob2_received_2]),
        vec!["m2", "m2", "m2"]
    );
    Ok(())
}
