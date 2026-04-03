mod support;

use nostr_double_ratchet::{
    codec::nostr as codec, AppKeys, DeviceEntry, DirectMessageContent, DomainError, Error, Invite,
    Result, UnixSeconds, MAX_SKIP, MESSAGE_EVENT_KIND,
};
use support::{
    actor, context, direct_session_pair, header_tag, mutate_text, receive_event, receive_message,
    send_message, signed_event, snapshot, ROOT_URL,
};

#[test]
fn replayed_message_is_rejected_and_state_unchanged() -> Result<()> {
    let (_alice, _bob, mut alice_session, mut bob_session) =
        direct_session_pair(21, 22, 1_700_200_000)?;

    let mut send_ctx = context(1, 1_700_200_010);
    let sent = send_message(
        &mut alice_session,
        &mut send_ctx,
        DirectMessageContent::Text("replay".to_string()),
    )?;

    let mut recv_ctx = context(2, 1_700_200_011);
    let first = receive_event(&mut bob_session, &mut recv_ctx, &sent.event)?;
    assert_eq!(first.content, "replay");
    let after_first = snapshot(&bob_session.state);

    let mut replay_ctx = context(3, 1_700_200_012);
    let replay = receive_event(&mut bob_session, &mut replay_ctx, &sent.event);
    assert!(replay.is_err());
    assert_eq!(snapshot(&bob_session.state), after_first);
    Ok(())
}

#[test]
fn tampered_ciphertext_is_rejected_and_state_unchanged() -> Result<()> {
    let (_alice, _bob, mut alice_session, mut bob_session) =
        direct_session_pair(23, 24, 1_700_200_100)?;

    let before = snapshot(&bob_session.state);
    let mut send_ctx = context(4, 1_700_200_110);
    let sent = send_message(
        &mut alice_session,
        &mut send_ctx,
        DirectMessageContent::Text("cipher".to_string()),
    )?;
    let mut tampered = sent.incoming.clone();
    tampered.ciphertext = mutate_text(&tampered.ciphertext);

    let mut recv_ctx = context(5, 1_700_200_111);
    let result = receive_message(&mut bob_session, &mut recv_ctx, &tampered);
    assert!(result.is_err());
    assert_eq!(snapshot(&bob_session.state), before);
    Ok(())
}

#[test]
fn tampered_encrypted_header_is_rejected_and_state_unchanged() -> Result<()> {
    let (_alice, _bob, mut alice_session, mut bob_session) =
        direct_session_pair(25, 26, 1_700_200_200)?;

    let before = snapshot(&bob_session.state);
    let mut send_ctx = context(6, 1_700_200_210);
    let sent = send_message(
        &mut alice_session,
        &mut send_ctx,
        DirectMessageContent::Text("header".to_string()),
    )?;
    let mut tampered = sent.incoming.clone();
    tampered.encrypted_header = mutate_text(&tampered.encrypted_header);

    let mut recv_ctx = context(7, 1_700_200_211);
    let result = receive_message(&mut bob_session, &mut recv_ctx, &tampered);
    assert!(result.is_err());
    assert_eq!(snapshot(&bob_session.state), before);
    Ok(())
}

#[test]
fn wrong_sender_identity_is_rejected_before_decrypt() -> Result<()> {
    let (_alice, _bob, mut alice_session, mut bob_session) =
        direct_session_pair(27, 28, 1_700_200_300)?;
    let impostor = actor(29, "mallory-device");

    let before = snapshot(&bob_session.state);
    let mut send_ctx = context(8, 1_700_200_310);
    let sent = send_message(
        &mut alice_session,
        &mut send_ctx,
        DirectMessageContent::Text("wrong-sender".to_string()),
    )?;
    let mut tampered = sent.incoming.clone();
    tampered.sender = impostor.device_pubkey;

    let mut recv_ctx = context(9, 1_700_200_311);
    let result = receive_message(&mut bob_session, &mut recv_ctx, &tampered);
    assert!(matches!(
        result,
        Err(Error::Domain(DomainError::UnexpectedSender))
    ));
    assert_eq!(snapshot(&bob_session.state), before);
    Ok(())
}

#[test]
fn dm_event_missing_or_wrong_header_tag_fails_parse() -> Result<()> {
    let alice = actor(30, "alice-device");

    let wrong_kind = signed_event(
        alice.secret_key,
        1,
        "ciphertext",
        vec![header_tag("hdr")],
        UnixSeconds(1_700_200_400),
    );
    assert!(codec::parse_direct_message_event(&wrong_kind).is_err());

    let missing_header = signed_event(
        alice.secret_key,
        MESSAGE_EVENT_KIND,
        "ciphertext",
        Vec::new(),
        UnixSeconds(1_700_200_401),
    );
    assert!(codec::parse_direct_message_event(&missing_header).is_err());

    let empty_header = signed_event(
        alice.secret_key,
        MESSAGE_EVENT_KIND,
        "ciphertext",
        vec![header_tag("")],
        UnixSeconds(1_700_200_402),
    );
    assert!(codec::parse_direct_message_event(&empty_header).is_err());
    Ok(())
}

#[test]
fn max_skip_exceeded_is_rejected_and_state_unchanged() -> Result<()> {
    let (_alice, _bob, mut alice_session, mut bob_session) =
        direct_session_pair(31, 32, 1_700_200_500)?;
    let before = snapshot(&bob_session.state);

    let mut last = None;
    for index in 0..(MAX_SKIP as u64 + 2) {
        let mut send_ctx = context(100 + index, 1_700_200_510 + index);
        last = Some(send_message(
            &mut alice_session,
            &mut send_ctx,
            DirectMessageContent::Text(format!("gap-{index}")),
        )?);
    }

    let mut recv_ctx = context(999, 1_700_200_999);
    let result = receive_event(
        &mut bob_session,
        &mut recv_ctx,
        &last.expect("last event").event,
    );
    assert!(matches!(
        result,
        Err(Error::Domain(DomainError::TooManySkippedMessages))
    ));
    assert_eq!(snapshot(&bob_session.state), before);
    Ok(())
}

#[test]
fn wrong_inviter_private_key_cannot_process_response() -> Result<()> {
    let alice = actor(33, "alice-device");
    let bob = actor(34, "bob-device");
    let wrong_alice = actor(35, "wrong-alice-device");

    let mut invite_ctx = context(11, 1_700_200_600);
    let mut owned_invite = Invite::create_new(
        &mut invite_ctx,
        alice.owner_pubkey,
        Some(alice.device_id.clone()),
        None,
    )?;
    let public_invite = codec::parse_invite_url(&codec::invite_url(&owned_invite, ROOT_URL)?)?;

    let mut accept_ctx = context(12, 1_700_200_601);
    let (_bob_session, response_envelope) = public_invite.accept(
        &mut accept_ctx,
        bob.device_pubkey,
        bob.secret_key,
        Some(bob.device_id.clone()),
    )?;
    let response_event = codec::invite_response_event(&response_envelope)?;
    let incoming_response = codec::parse_invite_response_event(&response_event)?;

    let mut process_ctx = context(13, 1_700_200_602);
    let result = owned_invite.process_invite_response(
        &mut process_ctx,
        &incoming_response,
        wrong_alice.secret_key,
    );
    assert!(result.is_err());
    Ok(())
}

#[test]
fn tampered_invite_response_is_rejected_and_invite_state_stays_usable() -> Result<()> {
    let alice = actor(36, "alice-device");
    let bob = actor(37, "bob-device");

    let mut invite_ctx = context(14, 1_700_200_700);
    let mut owned_invite = Invite::create_new(
        &mut invite_ctx,
        alice.owner_pubkey,
        Some(alice.device_id.clone()),
        None,
    )?;
    let public_invite = codec::parse_invite_url(&codec::invite_url(&owned_invite, ROOT_URL)?)?;

    let mut accept_ctx = context(15, 1_700_200_701);
    let (mut bob_session, response_envelope) = public_invite.accept(
        &mut accept_ctx,
        bob.device_pubkey,
        bob.secret_key,
        Some(bob.device_id.clone()),
    )?;
    let response_event = codec::invite_response_event(&response_envelope)?;
    let incoming_response = codec::parse_invite_response_event(&response_event)?;

    let mut tampered_response = incoming_response.clone();
    tampered_response.content = mutate_text(&tampered_response.content);

    let mut tampered_ctx = context(16, 1_700_200_702);
    let tampered = owned_invite.process_invite_response(
        &mut tampered_ctx,
        &tampered_response,
        alice.secret_key,
    );
    assert!(tampered.is_err());

    let mut process_ctx = context(17, 1_700_200_703);
    let invite_response = owned_invite
        .process_invite_response(&mut process_ctx, &incoming_response, alice.secret_key)?
        .expect("valid response");
    let mut alice_session = invite_response.session;

    let mut send_ctx = context(18, 1_700_200_704);
    let sent = send_message(
        &mut bob_session,
        &mut send_ctx,
        DirectMessageContent::Text("usable".to_string()),
    )?;
    let mut recv_ctx = context(19, 1_700_200_705);
    let received = receive_event(&mut alice_session, &mut recv_ctx, &sent.event)?;
    assert_eq!(received.content, "usable");
    Ok(())
}

#[test]
fn public_only_invite_cannot_process_response() -> Result<()> {
    let alice = actor(38, "alice-device");
    let bob = actor(39, "bob-device");

    let mut invite_ctx = context(20, 1_700_200_800);
    let owned_invite = Invite::create_new(
        &mut invite_ctx,
        alice.owner_pubkey,
        Some(alice.device_id.clone()),
        None,
    )?;
    let mut public_url_invite =
        codec::parse_invite_url(&codec::invite_url(&owned_invite, ROOT_URL)?)?;
    let invite_event = codec::invite_unsigned_event(&owned_invite)?.sign_with_keys(&alice.keys)?;
    let mut public_event_invite = codec::parse_invite_event(&invite_event)?;

    let mut accept_ctx = context(21, 1_700_200_801);
    let (_bob_session, response_envelope) = public_url_invite.accept(
        &mut accept_ctx,
        bob.device_pubkey,
        bob.secret_key,
        Some(bob.device_id.clone()),
    )?;
    let response_event = codec::invite_response_event(&response_envelope)?;
    let incoming_response = codec::parse_invite_response_event(&response_event)?;

    let mut process_ctx = context(22, 1_700_200_802);
    let url_result = public_url_invite.process_invite_response(
        &mut process_ctx,
        &incoming_response,
        alice.secret_key,
    );
    assert!(url_result.is_err());

    let mut process_ctx = context(23, 1_700_200_803);
    let event_result = public_event_invite.process_invite_response(
        &mut process_ctx,
        &incoming_response,
        alice.secret_key,
    );
    assert!(event_result.is_err());
    Ok(())
}

#[test]
fn forged_owner_claim_without_appkeys_proof_stays_unverified() -> Result<()> {
    let alice = actor(40, "alice-device");
    let bob = actor(41, "bob-device");
    let forged_owner = actor(42, "forged-owner-device");
    let unrelated_device = actor(43, "unrelated-device");

    let mut invite_ctx = context(24, 1_700_200_900);
    let mut owned_invite = Invite::create_new(
        &mut invite_ctx,
        alice.owner_pubkey,
        Some(alice.device_id.clone()),
        None,
    )?;
    let public_invite = codec::parse_invite_url(&codec::invite_url(&owned_invite, ROOT_URL)?)?;

    let mut accept_ctx = context(25, 1_700_200_901);
    let (_bob_session, response_envelope) = public_invite.accept_with_owner(
        &mut accept_ctx,
        bob.device_pubkey,
        bob.secret_key,
        Some(bob.device_id.clone()),
        Some(forged_owner.owner_pubkey),
    )?;
    let response_event = codec::invite_response_event(&response_envelope)?;
    let incoming_response = codec::parse_invite_response_event(&response_event)?;

    let mut process_ctx = context(26, 1_700_200_902);
    let response = owned_invite
        .process_invite_response(&mut process_ctx, &incoming_response, alice.secret_key)?
        .expect("expected response");

    assert_eq!(response.resolved_owner_pubkey(), forged_owner.owner_pubkey);
    assert!(!response.has_verified_owner_claim(None));

    let unrelated_app_keys = AppKeys::new(vec![DeviceEntry {
        identity_pubkey: unrelated_device.device_pubkey,
        created_at: UnixSeconds(1),
    }]);
    assert!(!response.has_verified_owner_claim(Some(&unrelated_app_keys)));
    Ok(())
}
