mod support;

use nostr_double_ratchet::{
    AppKeysSnapshotDecision, DirectMessageContent, DomainError, Error, Invite, OwnerPubkey,
    RelayGap, Result, UnixSeconds,
};
use support::{
    app_keys_for, custom_public_device_invite, manager_device, manager_device_snapshot,
    manager_observe_invite_response, manager_public_device_invite, manager_receive_delivery,
    manager_user_snapshot, observe_device_invites, restore_manager, send_message, session_manager,
    snapshot, ManagerDevice,
};

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
) -> Result<(
    nostr_double_ratchet::Session,
    nostr_double_ratchet::IncomingInviteResponseEnvelope,
)> {
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
fn restore_rejects_mismatched_local_secret_key() -> Result<()> {
    let alice = manager_device(61, 168, "alice-1");
    let bob = manager_device(62, 169, "bob-1");
    let manager = session_manager(&alice, 60 * 60);
    let result = restore_manager(&manager.snapshot(), bob.secret_key, 60 * 60);
    assert!(matches!(
        result,
        Err(Error::Domain(DomainError::InvalidState(_)))
    ));
    Ok(())
}

#[test]
fn duplicate_public_invite_observation_is_idempotent_across_restore() -> Result<()> {
    let alice = manager_device(63, 170, "alice-1");
    let bob = manager_device(64, 171, "bob-1");
    let mut alice_manager = session_manager(&alice, 60 * 60);
    let mut bob_manager = session_manager(&bob, 60 * 60);

    alice_manager.observe_peer_app_keys(
        bob.owner_pubkey,
        app_keys_for(&[&bob], 300),
        UnixSeconds(300),
    );
    let bob_invite = manager_public_device_invite(&mut bob_manager, &bob, 301, 301)?;
    alice_manager.observe_device_invite(bob.owner_pubkey, bob_invite.clone())?;
    let after_first = snapshot(&alice_manager.snapshot());

    let mut restored = restore_manager(&alice_manager.snapshot(), alice.secret_key, 60 * 60)?;
    restored.observe_device_invite(bob.owner_pubkey, bob_invite)?;
    assert_eq!(snapshot(&restored.snapshot()), after_first);
    Ok(())
}

#[test]
fn older_public_invite_replay_after_newer_invite_is_ignored() -> Result<()> {
    let alice = manager_device(65, 172, "alice-1");
    let bob = manager_device(66, 173, "bob-1");
    let mut alice_manager = session_manager(&alice, 60 * 60);

    alice_manager.observe_peer_app_keys(
        bob.owner_pubkey,
        app_keys_for(&[&bob], 310),
        UnixSeconds(310),
    );

    let older = custom_public_device_invite(&bob, 311, 311, Some(bob.device_id.clone()))?;
    let newer = custom_public_device_invite(&bob, 312, 312, Some(bob.device_id.clone()))?;
    alice_manager.observe_device_invite(bob.owner_pubkey, newer.clone())?;
    alice_manager.observe_device_invite(bob.owner_pubkey, older)?;

    let snapshot = alice_manager.snapshot();
    let bob_record = manager_device_snapshot(
        manager_user_snapshot(&snapshot, bob.owner_pubkey),
        bob.device_pubkey,
    );
    assert_eq!(
        bob_record
            .public_invite
            .as_ref()
            .expect("stored public invite")
            .created_at,
        newer.created_at
    );

    let mut send_ctx = support::context(313, 313);
    let prepared =
        alice_manager.prepare_send_text(&mut send_ctx, bob.owner_pubkey, "newer".into())?;
    assert_eq!(
        prepared.invite_responses[0].recipient,
        newer.inviter_ephemeral_public_key
    );
    Ok(())
}

#[test]
fn stale_appkeys_replay_after_restore_does_not_resurrect_removed_device() -> Result<()> {
    let alice = manager_device(67, 174, "alice-1");
    let bob = manager_device(68, 175, "bob-1");
    let mut alice_manager = session_manager(&alice, 60 * 60);

    alice_manager.observe_peer_app_keys(
        bob.owner_pubkey,
        app_keys_for(&[&bob], 320),
        UnixSeconds(320),
    );
    alice_manager.observe_peer_app_keys(bob.owner_pubkey, app_keys_for(&[], 321), UnixSeconds(321));

    let mut restored = restore_manager(&alice_manager.snapshot(), alice.secret_key, 60 * 60)?;
    assert_eq!(
        restored.observe_peer_app_keys(
            bob.owner_pubkey,
            app_keys_for(&[&bob], 320),
            UnixSeconds(320),
        ),
        AppKeysSnapshotDecision::Stale
    );

    let snapshot = restored.snapshot();
    let bob_record = manager_device_snapshot(
        manager_user_snapshot(&snapshot, bob.owner_pubkey),
        bob.device_pubkey,
    );
    assert!(!bob_record.authorized);
    assert!(bob_record.is_stale);
    Ok(())
}

#[test]
fn older_or_incomplete_invite_does_not_erase_known_device_metadata() -> Result<()> {
    let alice = manager_device(69, 176, "alice-1");
    let bob = manager_device(70, 177, "bob-1");
    let mut alice_manager = session_manager(&alice, 60 * 60);

    alice_manager.observe_peer_app_keys(
        bob.owner_pubkey,
        app_keys_for(&[&bob], 330),
        UnixSeconds(330),
    );

    let complete = custom_public_device_invite(&bob, 331, 331, Some(bob.device_id.clone()))?;
    let incomplete = custom_public_device_invite(&bob, 332, 100, None)?;
    observe_device_invites(
        &mut alice_manager,
        bob.owner_pubkey,
        &[complete.clone(), incomplete],
    )?;

    let snapshot = alice_manager.snapshot();
    let bob_record = manager_device_snapshot(
        manager_user_snapshot(&snapshot, bob.owner_pubkey),
        bob.device_pubkey,
    );
    assert_eq!(bob_record.device_id.as_ref(), Some(&bob.device_id));
    assert_eq!(
        bob_record
            .public_invite
            .as_ref()
            .expect("stored invite")
            .inviter_ephemeral_public_key,
        complete.inviter_ephemeral_public_key
    );
    Ok(())
}

#[test]
fn delayed_invite_response_for_superseded_bootstrap_does_not_corrupt_active_session() -> Result<()>
{
    let alice = manager_device(71, 178, "alice-1");
    let bob = manager_device(72, 179, "bob-1");
    let mut alice_manager = session_manager(&alice, 60 * 60);
    let mut bob_manager = session_manager(&bob, 60 * 60);

    let public_alice_invite = public_local_invite(&mut alice_manager, &alice, 340, 340)?;
    let (_old_bob_session, old_response) = incoming_response_from_public_invite(
        &public_alice_invite,
        &bob,
        Some(bob.owner_pubkey),
        341,
        341,
    )?;
    alice_manager.observe_peer_app_keys(
        bob.owner_pubkey,
        app_keys_for(&[&bob], 342),
        UnixSeconds(342),
    );
    alice_manager.observe_invite_response(&mut support::context(343, 343), &old_response)?;

    let bob_invite = manager_public_device_invite(&mut bob_manager, &bob, 344, 344)?;
    alice_manager.observe_device_invite(bob.owner_pubkey, bob_invite)?;
    let fresh = alice_manager.prepare_send_text(
        &mut support::context(345, 345),
        bob.owner_pubkey,
        "fresh".into(),
    )?;
    assert_eq!(fresh.invite_responses.len(), 1);

    let mut restored = restore_manager(&alice_manager.snapshot(), alice.secret_key, 60 * 60)?;
    let before = snapshot(&restored.snapshot());
    let replay = restored.observe_invite_response(&mut support::context(346, 346), &old_response);
    assert!(matches!(
        replay,
        Err(Error::Domain(DomainError::InviteAlreadyUsed))
    ));
    assert_eq!(snapshot(&restored.snapshot()), before);
    Ok(())
}

#[test]
fn delayed_message_from_old_session_after_rebootstrap_does_not_take_over_newer_active_session(
) -> Result<()> {
    let alice = manager_device(73, 180, "alice-1");
    let bob = manager_device(74, 181, "bob-1");
    let mut alice_manager = session_manager(&alice, 60 * 60);
    let mut bob_manager = session_manager(&bob, 60 * 60);

    let public_alice_invite = public_local_invite(&mut alice_manager, &alice, 350, 350)?;
    let (mut old_bob_session, old_response) = incoming_response_from_public_invite(
        &public_alice_invite,
        &bob,
        Some(bob.owner_pubkey),
        351,
        351,
    )?;
    alice_manager.observe_invite_response(&mut support::context(352, 352), &old_response)?;

    alice_manager.observe_peer_app_keys(
        bob.owner_pubkey,
        app_keys_for(&[&bob], 353),
        UnixSeconds(353),
    );
    bob_manager.observe_peer_app_keys(
        alice.owner_pubkey,
        app_keys_for(&[&alice], 353),
        UnixSeconds(353),
    );
    let bob_invite = manager_public_device_invite(&mut bob_manager, &bob, 354, 354)?;
    alice_manager.observe_device_invite(bob.owner_pubkey, bob_invite)?;

    let fresh = alice_manager.prepare_send_text(
        &mut support::context(355, 355),
        bob.owner_pubkey,
        "fresh".into(),
    )?;
    manager_observe_invite_response(
        &mut bob_manager,
        &mut support::context(356, 356),
        &fresh.invite_responses[0],
    )?;
    manager_receive_delivery(
        &mut bob_manager,
        &mut support::context(357, 357),
        alice.owner_pubkey,
        &fresh.deliveries[0],
    )?;

    let new_reply = bob_manager.prepare_send_text(
        &mut support::context(358, 358),
        alice.owner_pubkey,
        "new-reply".into(),
    )?;
    manager_receive_delivery(
        &mut alice_manager,
        &mut support::context(359, 359),
        bob.owner_pubkey,
        &new_reply.deliveries[0],
    )?;

    let before = alice_manager.snapshot();
    let before_record = manager_device_snapshot(
        manager_user_snapshot(&before, bob.owner_pubkey),
        bob.device_pubkey,
    )
    .active_session
    .clone();

    let delayed = send_message(
        &mut old_bob_session,
        &mut support::context(360, 360),
        DirectMessageContent::Text("old-delayed".into()),
    )?;
    let received = alice_manager
        .receive_direct_message(
            &mut support::context(361, 361),
            bob.owner_pubkey,
            &delayed.incoming,
        )?
        .expect("alice should still decrypt delayed old-session message");
    assert_eq!(received.rumor.content, "old-delayed");

    let after = alice_manager.snapshot();
    let after_record = manager_device_snapshot(
        manager_user_snapshot(&after, bob.owner_pubkey),
        bob.device_pubkey,
    );
    assert_eq!(after_record.active_session, before_record);
    assert!(!after_record.inactive_sessions.is_empty());
    Ok(())
}

#[test]
fn partial_restore_with_cached_invite_but_no_appkeys_still_surfaces_missing_appkeys_gap(
) -> Result<()> {
    let alice = manager_device(75, 182, "alice-1");
    let bob = manager_device(76, 183, "bob-1");
    let mut alice_manager = session_manager(&alice, 60 * 60);
    let mut bob_manager = session_manager(&bob, 60 * 60);

    let bob_invite = manager_public_device_invite(&mut bob_manager, &bob, 370, 370)?;
    alice_manager.observe_device_invite(bob.owner_pubkey, bob_invite)?;

    let mut restored = restore_manager(&alice_manager.snapshot(), alice.secret_key, 60 * 60)?;
    let prepared = restored.prepare_send_text(
        &mut support::context(371, 371),
        bob.owner_pubkey,
        "gap".into(),
    )?;
    assert_eq!(
        prepared.relay_gaps,
        vec![RelayGap::MissingAppKeys {
            owner_pubkey: bob.owner_pubkey,
        }]
    );
    Ok(())
}

#[test]
fn pruned_stale_device_is_not_recreated_by_late_old_invite_observation_without_new_appkeys(
) -> Result<()> {
    let alice = manager_device(77, 184, "alice-1");
    let bob = manager_device(78, 185, "bob-1");
    let mut alice_manager = session_manager(&alice, 10);
    let mut bob_manager = session_manager(&bob, 10);

    alice_manager.observe_peer_app_keys(
        bob.owner_pubkey,
        app_keys_for(&[&bob], 380),
        UnixSeconds(380),
    );
    let bob_invite = manager_public_device_invite(&mut bob_manager, &bob, 381, 381)?;
    alice_manager.observe_device_invite(bob.owner_pubkey, bob_invite.clone())?;
    alice_manager.observe_peer_app_keys(bob.owner_pubkey, app_keys_for(&[], 382), UnixSeconds(382));
    let prune = alice_manager.prune_stale_records(UnixSeconds(393));
    assert_eq!(
        prune.removed_devices,
        vec![(bob.owner_pubkey, bob.device_pubkey)]
    );

    let before = snapshot(&alice_manager.snapshot());
    alice_manager.observe_device_invite(bob.owner_pubkey, bob_invite)?;
    assert_eq!(snapshot(&alice_manager.snapshot()), before);
    Ok(())
}

#[test]
fn double_restore_with_replayed_artifacts_remains_deterministic() -> Result<()> {
    let alice = manager_device(79, 186, "alice-1");
    let bob = manager_device(80, 187, "bob-1");
    let mut alice_manager = session_manager(&alice, 60 * 60);
    let mut bob_manager = session_manager(&bob, 60 * 60);

    alice_manager.observe_peer_app_keys(
        bob.owner_pubkey,
        app_keys_for(&[&bob], 390),
        UnixSeconds(390),
    );
    let bob_invite = manager_public_device_invite(&mut bob_manager, &bob, 391, 391)?;
    alice_manager.observe_device_invite(bob.owner_pubkey, bob_invite.clone())?;

    let public_alice_invite = public_local_invite(&mut alice_manager, &alice, 392, 392)?;
    let (_bob_session, incoming_response) = incoming_response_from_public_invite(
        &public_alice_invite,
        &bob,
        Some(bob.owner_pubkey),
        393,
        393,
    )?;
    alice_manager.observe_invite_response(&mut support::context(394, 394), &incoming_response)?;

    let base_snapshot = alice_manager.snapshot();

    let mut restored_once = restore_manager(&base_snapshot, alice.secret_key, 60 * 60)?;
    let stale_replay = restored_once.observe_peer_app_keys(
        bob.owner_pubkey,
        app_keys_for(&[&bob], 389),
        UnixSeconds(389),
    );
    assert_eq!(stale_replay, AppKeysSnapshotDecision::Stale);
    restored_once.observe_device_invite(bob.owner_pubkey, bob_invite.clone())?;
    let response_replay =
        restored_once.observe_invite_response(&mut support::context(395, 395), &incoming_response);
    assert!(matches!(
        response_replay,
        Err(Error::Domain(DomainError::InviteAlreadyUsed))
    ));
    let after_first = restored_once.snapshot();

    let mut restored_twice = restore_manager(&after_first, alice.secret_key, 60 * 60)?;
    let stale_replay = restored_twice.observe_peer_app_keys(
        bob.owner_pubkey,
        app_keys_for(&[&bob], 389),
        UnixSeconds(389),
    );
    assert_eq!(stale_replay, AppKeysSnapshotDecision::Stale);
    restored_twice.observe_device_invite(bob.owner_pubkey, bob_invite)?;
    let response_replay =
        restored_twice.observe_invite_response(&mut support::context(396, 396), &incoming_response);
    assert!(matches!(
        response_replay,
        Err(Error::Domain(DomainError::InviteAlreadyUsed))
    ));

    assert_eq!(snapshot(&restored_twice.snapshot()), snapshot(&after_first));
    Ok(())
}
