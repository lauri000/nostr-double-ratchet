mod support;

use nostr_double_ratchet::{CodecError, Error, Result, MAX_SKIP};
use support::{
    checkpoint_session, context, direct_session_pair, mutate_text, receive_message,
    restore_session, snapshot, DeliveryScript, Side,
};

fn assert_receive_plan_pure(
    session: &nostr_double_ratchet::Session,
    incoming: &nostr_double_ratchet::IncomingDirectMessageEnvelope,
    seed: u64,
    now_secs: u64,
) -> Result<()> {
    let before = snapshot(&session.state);

    let mut ctx = context(seed, now_secs);
    let first = session.plan_receive(&mut ctx, incoming)?;

    let mut ctx = context(seed, now_secs);
    let second = session.plan_receive(&mut ctx, incoming)?;

    assert_eq!(snapshot(&session.state), before);
    assert_eq!(snapshot(&first.next_state), snapshot(&second.next_state));
    assert_eq!(first.sender, second.sender);
    assert_eq!(first.rumor.id, second.rumor.id);
    assert_eq!(first.rumor.content, second.rumor.content);
    Ok(())
}

#[test]
fn long_mixed_schedule_survives_many_ratchet_turns() -> Result<()> {
    let (_alice, _bob, mut alice_session, mut bob_session) =
        direct_session_pair(61, 62, 1_700_300_000)?;
    let mut script = DeliveryScript::new(5_000, 1_700_300_010);

    let rounds: Vec<(Side, Vec<usize>)> = vec![
        (Side::Alice, vec![0, 3, 1, 2]),
        (Side::Bob, vec![2, 0, 1]),
        (Side::Alice, vec![4, 1, 3, 0, 2]),
        (Side::Bob, vec![1, 3, 0, 2]),
        (Side::Alice, vec![2, 0, 1]),
        (Side::Bob, vec![4, 2, 0, 3, 1]),
        (Side::Alice, vec![1, 3, 0, 2]),
        (Side::Bob, vec![2, 0, 1]),
        (Side::Alice, vec![4, 1, 3, 0, 2]),
        (Side::Bob, vec![1, 3, 0, 2]),
        (Side::Alice, vec![2, 0, 1]),
        (Side::Bob, vec![4, 2, 0, 3, 1]),
        (Side::Alice, vec![0, 3, 1, 2]),
        (Side::Bob, vec![2, 0, 1]),
    ];

    let mut expected_transcript = Vec::new();
    let mut received_transcript = Vec::new();

    for (round_index, (side, order)) in rounds.into_iter().enumerate() {
        let mut ids = Vec::new();
        for burst_index in 0..order.len() {
            let id = script.send_text(
                side,
                &mut alice_session,
                &mut bob_session,
                format!("round-{round_index}-{:?}-msg-{burst_index}", side),
            )?;
            ids.push(id);
        }

        for order_index in order {
            let id = ids[order_index];
            let expected_text = script.sent(id).rumor.content.clone();
            let received = script.deliver(id, &mut alice_session, &mut bob_session)?;
            expected_transcript.push(expected_text.clone());
            received_transcript.push(received.content.clone());
            assert_eq!(received.content, expected_text);
        }

        if round_index == 6 {
            let alice_checkpoint = checkpoint_session(&alice_session);
            let bob_checkpoint = checkpoint_session(&bob_session);
            alice_session = restore_session(&alice_checkpoint, "alice-stress-restored");
            bob_session = restore_session(&bob_checkpoint, "bob-stress-restored");
        }
    }

    assert_eq!(received_transcript.len(), 55);
    assert_eq!(received_transcript, expected_transcript);
    Ok(())
}

#[test]
fn exact_max_skip_boundary_is_accepted() -> Result<()> {
    let (_alice, _bob, mut alice_session, mut bob_session) =
        direct_session_pair(63, 64, 1_700_301_000)?;
    let mut script = DeliveryScript::new(6_000, 1_700_301_010);

    let mut ids = Vec::new();
    for index in 0..=MAX_SKIP {
        ids.push(script.send_text(
            Side::Alice,
            &mut alice_session,
            &mut bob_session,
            format!("boundary-{index}"),
        )?);
    }

    let latest = script.deliver(ids[MAX_SKIP], &mut alice_session, &mut bob_session)?;
    assert_eq!(latest.content, format!("boundary-{MAX_SKIP}"));

    for index in (0..MAX_SKIP).rev() {
        let received = script.deliver(ids[index], &mut alice_session, &mut bob_session)?;
        assert_eq!(received.content, format!("boundary-{index}"));

        let replay = script.replay(ids[index], &mut alice_session, &mut bob_session);
        assert!(replay.is_err());
    }

    let replay_latest = script.replay(ids[MAX_SKIP], &mut alice_session, &mut bob_session);
    assert!(replay_latest.is_err());

    let reply_id = script.send_text(
        Side::Bob,
        &mut alice_session,
        &mut bob_session,
        "post-boundary",
    )?;
    let reply = script.deliver(reply_id, &mut alice_session, &mut bob_session)?;
    assert_eq!(reply.content, "post-boundary");
    Ok(())
}

#[test]
fn skipped_keys_are_single_use_under_multiple_late_deliveries() -> Result<()> {
    let (_alice, _bob, mut alice_session, mut bob_session) =
        direct_session_pair(65, 66, 1_700_302_000)?;
    let mut script = DeliveryScript::new(7_000, 1_700_302_010);

    let mut ids = Vec::new();
    for index in 0..6 {
        ids.push(script.send_text(
            Side::Alice,
            &mut alice_session,
            &mut bob_session,
            format!("late-{index}"),
        )?);
    }

    let latest = script.deliver(ids[5], &mut alice_session, &mut bob_session)?;
    assert_eq!(latest.content, "late-5");

    for index in [2usize, 0, 4, 1, 3] {
        let received = script.deliver(ids[index], &mut alice_session, &mut bob_session)?;
        assert_eq!(received.content, format!("late-{index}"));

        let replay = script.replay(ids[index], &mut alice_session, &mut bob_session);
        assert!(replay.is_err());
    }

    let reply_id = script.send_text(
        Side::Bob,
        &mut alice_session,
        &mut bob_session,
        "late-reply",
    )?;
    let reply = script.deliver(reply_id, &mut alice_session, &mut bob_session)?;
    assert_eq!(reply.content, "late-reply");
    Ok(())
}

#[test]
fn previous_header_key_is_only_retained_one_ratchet_deep() -> Result<()> {
    let (_alice, _bob, mut alice_session, mut bob_session) =
        direct_session_pair(67, 68, 1_700_303_000)?;
    let mut script = DeliveryScript::new(8_000, 1_700_303_010);

    let first = script.send_text(Side::Alice, &mut alice_session, &mut bob_session, "first")?;
    let delayed_one = script.send_text(
        Side::Alice,
        &mut alice_session,
        &mut bob_session,
        "delayed-one-ratchet",
    )?;
    let delayed_two = script.send_text(
        Side::Alice,
        &mut alice_session,
        &mut bob_session,
        "delayed-two-ratchets",
    )?;
    let _ = script.deliver(first, &mut alice_session, &mut bob_session)?;

    let bob_reply_one =
        script.send_text(Side::Bob, &mut alice_session, &mut bob_session, "reply-one")?;
    let _ = script.deliver(bob_reply_one, &mut alice_session, &mut bob_session)?;

    let chain_one = script.send_text(
        Side::Alice,
        &mut alice_session,
        &mut bob_session,
        "chain-one",
    )?;
    let _ = script.deliver(chain_one, &mut alice_session, &mut bob_session)?;

    let previous_ok = script.deliver(delayed_one, &mut alice_session, &mut bob_session)?;
    assert_eq!(previous_ok.content, "delayed-one-ratchet");

    let bob_reply_two =
        script.send_text(Side::Bob, &mut alice_session, &mut bob_session, "reply-two")?;
    let _ = script.deliver(bob_reply_two, &mut alice_session, &mut bob_session)?;

    let chain_two = script.send_text(
        Side::Alice,
        &mut alice_session,
        &mut bob_session,
        "chain-two",
    )?;
    let _ = script.deliver(chain_two, &mut alice_session, &mut bob_session)?;

    let two_deep = script.deliver(delayed_two, &mut alice_session, &mut bob_session);
    assert!(matches!(
        two_deep,
        Err(Error::Codec(CodecError::InvalidHeader))
    ));
    Ok(())
}

#[test]
fn invalid_previous_chain_message_does_not_poison_future_messages() -> Result<()> {
    let (_alice, _bob, mut alice_session, mut bob_session) =
        direct_session_pair(69, 70, 1_700_304_000)?;
    let mut script = DeliveryScript::new(9_000, 1_700_304_010);

    let first = script.send_text(Side::Alice, &mut alice_session, &mut bob_session, "first")?;
    let delayed_previous = script.send_text(
        Side::Alice,
        &mut alice_session,
        &mut bob_session,
        "delayed-previous",
    )?;
    let _ = script.deliver(first, &mut alice_session, &mut bob_session)?;

    let bob_reply = script.send_text(Side::Bob, &mut alice_session, &mut bob_session, "reply")?;
    let _ = script.deliver(bob_reply, &mut alice_session, &mut bob_session)?;

    let current_chain = script.send_text(
        Side::Alice,
        &mut alice_session,
        &mut bob_session,
        "current-chain",
    )?;
    let _ = script.deliver(current_chain, &mut alice_session, &mut bob_session)?;

    let before = snapshot(&bob_session.state);
    let mut tampered = script.sent(delayed_previous).incoming.clone();
    tampered.encrypted_header = mutate_text(&tampered.encrypted_header);

    let mut recv_ctx = context(9_999, 1_700_304_999);
    let result = receive_message(&mut bob_session, &mut recv_ctx, &tampered);
    assert!(result.is_err());
    assert_eq!(snapshot(&bob_session.state), before);

    let follow_up = script.send_text(
        Side::Alice,
        &mut alice_session,
        &mut bob_session,
        "after-tamper",
    )?;
    let received = script.deliver(follow_up, &mut alice_session, &mut bob_session)?;
    assert_eq!(received.content, "after-tamper");
    Ok(())
}

#[test]
fn plan_receive_is_pure_across_current_next_and_previous_header_paths() -> Result<()> {
    let (_alice, _bob, mut alice_session, mut bob_session) =
        direct_session_pair(71, 72, 1_700_305_000)?;
    let mut script = DeliveryScript::new(10_000, 1_700_305_010);

    let initial = script.send_text(Side::Alice, &mut alice_session, &mut bob_session, "initial")?;
    let _ = script.deliver(initial, &mut alice_session, &mut bob_session)?;

    let current = script.send_text(Side::Alice, &mut alice_session, &mut bob_session, "current")?;
    assert_receive_plan_pure(
        &bob_session,
        &script.sent(current).incoming,
        10_100,
        1_700_305_100,
    )?;

    let bob_reply = script.send_text(Side::Bob, &mut alice_session, &mut bob_session, "reply")?;
    let _ = script.deliver(bob_reply, &mut alice_session, &mut bob_session)?;

    let delayed_previous = script.send_text(
        Side::Alice,
        &mut alice_session,
        &mut bob_session,
        "previous",
    )?;
    let ratchet_trigger =
        script.send_text(Side::Alice, &mut alice_session, &mut bob_session, "next")?;

    assert_receive_plan_pure(
        &bob_session,
        &script.sent(ratchet_trigger).incoming,
        10_200,
        1_700_305_200,
    )?;

    let _ = script.deliver(ratchet_trigger, &mut alice_session, &mut bob_session)?;

    assert_receive_plan_pure(
        &bob_session,
        &script.sent(delayed_previous).incoming,
        10_300,
        1_700_305_300,
    )?;
    Ok(())
}
