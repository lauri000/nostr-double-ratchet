mod support;

use nostr_double_ratchet::{
    AppKeysSnapshotDecision, DeviceRecordSnapshot, PreparedFanout, RelayGap, Result,
    SessionManagerSnapshot, UnixSeconds, UserRecordSnapshot,
};
use support::{
    app_keys_for, context, manager_device, manager_observe_invite_response,
    manager_receive_delivery, public_invite_via_event, session_manager, snapshot, ManagerDevice,
};

fn public_device_invite(
    manager: &mut nostr_double_ratchet::SessionManager,
    device: &ManagerDevice,
    seed: u64,
    now_secs: u64,
) -> Result<nostr_double_ratchet::Invite> {
    let mut ctx = context(seed, now_secs);
    let invite = manager.ensure_local_device_invite(&mut ctx)?.clone();
    public_invite_via_event(&invite, device.secret_key)
}

fn user_snapshot(
    snapshot: &SessionManagerSnapshot,
    owner_pubkey: nostr_double_ratchet::OwnerPubkey,
) -> &UserRecordSnapshot {
    snapshot
        .users
        .iter()
        .find(|user| user.owner_pubkey == owner_pubkey)
        .expect("owner snapshot must exist")
}

fn device_snapshot(
    user: &UserRecordSnapshot,
    device_pubkey: nostr_double_ratchet::DevicePubkey,
) -> &DeviceRecordSnapshot {
    user.devices
        .iter()
        .find(|device| device.device_pubkey == device_pubkey)
        .expect("device snapshot must exist")
}

fn assert_gap(prepared: &PreparedFanout, expected: RelayGap) {
    assert!(
        prepared.relay_gaps.contains(&expected),
        "missing relay gap {expected:?} in {:?}",
        prepared.relay_gaps
    );
}

#[test]
fn local_device_invite_is_stable_and_owned() -> Result<()> {
    let alice = manager_device(1, 11, "alice-1");
    let mut manager = session_manager(&alice, 60 * 60);

    let mut first_ctx = context(1, 1_800_000_000);
    let first = manager.ensure_local_device_invite(&mut first_ctx)?.clone();
    let mut second_ctx = context(2, 1_800_000_001);
    let second = manager.ensure_local_device_invite(&mut second_ctx)?.clone();

    assert_eq!(first, second);
    assert_eq!(first.inviter, alice.device_pubkey.as_owner());
    assert_eq!(first.owner_public_key, Some(alice.owner_pubkey));
    assert!(first.inviter_ephemeral_private_key.is_some());
    Ok(())
}

#[test]
fn latest_app_keys_controls_authorized_device_roster() -> Result<()> {
    let alice1 = manager_device(2, 21, "alice-1");
    let alice2 = manager_device(2, 22, "alice-2");
    let alice3 = manager_device(2, 23, "alice-3");
    let mut manager = session_manager(&alice1, 60 * 60);

    assert_eq!(
        manager.apply_local_app_keys(app_keys_for(&[&alice1, &alice2], 10), UnixSeconds(10)),
        AppKeysSnapshotDecision::Advanced
    );
    assert_eq!(
        manager.apply_local_app_keys(app_keys_for(&[&alice1], 9), UnixSeconds(9)),
        AppKeysSnapshotDecision::Stale
    );
    assert_eq!(
        manager.apply_local_app_keys(app_keys_for(&[&alice1, &alice3], 10), UnixSeconds(10)),
        AppKeysSnapshotDecision::MergedEqualTimestamp
    );
    assert_eq!(
        manager.apply_local_app_keys(app_keys_for(&[&alice1, &alice3], 11), UnixSeconds(11)),
        AppKeysSnapshotDecision::Advanced
    );

    let snapshot = manager.snapshot();
    let local_user = user_snapshot(&snapshot, alice1.owner_pubkey);
    assert_eq!(local_user.devices.len(), 3);

    let alice2_record = device_snapshot(local_user, alice2.device_pubkey);
    assert!(!alice2_record.authorized);
    assert!(alice2_record.is_stale);
    assert_eq!(alice2_record.stale_since, Some(UnixSeconds(11)));

    let alice3_record = device_snapshot(local_user, alice3.device_pubkey);
    assert!(alice3_record.authorized);
    assert!(!alice3_record.is_stale);
    Ok(())
}

#[test]
fn invite_observed_before_appkeys_becomes_usable_after_authorization() -> Result<()> {
    let alice = manager_device(3, 31, "alice-1");
    let bob = manager_device(4, 41, "bob-1");
    let mut alice_manager = session_manager(&alice, 60 * 60);
    let mut bob_manager = session_manager(&bob, 60 * 60);

    let bob_invite = public_device_invite(&mut bob_manager, &bob, 10, 1_800_000_100)?;
    alice_manager.observe_device_invite(bob.owner_pubkey, bob_invite)?;

    let mut send_ctx = context(11, 1_800_000_101);
    let before_auth =
        alice_manager.prepare_send_text(&mut send_ctx, bob.owner_pubkey, "hi".to_string())?;
    assert!(before_auth.deliveries.is_empty());
    assert_gap(
        &before_auth,
        RelayGap::MissingAppKeys {
            owner_pubkey: bob.owner_pubkey,
        },
    );

    alice_manager.observe_peer_app_keys(
        bob.owner_pubkey,
        app_keys_for(&[&bob], 12),
        UnixSeconds(12),
    );
    let mut send_ctx = context(12, 1_800_000_102);
    let after_auth =
        alice_manager.prepare_send_text(&mut send_ctx, bob.owner_pubkey, "usable".to_string())?;

    assert_eq!(after_auth.deliveries.len(), 1);
    assert_eq!(after_auth.invite_responses.len(), 1);
    assert!(after_auth.relay_gaps.is_empty());
    Ok(())
}

#[test]
fn prepare_send_fans_out_to_recipient_devices_and_local_siblings() -> Result<()> {
    let alice1 = manager_device(5, 51, "alice-1");
    let alice2 = manager_device(5, 52, "alice-2");
    let bob1 = manager_device(6, 61, "bob-1");
    let bob2 = manager_device(6, 62, "bob-2");

    let mut alice_manager = session_manager(&alice1, 60 * 60);
    let mut alice2_manager = session_manager(&alice2, 60 * 60);
    let mut bob1_manager = session_manager(&bob1, 60 * 60);
    let mut bob2_manager = session_manager(&bob2, 60 * 60);

    alice_manager.apply_local_app_keys(app_keys_for(&[&alice1, &alice2], 20), UnixSeconds(20));
    alice_manager.observe_device_invite(
        alice1.owner_pubkey,
        public_device_invite(&mut alice2_manager, &alice2, 20, 1_800_000_200)?,
    )?;

    alice_manager.observe_peer_app_keys(
        bob1.owner_pubkey,
        app_keys_for(&[&bob1, &bob2], 21),
        UnixSeconds(21),
    );
    alice_manager.observe_device_invite(
        bob1.owner_pubkey,
        public_device_invite(&mut bob1_manager, &bob1, 21, 1_800_000_201)?,
    )?;
    alice_manager.observe_device_invite(
        bob2.owner_pubkey,
        public_device_invite(&mut bob2_manager, &bob2, 22, 1_800_000_202)?,
    )?;

    let mut send_ctx = context(23, 1_800_000_203);
    let prepared =
        alice_manager.prepare_send_text(&mut send_ctx, bob1.owner_pubkey, "fanout".to_string())?;

    let targets: std::collections::BTreeSet<_> = prepared
        .deliveries
        .iter()
        .map(|delivery| (delivery.owner_pubkey, delivery.device_pubkey))
        .collect();

    assert_eq!(prepared.deliveries.len(), 3);
    assert_eq!(prepared.invite_responses.len(), 3);
    assert!(prepared.relay_gaps.is_empty());
    assert!(targets.contains(&(bob1.owner_pubkey, bob1.device_pubkey)));
    assert!(targets.contains(&(bob2.owner_pubkey, bob2.device_pubkey)));
    assert!(targets.contains(&(alice1.owner_pubkey, alice2.device_pubkey)));
    assert!(!targets.contains(&(alice1.owner_pubkey, alice1.device_pubkey)));
    Ok(())
}

#[test]
fn prepare_send_bootstraps_from_public_invite_and_returns_invite_response() -> Result<()> {
    let alice = manager_device(7, 71, "alice-1");
    let bob = manager_device(8, 81, "bob-1");

    let mut alice_manager = session_manager(&alice, 60 * 60);
    let mut bob_manager = session_manager(&bob, 60 * 60);

    alice_manager.observe_peer_app_keys(
        bob.owner_pubkey,
        app_keys_for(&[&bob], 30),
        UnixSeconds(30),
    );
    alice_manager.observe_device_invite(
        bob.owner_pubkey,
        public_device_invite(&mut bob_manager, &bob, 30, 1_800_000_300)?,
    )?;

    let mut send_ctx = context(31, 1_800_000_301);
    let prepared =
        alice_manager.prepare_send_text(&mut send_ctx, bob.owner_pubkey, "hello".to_string())?;
    assert_eq!(prepared.deliveries.len(), 1);
    assert_eq!(prepared.invite_responses.len(), 1);

    let mut observe_ctx = context(32, 1_800_000_302);
    let observed = manager_observe_invite_response(
        &mut bob_manager,
        &mut observe_ctx,
        &prepared.invite_responses[0],
    )?;
    assert!(observed.is_some());

    let mut receive_ctx = context(33, 1_800_000_303);
    let received = manager_receive_delivery(
        &mut bob_manager,
        &mut receive_ctx,
        alice.owner_pubkey,
        &prepared.deliveries[0],
    )?
    .expect("expected received message");
    assert_eq!(received.rumor.content, "hello");
    Ok(())
}

#[test]
fn receive_on_inactive_session_promotes_it_to_active() -> Result<()> {
    let alice = manager_device(9, 91, "alice-1");
    let bob = manager_device(10, 101, "bob-1");

    let mut alice_manager = session_manager(&alice, 60 * 60);
    let mut bob_manager = session_manager(&bob, 60 * 60);

    alice_manager.observe_peer_app_keys(
        bob.owner_pubkey,
        app_keys_for(&[&bob], 40),
        UnixSeconds(40),
    );
    bob_manager.observe_peer_app_keys(
        alice.owner_pubkey,
        app_keys_for(&[&alice], 40),
        UnixSeconds(40),
    );

    alice_manager.observe_device_invite(
        bob.owner_pubkey,
        public_device_invite(&mut bob_manager, &bob, 40, 1_800_000_400)?,
    )?;
    bob_manager.observe_device_invite(
        alice.owner_pubkey,
        public_device_invite(&mut alice_manager, &alice, 41, 1_800_000_401)?,
    )?;

    let mut alice_send_ctx = context(42, 1_800_000_402);
    let alice_prepared =
        alice_manager.prepare_send_text(&mut alice_send_ctx, bob.owner_pubkey, "a1".to_string())?;
    let mut bob_send_ctx = context(43, 1_800_000_403);
    let bob_prepared =
        bob_manager.prepare_send_text(&mut bob_send_ctx, alice.owner_pubkey, "b1".to_string())?;

    let mut alice_observe_ctx = context(44, 1_800_000_404);
    alice_manager.observe_invite_response(
        &mut alice_observe_ctx,
        &support::incoming_invite_response(&bob_prepared.invite_responses[0])?,
    )?;
    let before = alice_manager.snapshot();

    let mut bob_observe_ctx = context(45, 1_800_000_405);
    bob_manager.observe_invite_response(
        &mut bob_observe_ctx,
        &support::incoming_invite_response(&alice_prepared.invite_responses[0])?,
    )?;

    let mut receive_ctx = context(46, 1_800_000_406);
    let received = manager_receive_delivery(
        &mut alice_manager,
        &mut receive_ctx,
        bob.owner_pubkey,
        &bob_prepared.deliveries[0],
    )?
    .expect("expected received message");
    assert_eq!(received.rumor.content, "b1");

    let after = alice_manager.snapshot();
    let before_record =
        device_snapshot(user_snapshot(&before, bob.owner_pubkey), bob.device_pubkey);
    let after_record = device_snapshot(user_snapshot(&after, bob.owner_pubkey), bob.device_pubkey);
    assert_ne!(before_record.active_session, after_record.active_session);
    assert!(before_record
        .active_session
        .as_ref()
        .is_some_and(|state| after_record.inactive_sessions.contains(state)));
    Ok(())
}

#[test]
fn simultaneous_bootstrap_converges_on_matching_active_sessions() -> Result<()> {
    let alice = manager_device(11, 111, "alice-1");
    let bob = manager_device(12, 121, "bob-1");

    let mut alice_manager = session_manager(&alice, 60 * 60);
    let mut bob_manager = session_manager(&bob, 60 * 60);

    alice_manager.observe_peer_app_keys(
        bob.owner_pubkey,
        app_keys_for(&[&bob], 50),
        UnixSeconds(50),
    );
    bob_manager.observe_peer_app_keys(
        alice.owner_pubkey,
        app_keys_for(&[&alice], 50),
        UnixSeconds(50),
    );

    alice_manager.observe_device_invite(
        bob.owner_pubkey,
        public_device_invite(&mut bob_manager, &bob, 50, 1_800_000_500)?,
    )?;
    bob_manager.observe_device_invite(
        alice.owner_pubkey,
        public_device_invite(&mut alice_manager, &alice, 51, 1_800_000_501)?,
    )?;

    let mut alice_send_ctx = context(52, 1_800_000_502);
    let alice_first =
        alice_manager.prepare_send_text(&mut alice_send_ctx, bob.owner_pubkey, "a1".to_string())?;
    let mut bob_send_ctx = context(53, 1_800_000_503);
    let bob_first =
        bob_manager.prepare_send_text(&mut bob_send_ctx, alice.owner_pubkey, "b1".to_string())?;

    let mut bob_observe_ctx = context(54, 1_800_000_504);
    bob_manager.observe_invite_response(
        &mut bob_observe_ctx,
        &support::incoming_invite_response(&alice_first.invite_responses[0])?,
    )?;
    let mut alice_observe_ctx = context(55, 1_800_000_505);
    alice_manager.observe_invite_response(
        &mut alice_observe_ctx,
        &support::incoming_invite_response(&bob_first.invite_responses[0])?,
    )?;

    let mut bob_receive_ctx = context(56, 1_800_000_506);
    let bob_received = manager_receive_delivery(
        &mut bob_manager,
        &mut bob_receive_ctx,
        alice.owner_pubkey,
        &alice_first.deliveries[0],
    )?
    .expect("bob should receive alice first message");
    assert_eq!(bob_received.rumor.content, "a1");

    let mut alice_receive_ctx = context(57, 1_800_000_507);
    let alice_received = manager_receive_delivery(
        &mut alice_manager,
        &mut alice_receive_ctx,
        bob.owner_pubkey,
        &bob_first.deliveries[0],
    )?
    .expect("alice should receive bob first message");
    assert_eq!(alice_received.rumor.content, "b1");

    let mut alice_next_ctx = context(58, 1_800_000_508);
    let alice_second =
        alice_manager.prepare_send_text(&mut alice_next_ctx, bob.owner_pubkey, "a2".to_string())?;
    assert!(alice_second.invite_responses.is_empty());

    let mut bob_next_receive_ctx = context(59, 1_800_000_509);
    let bob_second = manager_receive_delivery(
        &mut bob_manager,
        &mut bob_next_receive_ctx,
        alice.owner_pubkey,
        &alice_second.deliveries[0],
    )?
    .expect("bob should receive alice second message");
    assert_eq!(bob_second.rumor.content, "a2");
    Ok(())
}

#[test]
fn removed_device_is_excluded_from_send_but_can_still_decrypt_while_stale() -> Result<()> {
    let alice = manager_device(13, 131, "alice-1");
    let bob = manager_device(14, 141, "bob-1");

    let mut alice_manager = session_manager(&alice, 60 * 60);
    let mut bob_manager = session_manager(&bob, 60 * 60);

    alice_manager.observe_peer_app_keys(
        bob.owner_pubkey,
        app_keys_for(&[&bob], 60),
        UnixSeconds(60),
    );
    bob_manager.observe_peer_app_keys(
        alice.owner_pubkey,
        app_keys_for(&[&alice], 60),
        UnixSeconds(60),
    );
    alice_manager.observe_device_invite(
        bob.owner_pubkey,
        public_device_invite(&mut bob_manager, &bob, 60, 1_800_000_600)?,
    )?;
    bob_manager.observe_device_invite(
        alice.owner_pubkey,
        public_device_invite(&mut alice_manager, &alice, 61, 1_800_000_601)?,
    )?;

    let mut alice_send_ctx = context(62, 1_800_000_602);
    let alice_first = alice_manager.prepare_send_text(
        &mut alice_send_ctx,
        bob.owner_pubkey,
        "boot".to_string(),
    )?;
    let mut bob_observe_ctx = context(63, 1_800_000_603);
    bob_manager.observe_invite_response(
        &mut bob_observe_ctx,
        &support::incoming_invite_response(&alice_first.invite_responses[0])?,
    )?;
    let mut bob_receive_ctx = context(64, 1_800_000_604);
    manager_receive_delivery(
        &mut bob_manager,
        &mut bob_receive_ctx,
        alice.owner_pubkey,
        &alice_first.deliveries[0],
    )?;

    let mut bob_send_ctx = context(65, 1_800_000_605);
    let bob_reply =
        bob_manager.prepare_send_text(&mut bob_send_ctx, alice.owner_pubkey, "late".to_string())?;

    alice_manager.observe_peer_app_keys(bob.owner_pubkey, app_keys_for(&[], 61), UnixSeconds(61));

    let mut alice_send_ctx = context(66, 1_800_000_606);
    let after_removal = alice_manager.prepare_send_text(
        &mut alice_send_ctx,
        bob.owner_pubkey,
        "blocked".to_string(),
    )?;
    assert!(after_removal.deliveries.is_empty());

    let mut receive_ctx = context(67, 1_800_000_607);
    let received = manager_receive_delivery(
        &mut alice_manager,
        &mut receive_ctx,
        bob.owner_pubkey,
        &bob_reply.deliveries[0],
    )?
    .expect("stale device should still decrypt delayed message");
    assert_eq!(received.rumor.content, "late");
    Ok(())
}

#[test]
fn prune_removes_stale_devices_after_max_relay_latency() -> Result<()> {
    let alice = manager_device(15, 151, "alice-1");
    let bob = manager_device(16, 161, "bob-1");
    let mut manager = session_manager(&alice, 10);

    manager.observe_peer_app_keys(bob.owner_pubkey, app_keys_for(&[&bob], 70), UnixSeconds(70));
    manager.observe_device_invite(bob.owner_pubkey, {
        let mut bob_manager = session_manager(&bob, 10);
        public_device_invite(&mut bob_manager, &bob, 70, 1_800_000_700)?
    })?;
    manager.observe_peer_app_keys(bob.owner_pubkey, app_keys_for(&[], 71), UnixSeconds(71));

    let early = manager.prune_stale_records(UnixSeconds(80));
    assert!(early.removed_devices.is_empty());

    let late = manager.prune_stale_records(UnixSeconds(82));
    assert_eq!(
        late.removed_devices,
        vec![(bob.owner_pubkey, bob.device_pubkey)]
    );
    let snapshot = manager.snapshot();
    assert!(user_snapshot(&snapshot, bob.owner_pubkey)
        .devices
        .is_empty());
    Ok(())
}

#[test]
fn snapshot_is_deterministic_for_users_devices_and_sessions() -> Result<()> {
    let alice1 = manager_device(17, 171, "alice-1");
    let alice2 = manager_device(17, 172, "alice-2");
    let bob1 = manager_device(18, 181, "bob-1");
    let bob2 = manager_device(18, 182, "bob-2");

    let mut left = session_manager(&alice1, 60 * 60);
    let mut right = session_manager(&alice1, 60 * 60);
    let mut alice2_manager = session_manager(&alice2, 60 * 60);
    let mut bob1_manager = session_manager(&bob1, 60 * 60);
    let mut bob2_manager = session_manager(&bob2, 60 * 60);

    let alice2_invite = public_device_invite(&mut alice2_manager, &alice2, 80, 1_800_000_800)?;
    let bob1_invite = public_device_invite(&mut bob1_manager, &bob1, 81, 1_800_000_801)?;
    let bob2_invite = public_device_invite(&mut bob2_manager, &bob2, 82, 1_800_000_802)?;

    for manager in [&mut left, &mut right] {
        manager.apply_local_app_keys(app_keys_for(&[&alice1, &alice2], 80), UnixSeconds(80));
        manager.observe_peer_app_keys(
            bob1.owner_pubkey,
            app_keys_for(&[&bob1, &bob2], 81),
            UnixSeconds(81),
        );
    }

    left.observe_device_invite(alice1.owner_pubkey, alice2_invite.clone())?;
    left.observe_device_invite(bob1.owner_pubkey, bob1_invite.clone())?;
    left.observe_device_invite(bob2.owner_pubkey, bob2_invite.clone())?;

    right.observe_device_invite(bob2.owner_pubkey, bob2_invite)?;
    right.observe_device_invite(alice1.owner_pubkey, alice2_invite)?;
    right.observe_device_invite(bob1.owner_pubkey, bob1_invite)?;

    let mut left_ctx = context(83, 1_800_000_803);
    let left_prepared = left.prepare_send_text(
        &mut left_ctx,
        bob1.owner_pubkey,
        "deterministic".to_string(),
    )?;
    let mut right_ctx = context(83, 1_800_000_803);
    let right_prepared = right.prepare_send_text(
        &mut right_ctx,
        bob1.owner_pubkey,
        "deterministic".to_string(),
    )?;

    assert_eq!(
        left_prepared.deliveries.len(),
        right_prepared.deliveries.len()
    );
    assert_eq!(snapshot(&left.snapshot()), snapshot(&right.snapshot()));
    Ok(())
}
