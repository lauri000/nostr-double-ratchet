mod support;

use nostr_double_ratchet::{
    AppKeysSnapshotDecision, DeviceRecordSnapshot, DomainError, Error, Invite, RelayGap, Result,
    SessionManagerSnapshot, UnixSeconds,
};
use support::{
    app_keys_for, context, incoming_invite_response, manager_device, manager_receive_delivery,
    mutate_text, public_invite_via_event, restore_manager, session_manager, snapshot,
    ManagerDevice,
};

fn public_device_invite(
    manager: &mut nostr_double_ratchet::SessionManager,
    device: &ManagerDevice,
    seed: u64,
    now_secs: u64,
) -> Result<Invite> {
    let mut ctx = context(seed, now_secs);
    let invite = manager.ensure_local_device_invite(&mut ctx)?.clone();
    public_invite_via_event(&invite, device.secret_key)
}

fn user_snapshot(
    snapshot: &SessionManagerSnapshot,
    owner_pubkey: nostr_double_ratchet::OwnerPubkey,
) -> &nostr_double_ratchet::UserRecordSnapshot {
    snapshot
        .users
        .iter()
        .find(|user| user.owner_pubkey == owner_pubkey)
        .expect("owner snapshot must exist")
}

fn device_snapshot(
    user: &nostr_double_ratchet::UserRecordSnapshot,
    device_pubkey: nostr_double_ratchet::DevicePubkey,
) -> &DeviceRecordSnapshot {
    user.devices
        .iter()
        .find(|device| device.device_pubkey == device_pubkey)
        .expect("device snapshot must exist")
}

#[test]
fn missing_app_keys_surfaces_gap_not_hidden_failure() -> Result<()> {
    let alice = manager_device(21, 211, "alice-1");
    let bob = manager_device(22, 221, "bob-1");
    let mut alice_manager = session_manager(&alice, 60 * 60);
    let mut bob_manager = session_manager(&bob, 60 * 60);

    alice_manager.observe_device_invite(
        bob.owner_pubkey,
        public_device_invite(&mut bob_manager, &bob, 1, 1_810_000_001)?,
    )?;

    let mut send_ctx = context(2, 1_810_000_002);
    let prepared =
        alice_manager.prepare_send_text(&mut send_ctx, bob.owner_pubkey, "gap".to_string())?;
    assert!(prepared.deliveries.is_empty());
    assert_eq!(
        prepared.relay_gaps,
        vec![RelayGap::MissingAppKeys {
            owner_pubkey: bob.owner_pubkey
        }]
    );
    Ok(())
}

#[test]
fn missing_device_invite_surfaces_gap_not_hidden_failure() -> Result<()> {
    let alice = manager_device(23, 231, "alice-1");
    let bob = manager_device(24, 241, "bob-1");
    let mut alice_manager = session_manager(&alice, 60 * 60);

    assert_eq!(
        alice_manager.observe_peer_app_keys(
            bob.owner_pubkey,
            app_keys_for(&[&bob], 3),
            UnixSeconds(3),
        ),
        AppKeysSnapshotDecision::Advanced
    );

    let mut send_ctx = context(4, 1_810_000_004);
    let prepared =
        alice_manager.prepare_send_text(&mut send_ctx, bob.owner_pubkey, "gap".to_string())?;
    assert!(prepared.deliveries.is_empty());
    assert_eq!(
        prepared.relay_gaps,
        vec![RelayGap::MissingDeviceInvite {
            owner_pubkey: bob.owner_pubkey,
            device_pubkey: bob.device_pubkey,
            device_id: None,
        }]
    );
    Ok(())
}

#[test]
fn failed_bootstrap_for_one_device_does_not_corrupt_other_device_records() -> Result<()> {
    let alice = manager_device(25, 251, "alice-1");
    let bob1 = manager_device(26, 61, "bob-1");
    let bob2 = manager_device(26, 62, "bob-2");

    let mut alice_manager = session_manager(&alice, 60 * 60);
    let mut bob1_manager = session_manager(&bob1, 60 * 60);
    let mut bob2_manager = session_manager(&bob2, 60 * 60);

    alice_manager.observe_peer_app_keys(
        bob1.owner_pubkey,
        app_keys_for(&[&bob1, &bob2], 5),
        UnixSeconds(5),
    );
    alice_manager.observe_device_invite(
        bob1.owner_pubkey,
        public_device_invite(&mut bob1_manager, &bob1, 5, 1_810_000_005)?,
    )?;
    alice_manager.observe_device_invite(
        bob2.owner_pubkey,
        public_device_invite(&mut bob2_manager, &bob2, 6, 1_810_000_006)?,
    )?;

    let mut poisoned_snapshot = alice_manager.snapshot();
    let bob2_record = poisoned_snapshot
        .users
        .iter_mut()
        .find(|user| user.owner_pubkey == bob2.owner_pubkey)
        .and_then(|user| {
            user.devices
                .iter_mut()
                .find(|device| device.device_pubkey == bob2.device_pubkey)
        })
        .expect("bob2 device record");
    bob2_record
        .public_invite
        .as_mut()
        .expect("public invite should exist")
        .max_uses = Some(0);

    let mut restored = restore_manager(&poisoned_snapshot, alice.secret_key, 60 * 60)?;
    let before = snapshot(&restored.snapshot());

    let mut send_ctx = context(7, 1_810_000_007);
    let result = restored.prepare_send_text(&mut send_ctx, bob1.owner_pubkey, "fail".to_string());
    assert!(matches!(
        result,
        Err(Error::Domain(DomainError::InviteExhausted))
    ));
    assert_eq!(snapshot(&restored.snapshot()), before);
    Ok(())
}

#[test]
fn failed_receive_rolls_back_state_for_that_device_record() -> Result<()> {
    let alice = manager_device(27, 71, "alice-1");
    let bob = manager_device(28, 81, "bob-1");

    let mut alice_manager = session_manager(&alice, 60 * 60);
    let mut bob_manager = session_manager(&bob, 60 * 60);

    alice_manager.observe_peer_app_keys(bob.owner_pubkey, app_keys_for(&[&bob], 8), UnixSeconds(8));
    bob_manager.observe_peer_app_keys(
        alice.owner_pubkey,
        app_keys_for(&[&alice], 8),
        UnixSeconds(8),
    );
    alice_manager.observe_device_invite(
        bob.owner_pubkey,
        public_device_invite(&mut bob_manager, &bob, 8, 1_810_000_008)?,
    )?;
    bob_manager.observe_device_invite(
        alice.owner_pubkey,
        public_device_invite(&mut alice_manager, &alice, 9, 1_810_000_009)?,
    )?;

    let mut boot_ctx = context(10, 1_810_000_010);
    let prepared =
        alice_manager.prepare_send_text(&mut boot_ctx, bob.owner_pubkey, "boot".to_string())?;
    let mut bob_observe_ctx = context(11, 1_810_000_011);
    bob_manager.observe_invite_response(
        &mut bob_observe_ctx,
        &incoming_invite_response(&prepared.invite_responses[0])?,
    )?;
    let mut bob_receive_ctx = context(12, 1_810_000_012);
    manager_receive_delivery(
        &mut bob_manager,
        &mut bob_receive_ctx,
        alice.owner_pubkey,
        &prepared.deliveries[0],
    )?;

    let mut reply_ctx = context(13, 1_810_000_013);
    let reply =
        bob_manager.prepare_send_text(&mut reply_ctx, alice.owner_pubkey, "reply".to_string())?;
    let mut tampered = reply.deliveries[0].clone();
    tampered.envelope.ciphertext = mutate_text(&tampered.envelope.ciphertext);

    let before = snapshot(&alice_manager.snapshot());
    let mut receive_ctx = context(14, 1_810_000_014);
    let result = manager_receive_delivery(
        &mut alice_manager,
        &mut receive_ctx,
        bob.owner_pubkey,
        &tampered,
    );
    assert!(result.is_err());
    assert_eq!(snapshot(&alice_manager.snapshot()), before);
    Ok(())
}

#[test]
fn malformed_device_invite_observation_does_not_corrupt_state() -> Result<()> {
    let alice = manager_device(29, 91, "alice-1");
    let bob = manager_device(30, 101, "bob-1");
    let mut alice_manager = session_manager(&alice, 60 * 60);
    let mut bob_manager = session_manager(&bob, 60 * 60);

    alice_manager.observe_peer_app_keys(
        bob.owner_pubkey,
        app_keys_for(&[&bob], 15),
        UnixSeconds(15),
    );
    let before = snapshot(&alice_manager.snapshot());

    let mut bad_invite = public_device_invite(&mut bob_manager, &bob, 15, 1_810_000_015)?;
    bad_invite.owner_public_key = Some(alice.owner_pubkey);
    let result = alice_manager.observe_device_invite(bob.owner_pubkey, bad_invite);
    assert!(result.is_err());
    assert_eq!(snapshot(&alice_manager.snapshot()), before);
    Ok(())
}

#[test]
fn invite_response_replay_is_rejected_and_state_unchanged() -> Result<()> {
    let alice = manager_device(31, 111, "alice-1");
    let bob = manager_device(32, 121, "bob-1");
    let mut alice_manager = session_manager(&alice, 60 * 60);

    let public_invite = {
        let mut ctx = context(16, 1_810_000_016);
        let invite = alice_manager.ensure_local_device_invite(&mut ctx)?.clone();
        public_invite_via_event(&invite, alice.secret_key)?
    };

    let mut accept_ctx = context(17, 1_810_000_017);
    let (_session, response) = public_invite.accept_with_owner(
        &mut accept_ctx,
        bob.device_pubkey,
        bob.secret_key,
        Some(bob.device_id.clone()),
        Some(bob.owner_pubkey),
    )?;
    let incoming = incoming_invite_response(&response)?;

    let mut observe_ctx = context(18, 1_810_000_018);
    let observed = alice_manager.observe_invite_response(&mut observe_ctx, &incoming)?;
    assert!(observed.is_some());

    let after_first = snapshot(&alice_manager.snapshot());
    let mut replay_ctx = context(19, 1_810_000_019);
    let replay = alice_manager.observe_invite_response(&mut replay_ctx, &incoming);
    assert!(matches!(
        replay,
        Err(Error::Domain(DomainError::InviteAlreadyUsed))
    ));
    assert_eq!(snapshot(&alice_manager.snapshot()), after_first);
    Ok(())
}

#[test]
fn message_replay_on_stale_or_inactive_session_is_rejected_without_corruption() -> Result<()> {
    let alice = manager_device(33, 131, "alice-1");
    let bob = manager_device(34, 141, "bob-1");

    let mut alice_manager = session_manager(&alice, 60 * 60);
    let mut bob_manager = session_manager(&bob, 60 * 60);

    alice_manager.observe_peer_app_keys(
        bob.owner_pubkey,
        app_keys_for(&[&bob], 20),
        UnixSeconds(20),
    );
    bob_manager.observe_peer_app_keys(
        alice.owner_pubkey,
        app_keys_for(&[&alice], 20),
        UnixSeconds(20),
    );
    alice_manager.observe_device_invite(
        bob.owner_pubkey,
        public_device_invite(&mut bob_manager, &bob, 20, 1_810_000_020)?,
    )?;
    bob_manager.observe_device_invite(
        alice.owner_pubkey,
        public_device_invite(&mut alice_manager, &alice, 21, 1_810_000_021)?,
    )?;

    let mut alice_send_ctx = context(22, 1_810_000_022);
    let alice_first =
        alice_manager.prepare_send_text(&mut alice_send_ctx, bob.owner_pubkey, "a1".to_string())?;
    let mut bob_send_ctx = context(23, 1_810_000_023);
    let bob_first =
        bob_manager.prepare_send_text(&mut bob_send_ctx, alice.owner_pubkey, "b1".to_string())?;

    let mut bob_observe_ctx = context(24, 1_810_000_024);
    bob_manager.observe_invite_response(
        &mut bob_observe_ctx,
        &incoming_invite_response(&alice_first.invite_responses[0])?,
    )?;
    let mut alice_observe_ctx = context(25, 1_810_000_025);
    alice_manager.observe_invite_response(
        &mut alice_observe_ctx,
        &incoming_invite_response(&bob_first.invite_responses[0])?,
    )?;

    let mut alice_receive_ctx = context(26, 1_810_000_026);
    manager_receive_delivery(
        &mut alice_manager,
        &mut alice_receive_ctx,
        bob.owner_pubkey,
        &bob_first.deliveries[0],
    )?;
    let after_first = snapshot(&alice_manager.snapshot());

    let mut replay_ctx = context(27, 1_810_000_027);
    let replay = manager_receive_delivery(
        &mut alice_manager,
        &mut replay_ctx,
        bob.owner_pubkey,
        &bob_first.deliveries[0],
    );
    assert!(replay.is_err());
    assert_eq!(snapshot(&alice_manager.snapshot()), after_first);
    Ok(())
}

#[test]
fn invalid_sender_owner_or_device_does_not_create_records() -> Result<()> {
    let alice = manager_device(35, 151, "alice-1");
    let bob = manager_device(36, 161, "bob-1");
    let mut alice_manager = session_manager(&alice, 60 * 60);
    let mut bob_manager = session_manager(&bob, 60 * 60);

    bob_manager.observe_peer_app_keys(
        alice.owner_pubkey,
        app_keys_for(&[&alice], 28),
        UnixSeconds(28),
    );
    bob_manager.observe_device_invite(
        alice.owner_pubkey,
        public_device_invite(&mut alice_manager, &alice, 28, 1_810_000_028)?,
    )?;
    let mut send_ctx = context(29, 1_810_000_029);
    let prepared =
        bob_manager.prepare_send_text(&mut send_ctx, alice.owner_pubkey, "stray".to_string())?;

    let before = snapshot(&alice_manager.snapshot());
    let mut receive_ctx = context(30, 1_810_000_030);
    let received = manager_receive_delivery(
        &mut alice_manager,
        &mut receive_ctx,
        bob.owner_pubkey,
        &prepared.deliveries[0],
    )?;
    assert!(received.is_none());
    assert_eq!(snapshot(&alice_manager.snapshot()), before);
    Ok(())
}

#[test]
fn late_message_after_pruned_stale_record_is_ignored() -> Result<()> {
    let alice = manager_device(37, 171, "alice-1");
    let bob = manager_device(38, 181, "bob-1");

    let mut alice_manager = session_manager(&alice, 5);
    let mut bob_manager = session_manager(&bob, 5);

    alice_manager.observe_peer_app_keys(
        bob.owner_pubkey,
        app_keys_for(&[&bob], 31),
        UnixSeconds(31),
    );
    bob_manager.observe_peer_app_keys(
        alice.owner_pubkey,
        app_keys_for(&[&alice], 31),
        UnixSeconds(31),
    );
    alice_manager.observe_device_invite(
        bob.owner_pubkey,
        public_device_invite(&mut bob_manager, &bob, 31, 1_810_000_031)?,
    )?;
    bob_manager.observe_device_invite(
        alice.owner_pubkey,
        public_device_invite(&mut alice_manager, &alice, 32, 1_810_000_032)?,
    )?;

    let mut boot_ctx = context(33, 1_810_000_033);
    let alice_first =
        alice_manager.prepare_send_text(&mut boot_ctx, bob.owner_pubkey, "boot".to_string())?;
    let mut bob_observe_ctx = context(34, 1_810_000_034);
    bob_manager.observe_invite_response(
        &mut bob_observe_ctx,
        &incoming_invite_response(&alice_first.invite_responses[0])?,
    )?;
    let mut bob_receive_ctx = context(35, 1_810_000_035);
    manager_receive_delivery(
        &mut bob_manager,
        &mut bob_receive_ctx,
        alice.owner_pubkey,
        &alice_first.deliveries[0],
    )?;

    let mut bob_send_ctx = context(36, 1_810_000_036);
    let bob_reply =
        bob_manager.prepare_send_text(&mut bob_send_ctx, alice.owner_pubkey, "late".to_string())?;

    alice_manager.observe_peer_app_keys(bob.owner_pubkey, app_keys_for(&[], 32), UnixSeconds(32));
    alice_manager.prune_stale_records(UnixSeconds(38));

    let before = snapshot(&alice_manager.snapshot());
    let mut receive_ctx = context(37, 1_810_000_037);
    let received = manager_receive_delivery(
        &mut alice_manager,
        &mut receive_ctx,
        bob.owner_pubkey,
        &bob_reply.deliveries[0],
    )?;
    assert!(received.is_none());
    assert_eq!(snapshot(&alice_manager.snapshot()), before);
    Ok(())
}

#[test]
fn send_after_restore_with_orphaned_old_session_prefers_newly_bootstrapped_active_session(
) -> Result<()> {
    let alice = manager_device(39, 191, "alice-1");
    let bob = manager_device(40, 201, "bob-1");

    let mut alice_manager = session_manager(&alice, 60 * 60);
    let mut bob_manager = session_manager(&bob, 60 * 60);

    let public_alice_invite = {
        let mut ctx = context(38, 1_810_000_038);
        let invite = alice_manager.ensure_local_device_invite(&mut ctx)?.clone();
        public_invite_via_event(&invite, alice.secret_key)?
    };
    let mut accept_ctx = context(39, 1_810_000_039);
    let (_bob_session, response) = public_alice_invite.accept_with_owner(
        &mut accept_ctx,
        bob.device_pubkey,
        bob.secret_key,
        Some(bob.device_id.clone()),
        Some(bob.owner_pubkey),
    )?;
    let mut observe_ctx = context(40, 1_810_000_040);
    alice_manager
        .observe_invite_response(&mut observe_ctx, &incoming_invite_response(&response)?)?;

    alice_manager.observe_peer_app_keys(
        bob.owner_pubkey,
        app_keys_for(&[&bob], 41),
        UnixSeconds(41),
    );
    alice_manager.observe_device_invite(
        bob.owner_pubkey,
        public_device_invite(&mut bob_manager, &bob, 41, 1_810_000_041)?,
    )?;

    let restored = restore_manager(&alice_manager.snapshot(), alice.secret_key, 60 * 60)?;
    let mut restored = restored;

    let mut send_ctx = context(42, 1_810_000_042);
    let prepared =
        restored.prepare_send_text(&mut send_ctx, bob.owner_pubkey, "fresh".to_string())?;
    assert_eq!(prepared.deliveries.len(), 1);
    assert_eq!(prepared.invite_responses.len(), 1);

    let snapshot = restored.snapshot();
    let bob_record = device_snapshot(
        user_snapshot(&snapshot, bob.owner_pubkey),
        bob.device_pubkey,
    );
    let active = bob_record.active_session.as_ref().expect("active session");
    assert_eq!(active.sending_chain_message_number, 1);
    assert!(!bob_record.inactive_sessions.is_empty());
    Ok(())
}
