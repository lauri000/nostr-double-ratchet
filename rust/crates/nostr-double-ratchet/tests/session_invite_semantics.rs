mod support;

use nostr_double_ratchet::{
    codec::nostr as codec, DomainError, Error, Invite, Result,
};
use support::{
    actor, context, corrupt_invite_response_layer, invite_response_fixture, receive_event,
    send_message, snapshot, InviteResponseCorruption, ROOT_URL,
};

#[test]
fn response_is_bound_to_the_specific_invite_instance() -> Result<()> {
    let alice = actor(81, "alice-device");
    let bob = actor(82, "bob-device");

    let mut invite_a_ctx = context(11_000, 1_700_400_000);
    let mut invite_a = Invite::create_new(
        &mut invite_a_ctx,
        alice.owner_pubkey,
        Some(alice.device_id.clone()),
        None,
    )?;

    let mut invite_b_ctx = context(11_001, 1_700_400_001);
    let mut invite_b = Invite::create_new(
        &mut invite_b_ctx,
        alice.owner_pubkey,
        Some(alice.device_id.clone()),
        None,
    )?;

    let public_invite_a = codec::parse_invite_url(&codec::invite_url(&invite_a, ROOT_URL)?)?;
    let mut accept_ctx = context(11_002, 1_700_400_002);
    let (mut bob_session, response_envelope) = public_invite_a.accept(
        &mut accept_ctx,
        bob.device_pubkey,
        bob.secret_key,
        Some(bob.device_id.clone()),
    )?;
    let response_event = codec::invite_response_event(&response_envelope)?;
    let incoming_response = codec::parse_invite_response_event(&response_event)?;

    let before_b = snapshot(&invite_b);
    let mut process_ctx = context(11_003, 1_700_400_003);
    let wrong_invite = invite_b.process_invite_response(
        &mut process_ctx,
        &incoming_response,
        alice.secret_key,
    );
    assert!(wrong_invite.is_err());
    assert_eq!(snapshot(&invite_b), before_b);

    let mut process_ctx = context(11_004, 1_700_400_004);
    let response = invite_a
        .process_invite_response(&mut process_ctx, &incoming_response, alice.secret_key)?
        .expect("expected valid invite response");
    let mut alice_session = response.session;
    assert_eq!(invite_a.used_by, vec![bob.device_pubkey]);

    let mut send_ctx = context(11_005, 1_700_400_005);
    let sent = send_message(
        &mut bob_session,
        &mut send_ctx,
        nostr_double_ratchet::DirectMessageContent::Text("bound".to_string()),
    )?;
    let mut recv_ctx = context(11_006, 1_700_400_006);
    let received = receive_event(&mut alice_session, &mut recv_ctx, &sent.event)?;
    assert_eq!(received.content, "bound");
    Ok(())
}

#[test]
fn invite_max_uses_is_enforced() -> Result<()> {
    let mut fixture = invite_response_fixture(1_700_401_000, Some(1))?;
    let carol = actor(83, "carol-device");

    let mut process_ctx = context(11_100, 1_700_401_010);
    let _first = fixture
        .owned_invite
        .process_invite_response(
            &mut process_ctx,
            &fixture.incoming_response,
            fixture.alice.secret_key,
        )?
        .expect("expected first invite response");
    assert_eq!(fixture.owned_invite.used_by, vec![fixture.bob.device_pubkey]);

    let mut accept_ctx = context(11_101, 1_700_401_011);
    let second_accept = fixture.owned_invite.accept(
        &mut accept_ctx,
        carol.device_pubkey,
        carol.secret_key,
        Some(carol.device_id.clone()),
    );
    assert!(matches!(
        second_accept,
        Err(Error::Domain(DomainError::InviteExhausted))
    ));

    let stale_public_invite = fixture.public_invite.clone();
    let mut stale_accept_ctx = context(11_102, 1_700_401_012);
    let (_carol_session, second_response) = stale_public_invite.accept(
        &mut stale_accept_ctx,
        carol.device_pubkey,
        carol.secret_key,
        Some(carol.device_id.clone()),
    )?;
    let second_event = codec::invite_response_event(&second_response)?;
    let second_incoming = codec::parse_invite_response_event(&second_event)?;

    let mut second_process_ctx = context(11_103, 1_700_401_013);
    let second_process = fixture.owned_invite.process_invite_response(
        &mut second_process_ctx,
        &second_incoming,
        fixture.alice.secret_key,
    );
    assert!(matches!(
        second_process,
        Err(Error::Domain(DomainError::InviteExhausted))
    ));
    assert_eq!(fixture.owned_invite.used_by, vec![fixture.bob.device_pubkey]);
    Ok(())
}

#[test]
fn unbounded_invite_can_bootstrap_multiple_independent_sessions() -> Result<()> {
    let alice = actor(84, "alice-device");
    let bob = actor(85, "bob-device");
    let carol = actor(86, "carol-device");

    let mut invite_ctx = context(11_200, 1_700_402_000);
    let mut invite = Invite::create_new(
        &mut invite_ctx,
        alice.owner_pubkey,
        Some(alice.device_id.clone()),
        None,
    )?;
    let public_invite = codec::parse_invite_url(&codec::invite_url(&invite, ROOT_URL)?)?;

    let mut bob_accept_ctx = context(11_201, 1_700_402_001);
    let (mut bob_session, bob_response) = public_invite.accept(
        &mut bob_accept_ctx,
        bob.device_pubkey,
        bob.secret_key,
        Some(bob.device_id.clone()),
    )?;
    let bob_event = codec::invite_response_event(&bob_response)?;
    let bob_incoming = codec::parse_invite_response_event(&bob_event)?;
    let mut bob_process_ctx = context(11_202, 1_700_402_002);
    let mut alice_bob_session = invite
        .process_invite_response(&mut bob_process_ctx, &bob_incoming, alice.secret_key)?
        .expect("expected bob invite response")
        .session;

    let mut carol_accept_ctx = context(11_203, 1_700_402_003);
    let (mut carol_session, carol_response) = public_invite.accept(
        &mut carol_accept_ctx,
        carol.device_pubkey,
        carol.secret_key,
        Some(carol.device_id.clone()),
    )?;
    let carol_event = codec::invite_response_event(&carol_response)?;
    let carol_incoming = codec::parse_invite_response_event(&carol_event)?;
    let mut carol_process_ctx = context(11_204, 1_700_402_004);
    let mut alice_carol_session = invite
        .process_invite_response(&mut carol_process_ctx, &carol_incoming, alice.secret_key)?
        .expect("expected carol invite response")
        .session;

    let expected_used_by = {
        let mut used_by = vec![bob.device_pubkey, carol.device_pubkey];
        used_by.sort();
        used_by
    };
    assert_eq!(invite.used_by, expected_used_by);

    let mut bob_send_ctx = context(11_205, 1_700_402_005);
    let bob_sent = send_message(
        &mut bob_session,
        &mut bob_send_ctx,
        nostr_double_ratchet::DirectMessageContent::Text("from-bob".to_string()),
    )?;
    let mut alice_bob_recv_ctx = context(11_206, 1_700_402_006);
    let bob_received =
        receive_event(&mut alice_bob_session, &mut alice_bob_recv_ctx, &bob_sent.event)?;
    assert_eq!(bob_received.content, "from-bob");

    let mut carol_send_ctx = context(11_207, 1_700_402_007);
    let carol_sent = send_message(
        &mut carol_session,
        &mut carol_send_ctx,
        nostr_double_ratchet::DirectMessageContent::Text("from-carol".to_string()),
    )?;
    let mut alice_carol_recv_ctx = context(11_208, 1_700_402_008);
    let carol_received = receive_event(
        &mut alice_carol_session,
        &mut alice_carol_recv_ctx,
        &carol_sent.event,
    )?;
    assert_eq!(carol_received.content, "from-carol");

    let mut cross_ctx = context(11_209, 1_700_402_009);
    let wrong_bob = receive_event(&mut alice_bob_session, &mut cross_ctx, &carol_sent.event);
    assert!(wrong_bob.is_err());

    let mut cross_ctx = context(11_210, 1_700_402_010);
    let wrong_carol = receive_event(&mut alice_carol_session, &mut cross_ctx, &bob_sent.event);
    assert!(wrong_carol.is_err());
    Ok(())
}

#[test]
fn invite_response_replay_is_rejected_without_duplicate_effects() -> Result<()> {
    let mut fixture = invite_response_fixture(1_700_403_000, None)?;

    let mut process_ctx = context(11_300, 1_700_403_010);
    let first = fixture
        .owned_invite
        .process_invite_response(
            &mut process_ctx,
            &fixture.incoming_response,
            fixture.alice.secret_key,
        )?
        .expect("expected invite response");
    let mut alice_session = first.session;
    let after_first = snapshot(&fixture.owned_invite);

    let mut replay_ctx = context(11_301, 1_700_403_011);
    let replay = fixture.owned_invite.process_invite_response(
        &mut replay_ctx,
        &fixture.incoming_response,
        fixture.alice.secret_key,
    );
    assert!(matches!(
        replay,
        Err(Error::Domain(DomainError::InviteAlreadyUsed))
    ));
    assert_eq!(snapshot(&fixture.owned_invite), after_first);

    let mut send_ctx = context(11_302, 1_700_403_012);
    let sent = send_message(
        &mut fixture.bob_session,
        &mut send_ctx,
        nostr_double_ratchet::DirectMessageContent::Text("replay-safe".to_string()),
    )?;
    let mut recv_ctx = context(11_303, 1_700_403_013);
    let received = receive_event(&mut alice_session, &mut recv_ctx, &sent.event)?;
    assert_eq!(received.content, "replay-safe");
    Ok(())
}

#[test]
fn malformed_invite_response_layers_fail_independently() -> Result<()> {
    for (index, corruption) in [
        InviteResponseCorruption::OuterEnvelope,
        InviteResponseCorruption::InnerBase64,
        InviteResponseCorruption::InnerJson,
        InviteResponseCorruption::PayloadJson,
        InviteResponseCorruption::InvalidSessionKey,
    ]
    .into_iter()
    .enumerate()
    {
        let mut fixture = invite_response_fixture(1_700_404_000 + index as u64 * 10, None)?;
        let corrupted = corrupt_invite_response_layer(
            &fixture.owned_invite,
            &fixture.response_envelope,
            &fixture.bob,
            corruption,
        )?;
        let before = snapshot(&fixture.owned_invite);

        let mut bad_ctx = context(11_400 + index as u64, 1_700_404_100 + index as u64);
        let result = fixture.owned_invite.process_invite_response(
            &mut bad_ctx,
            &corrupted,
            fixture.alice.secret_key,
        );
        assert!(result.is_err(), "expected {corruption:?} to fail");
        assert_eq!(snapshot(&fixture.owned_invite), before);

        let mut good_ctx = context(11_500 + index as u64, 1_700_404_200 + index as u64);
        let response = fixture
            .owned_invite
            .process_invite_response(
                &mut good_ctx,
                &fixture.incoming_response,
                fixture.alice.secret_key,
            )?
            .expect("valid response should still succeed after failed corruption");
        let mut alice_session = response.session;

        let mut send_ctx = context(11_600 + index as u64, 1_700_404_300 + index as u64);
        let sent = send_message(
            &mut fixture.bob_session,
            &mut send_ctx,
            nostr_double_ratchet::DirectMessageContent::Text(format!("usable-{index}")),
        )?;
        let mut recv_ctx = context(11_700 + index as u64, 1_700_404_400 + index as u64);
        let received = receive_event(&mut alice_session, &mut recv_ctx, &sent.event)?;
        assert_eq!(received.content, format!("usable-{index}"));
    }

    Ok(())
}

#[test]
fn invite_metadata_public_surface_is_stable() -> Result<()> {
    let alice = actor(87, "alice-device");
    let claimed_owner = actor(88, "claimed-owner-device");

    let mut invite_ctx = context(11_800, 1_700_405_000);
    let mut invite = Invite::create_new(
        &mut invite_ctx,
        alice.owner_pubkey,
        Some(alice.device_id.clone()),
        None,
    )?;
    invite.purpose = Some("metadata-test".to_string());
    invite.owner_public_key = Some(claimed_owner.owner_pubkey);

    let owned_roundtrip: Invite = serde_json::from_str(&serde_json::to_string(&invite).unwrap())?;
    assert_eq!(owned_roundtrip.device_id, invite.device_id);
    assert_eq!(owned_roundtrip.purpose, invite.purpose);
    assert_eq!(owned_roundtrip.owner_public_key, invite.owner_public_key);
    assert_eq!(
        owned_roundtrip.inviter_ephemeral_private_key,
        invite.inviter_ephemeral_private_key
    );
    assert!(owned_roundtrip.inviter_ephemeral_private_key.is_some());

    let url = codec::invite_url(&invite, ROOT_URL)?;
    let parsed_url = codec::parse_invite_url(&url)?;
    assert_eq!(parsed_url.device_id, invite.device_id);
    assert_eq!(parsed_url.purpose, invite.purpose);
    assert_eq!(parsed_url.owner_public_key, invite.owner_public_key);
    assert!(parsed_url.inviter_ephemeral_private_key.is_none());

    let signed_event = codec::invite_unsigned_event(&invite)?.sign_with_keys(&alice.keys)?;
    let parsed_event = codec::parse_invite_event(&signed_event)?;
    assert_eq!(parsed_event.device_id, invite.device_id);
    assert_eq!(parsed_event.purpose, invite.purpose);
    assert_eq!(parsed_event.owner_public_key, invite.owner_public_key);
    assert!(parsed_event.inviter_ephemeral_private_key.is_none());
    Ok(())
}
