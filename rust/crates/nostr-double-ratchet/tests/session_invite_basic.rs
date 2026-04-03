mod support;

use nostr_double_ratchet::{
    codec::nostr as codec, AppKeys, DeviceEntry, DirectMessageContent, GroupId, Result,
    CHAT_MESSAGE_KIND, REACTION_KIND, RECEIPT_KIND, TYPING_KIND, UnixSeconds, MAX_SKIP,
};
use support::{
    actor, assert_rumor_eq, bootstrap_via_invite_event, bootstrap_via_invite_url, context,
    direct_session_pair, receive_event, restore_session, send_message, snapshot,
};

#[test]
fn invite_url_bootstrap_first_message_end_to_end() -> Result<()> {
    let mut boot = bootstrap_via_invite_url(1_700_100_000)?;

    let mut send_ctx = context(1, 1_700_100_010);
    let sent = send_message(
        &mut boot.bob_session,
        &mut send_ctx,
        DirectMessageContent::Text("hello via url".to_string()),
    )?;

    let mut recv_ctx = context(2, 1_700_100_011);
    let received = receive_event(&mut boot.alice_session, &mut recv_ctx, &sent.event)?;
    assert_rumor_eq(&received, &sent.rumor);
    Ok(())
}

#[test]
fn invite_event_bootstrap_first_message_end_to_end() -> Result<()> {
    let mut boot = bootstrap_via_invite_event(1_700_100_100)?;

    let mut send_ctx = context(3, 1_700_100_110);
    let sent = send_message(
        &mut boot.bob_session,
        &mut send_ctx,
        DirectMessageContent::Text("hello via event".to_string()),
    )?;

    let mut recv_ctx = context(4, 1_700_100_111);
    let received = receive_event(&mut boot.alice_session, &mut recv_ctx, &sent.event)?;
    assert_rumor_eq(&received, &sent.rumor);
    Ok(())
}

#[test]
fn post_bootstrap_bidirectional_ping_pong_over_many_turns() -> Result<()> {
    let mut boot = bootstrap_via_invite_url(1_700_100_200)?;
    let mut seen_ids = std::collections::BTreeSet::new();

    for (index, text) in [
        "m1", "m2", "m3", "m4", "m5", "m6", "m7", "m8", "m9", "m10",
    ]
    .into_iter()
    .enumerate()
    {
        let from_bob = index % 2 == 0;
        let secs = 1_700_100_210 + index as u64 * 2;
        if from_bob {
            let mut send_ctx = context(10 + index as u64, secs);
            let sent = send_message(
                &mut boot.bob_session,
                &mut send_ctx,
                DirectMessageContent::Text(text.to_string()),
            )?;
            let bob_state = snapshot(&boot.bob_session.state);
            let alice_state_before_receive = snapshot(&boot.alice_session.state);
            let mut recv_ctx = context(100 + index as u64, secs + 1);
            let received = match receive_event(&mut boot.alice_session, &mut recv_ctx, &sent.event)
            {
                Ok(received) => received,
                Err(err) => panic!(
                    "alice receive failed at turn {index}: {err:?}\nBob state after send: {bob_state}\nAlice state before receive: {alice_state_before_receive}"
                ),
            };
            assert_rumor_eq(&received, &sent.rumor);
            assert!(seen_ids.insert(sent.rumor.id.clone().unwrap()));
        } else {
            let mut send_ctx = context(10 + index as u64, secs);
            let sent = send_message(
                &mut boot.alice_session,
                &mut send_ctx,
                DirectMessageContent::Text(text.to_string()),
            )?;
            let alice_state = snapshot(&boot.alice_session.state);
            let bob_state_before_receive = snapshot(&boot.bob_session.state);
            let mut recv_ctx = context(100 + index as u64, secs + 1);
            let received = match receive_event(&mut boot.bob_session, &mut recv_ctx, &sent.event) {
                Ok(received) => received,
                Err(err) => panic!(
                    "bob receive failed at turn {index}: {err:?}\nAlice state after send: {alice_state}\nBob state before receive: {bob_state_before_receive}"
                ),
            };
            assert_rumor_eq(&received, &sent.rumor);
            assert!(seen_ids.insert(sent.rumor.id.clone().unwrap()));
        }
    }

    assert_eq!(seen_ids.len(), 10);
    Ok(())
}

#[test]
fn same_sender_burst_before_reply() -> Result<()> {
    let (_alice, _bob, mut alice_session, mut bob_session) = direct_session_pair(1, 2, 1_700_100_300)?;

    let mut sent_messages = Vec::new();
    for index in 0..5 {
        let mut send_ctx = context(200 + index, 1_700_100_310 + index);
        let sent = send_message(
            &mut alice_session,
            &mut send_ctx,
            DirectMessageContent::Text(format!("burst-{index}")),
        )?;
        sent_messages.push(sent);
    }

    for (index, sent) in sent_messages.iter().enumerate() {
        let mut recv_ctx = context(300 + index as u64, 1_700_100_320 + index as u64);
        let received = receive_event(&mut bob_session, &mut recv_ctx, &sent.event)?;
        assert_rumor_eq(&received, &sent.rumor);
    }

    let mut reply_ctx = context(400, 1_700_100_400);
    let reply = send_message(
        &mut bob_session,
        &mut reply_ctx,
        DirectMessageContent::Text("reply".to_string()),
    )?;
    let mut recv_ctx = context(401, 1_700_100_401);
    let received = receive_event(&mut alice_session, &mut recv_ctx, &reply.event)?;
    assert_rumor_eq(&received, &reply.rumor);
    Ok(())
}

#[test]
fn out_of_order_within_single_chain_recovers_skipped_messages() -> Result<()> {
    let (_alice, _bob, mut alice_session, mut bob_session) = direct_session_pair(3, 4, 1_700_100_500)?;

    let mut sent = Vec::new();
    for index in 0..4 {
        let mut send_ctx = context(500 + index, 1_700_100_510 + index);
        sent.push(send_message(
            &mut alice_session,
            &mut send_ctx,
            DirectMessageContent::Text(format!("ooo-{index}")),
        )?);
    }

    for (receive_index, sent_index) in [3usize, 1, 0, 2].into_iter().enumerate() {
        let mut recv_ctx = context(600 + receive_index as u64, 1_700_100_520 + receive_index as u64);
        let received = receive_event(&mut bob_session, &mut recv_ctx, &sent[sent_index].event)?;
        assert_rumor_eq(&received, &sent[sent_index].rumor);
    }

    Ok(())
}

#[test]
fn cross_chain_out_of_order_after_dh_ratchet_recovers_previous_chain_messages() -> Result<()> {
    let (_alice, _bob, mut alice_session, mut bob_session) = direct_session_pair(5, 6, 1_700_100_600)?;

    let mut send_ctx = context(700, 1_700_100_610);
    let old_1 = send_message(
        &mut alice_session,
        &mut send_ctx,
        DirectMessageContent::Text("old-1".to_string()),
    )?;
    let mut send_ctx = context(701, 1_700_100_611);
    let old_2 = send_message(
        &mut alice_session,
        &mut send_ctx,
        DirectMessageContent::Text("old-2".to_string()),
    )?;

    let mut recv_ctx = context(702, 1_700_100_612);
    let received_old_1 = receive_event(&mut bob_session, &mut recv_ctx, &old_1.event)?;
    assert_rumor_eq(&received_old_1, &old_1.rumor);

    let mut bob_reply_ctx = context(703, 1_700_100_613);
    let bob_reply = send_message(
        &mut bob_session,
        &mut bob_reply_ctx,
        DirectMessageContent::Text("reply-1".to_string()),
    )?;
    let mut alice_recv_ctx = context(704, 1_700_100_614);
    let alice_received_reply = receive_event(&mut alice_session, &mut alice_recv_ctx, &bob_reply.event)?;
    assert_rumor_eq(&alice_received_reply, &bob_reply.rumor);

    let mut new_chain_ctx = context(705, 1_700_100_615);
    let new_chain = send_message(
        &mut alice_session,
        &mut new_chain_ctx,
        DirectMessageContent::Text("new-chain".to_string()),
    )?;

    let mut recv_ctx = context(706, 1_700_100_616);
    let received_new_chain = match receive_event(&mut bob_session, &mut recv_ctx, &new_chain.event)
    {
        Ok(received) => received,
        Err(err) => panic!("bob failed to receive new chain message: {err:?}"),
    };
    assert_rumor_eq(&received_new_chain, &new_chain.rumor);

    let mut recv_ctx = context(707, 1_700_100_617);
    let received_old_2 = match receive_event(&mut bob_session, &mut recv_ctx, &old_2.event) {
        Ok(received) => received,
        Err(err) => panic!("bob failed to receive delayed prior-chain message: {err:?}"),
    };
    assert_rumor_eq(&received_old_2, &old_2.rumor);

    let mut final_reply_ctx = context(708, 1_700_100_618);
    let final_reply = send_message(
        &mut bob_session,
        &mut final_reply_ctx,
        DirectMessageContent::Text("reply-2".to_string()),
    )?;
    let mut recv_ctx = context(709, 1_700_100_619);
    let received_final_reply = receive_event(&mut alice_session, &mut recv_ctx, &final_reply.event)?;
    assert_rumor_eq(&received_final_reply, &final_reply.rumor);
    Ok(())
}

#[test]
fn session_state_serde_roundtrip_mid_conversation() -> Result<()> {
    let (_alice, _bob, mut alice_session, mut bob_session) = direct_session_pair(7, 8, 1_700_100_700)?;

    for index in 0..3 {
        let mut send_ctx = context(800 + index, 1_700_100_710 + index);
        let sent = send_message(
            &mut alice_session,
            &mut send_ctx,
            DirectMessageContent::Text(format!("before-{index}")),
        )?;
        let mut recv_ctx = context(900 + index, 1_700_100_720 + index);
        let received = receive_event(&mut bob_session, &mut recv_ctx, &sent.event)?;
        assert_rumor_eq(&received, &sent.rumor);
    }

    alice_session = restore_session(&alice_session.state, "alice-restored");
    bob_session = restore_session(&bob_session.state, "bob-restored");

    let mut bob_send_ctx = context(950, 1_700_100_750);
    let sent = send_message(
        &mut bob_session,
        &mut bob_send_ctx,
        DirectMessageContent::Text("after-restore".to_string()),
    )?;
    let mut recv_ctx = context(951, 1_700_100_751);
    let received = receive_event(&mut alice_session, &mut recv_ctx, &sent.event)?;
    assert_rumor_eq(&received, &sent.rumor);
    Ok(())
}

#[test]
fn owned_invite_serde_roundtrip_preserves_bootstrap_capability() -> Result<()> {
    let alice = actor(13, "alice-device");
    let bob = actor(14, "bob-device");
    let mut invite_ctx = context(1000, 1_700_100_800);
    let invite = nostr_double_ratchet::Invite::create_new(
        &mut invite_ctx,
        alice.owner_pubkey,
        Some(alice.device_id.clone()),
        None,
    )?;
    let mut restored_owned_invite: nostr_double_ratchet::Invite =
        serde_json::from_str(&serde_json::to_string(&invite).unwrap()).unwrap();

    let url = codec::invite_url(&restored_owned_invite, support::ROOT_URL)?;
    let public_invite = codec::parse_invite_url(&url)?;

    let mut bob_accept_ctx = context(1001, 1_700_100_801);
    let (mut bob_session, response_envelope) = public_invite.accept(
        &mut bob_accept_ctx,
        bob.device_pubkey,
        bob.secret_key,
        Some(bob.device_id.clone()),
    )?;
    let response_event = codec::invite_response_event(&response_envelope)?;
    let incoming_response = codec::parse_invite_response_event(&response_event)?;

    let mut alice_process_ctx = context(1002, 1_700_100_802);
    let invite_response = restored_owned_invite
        .process_invite_response(&mut alice_process_ctx, &incoming_response, alice.secret_key)?
        .expect("expected response");
    let mut alice_session = invite_response.session;

    let mut send_ctx = context(1003, 1_700_100_803);
    let sent = send_message(
        &mut bob_session,
        &mut send_ctx,
        DirectMessageContent::Text("serde-owned-invite".to_string()),
    )?;
    let mut recv_ctx = context(1004, 1_700_100_804);
    let received = receive_event(&mut alice_session, &mut recv_ctx, &sent.event)?;
    assert_rumor_eq(&received, &sent.rumor);
    Ok(())
}

#[test]
fn invite_owner_claim_with_appkeys_proof_verifies() -> Result<()> {
    let alice = actor(15, "alice-device");
    let bob = actor(16, "bob-device");
    let claimed_owner = actor(17, "owner-device");

    let mut invite_ctx = context(1100, 1_700_100_900);
    let mut owned_invite = nostr_double_ratchet::Invite::create_new(
        &mut invite_ctx,
        alice.owner_pubkey,
        Some(alice.device_id.clone()),
        None,
    )?;
    let public_invite = codec::parse_invite_url(&codec::invite_url(&owned_invite, support::ROOT_URL)?)?;

    let mut bob_accept_ctx = context(1101, 1_700_100_901);
    let (_bob_session, response_envelope) = public_invite.accept_with_owner(
        &mut bob_accept_ctx,
        bob.device_pubkey,
        bob.secret_key,
        Some(bob.device_id.clone()),
        Some(claimed_owner.owner_pubkey),
    )?;
    let response_event = codec::invite_response_event(&response_envelope)?;
    let incoming_response = codec::parse_invite_response_event(&response_event)?;

    let mut alice_process_ctx = context(1102, 1_700_100_902);
    let response = owned_invite
        .process_invite_response(&mut alice_process_ctx, &incoming_response, alice.secret_key)?
        .expect("expected response");

    assert_eq!(response.resolved_owner_pubkey(), claimed_owner.owner_pubkey);
    assert!(!response.has_verified_owner_claim(None));

    let app_keys = AppKeys::new(vec![DeviceEntry {
        identity_pubkey: response.invitee_identity,
        created_at: UnixSeconds(1),
    }]);
    assert!(response.has_verified_owner_claim(Some(&app_keys)));
    Ok(())
}

#[test]
fn message_kind_matrix_survives_full_wire_path() -> Result<()> {
    let (_alice, _bob, mut alice_session, mut bob_session) = direct_session_pair(18, 19, 1_700_101_000)?;

    let mut seed_ctx = context(1200, 1_700_101_001);
    let seed = send_message(
        &mut alice_session,
        &mut seed_ctx,
        DirectMessageContent::Text("seed".to_string()),
    )?;
    let mut recv_ctx = context(1201, 1_700_101_002);
    let received_seed = receive_event(&mut bob_session, &mut recv_ctx, &seed.event)?;
    assert_rumor_eq(&received_seed, &seed.rumor);

    let seed_id = seed.rumor.id.clone().unwrap();

    let cases = vec![
        (
            DirectMessageContent::Reply {
                reply_to: seed_id.clone(),
                text: "reply".to_string(),
            },
            CHAT_MESSAGE_KIND,
            "reply".to_string(),
        ),
        (
            DirectMessageContent::Reaction {
                message_id: seed_id.clone(),
                emoji: ":+1:".to_string(),
            },
            REACTION_KIND,
            ":+1:".to_string(),
        ),
        (
            DirectMessageContent::Receipt {
                receipt_type: "seen".to_string(),
                message_ids: vec![seed_id.clone()],
            },
            RECEIPT_KIND,
            "seen".to_string(),
        ),
        (DirectMessageContent::Typing, TYPING_KIND, "typing".to_string()),
    ];

    for (index, (content, kind, body)) in cases.into_iter().enumerate() {
        let mut send_ctx = context(1210 + index as u64, 1_700_101_010 + index as u64);
        let sent = send_message(&mut alice_session, &mut send_ctx, content)?;
        let mut recv_ctx = context(1220 + index as u64, 1_700_101_020 + index as u64);
        let received = receive_event(&mut bob_session, &mut recv_ctx, &sent.event)?;
        assert_rumor_eq(&received, &sent.rumor);
        assert_eq!(received.kind, kind);
        assert_eq!(received.content, body);
        if kind != TYPING_KIND {
            assert!(received.tags.iter().any(|tag| tag.iter().any(|value| value == &seed_id)));
        }
    }

    assert!(MAX_SKIP >= 1000);
    let _ = GroupId::new("unused-but-confirms-public-ids");
    let _ = snapshot(&seed.rumor);
    Ok(())
}
