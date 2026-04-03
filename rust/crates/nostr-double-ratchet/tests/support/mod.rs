#![allow(dead_code)]

use nostr::{Event, EventBuilder, Keys, Kind, SecretKey, Tag, Timestamp};
use nostr_double_ratchet::{
    codec::nostr as codec, DeviceId, DevicePubkey, DirectMessageContent,
    IncomingDirectMessageEnvelope, IncomingInviteResponseEnvelope, Invite, InviteResponse,
    OwnerPubkey, ProtocolContext, Result, Rumor, Session, SessionState, UnixMillis, UnixSeconds,
};
use rand::{rngs::StdRng, CryptoRng, RngCore, SeedableRng};
use serde::Serialize;

pub const ROOT_URL: &str = "https://chat.iris.to";

pub struct Actor {
    pub secret_key: [u8; 32],
    pub keys: Keys,
    pub device_pubkey: DevicePubkey,
    pub owner_pubkey: OwnerPubkey,
    pub device_id: DeviceId,
}

pub struct InviteBootstrap {
    pub alice: Actor,
    pub bob: Actor,
    pub owned_invite: Invite,
    pub invite_response: InviteResponse,
    pub incoming_response: IncomingInviteResponseEnvelope,
    pub alice_session: Session,
    pub bob_session: Session,
}

pub struct SentMessage {
    pub rumor: Rumor,
    pub event: Event,
    pub incoming: IncomingDirectMessageEnvelope,
}

pub fn actor(secret_fill: u8, device_id: &str) -> Actor {
    let secret_key = [secret_fill; 32];
    let keys = Keys::new(SecretKey::from_slice(&secret_key).unwrap());
    let device_pubkey = DevicePubkey::from_bytes(keys.public_key().to_bytes());
    Actor {
        secret_key,
        keys,
        device_pubkey,
        owner_pubkey: device_pubkey.as_owner(),
        device_id: DeviceId::new(device_id),
    }
}

pub fn context(seed: u64, now_secs: u64) -> ProtocolContext<'static, StdRng> {
    let mixed_seed = seed
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .rotate_left(17)
        ^ now_secs.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let rng = Box::new(StdRng::seed_from_u64(mixed_seed));
    let rng = Box::leak(rng);
    ProtocolContext::new(
        UnixSeconds(now_secs),
        UnixMillis(now_secs.saturating_mul(1000).saturating_add(seed % 997)),
        rng,
    )
}

pub fn direct_session_pair(
    alice_fill: u8,
    bob_fill: u8,
    base_secs: u64,
) -> Result<(Actor, Actor, Session, Session)> {
    let alice = actor(alice_fill, "alice-device");
    let bob = actor(bob_fill, "bob-device");
    let shared_secret = [77u8; 32];

    let mut alice_init = context(10 + alice_fill as u64, base_secs);
    let alice_session = Session::init(
        &mut alice_init,
        bob.device_pubkey,
        alice.secret_key,
        true,
        shared_secret,
        Some("alice".to_string()),
    )?;

    let mut bob_init = context(20 + bob_fill as u64, base_secs);
    let bob_session = Session::init(
        &mut bob_init,
        alice.device_pubkey,
        bob.secret_key,
        false,
        shared_secret,
        Some("bob".to_string()),
    )?;

    Ok((alice, bob, alice_session, bob_session))
}

pub fn bootstrap_via_invite_url(base_secs: u64) -> Result<InviteBootstrap> {
    bootstrap_via_invite(base_secs, true)
}

pub fn bootstrap_via_invite_event(base_secs: u64) -> Result<InviteBootstrap> {
    bootstrap_via_invite(base_secs, false)
}

fn bootstrap_via_invite(base_secs: u64, via_url: bool) -> Result<InviteBootstrap> {
    let alice = actor(11, "alice-device");
    let bob = actor(12, "bob-device");

    let mut invite_ctx = context(100, base_secs);
    let owned_invite = Invite::create_new(
        &mut invite_ctx,
        alice.owner_pubkey,
        Some(alice.device_id.clone()),
        None,
    )?;

    let public_invite = if via_url {
        let url = codec::invite_url(&owned_invite, ROOT_URL)?;
        codec::parse_invite_url(&url)?
    } else {
        let signed_event = codec::invite_unsigned_event(&owned_invite)?.sign_with_keys(&alice.keys)?;
        codec::parse_invite_event(&signed_event)?
    };

    let mut bob_accept_ctx = context(101, base_secs + 1);
    let (bob_session, response_envelope) = public_invite.accept(
        &mut bob_accept_ctx,
        bob.device_pubkey,
        bob.secret_key,
        Some(bob.device_id.clone()),
    )?;

    let response_event = codec::invite_response_event(&response_envelope)?;
    let incoming_response = codec::parse_invite_response_event(&response_event)?;

    let mut alice_process_ctx = context(102, base_secs + 2);
    let invite_response = owned_invite
        .process_invite_response(&mut alice_process_ctx, &incoming_response, alice.secret_key)?
        .expect("expected invite response");
    let alice_session = invite_response.session.clone();

    Ok(InviteBootstrap {
        alice,
        bob,
        owned_invite,
        invite_response,
        incoming_response,
        alice_session,
        bob_session,
    })
}

pub fn send_message<R>(
    session: &mut Session,
    ctx: &mut ProtocolContext<'_, R>,
    content: DirectMessageContent,
) -> Result<SentMessage>
where
    R: RngCore + CryptoRng,
{
    let rumor = Rumor::from_content(ctx, content)?;
    send_rumor(session, ctx.now_secs, rumor)
}

pub fn send_rumor(session: &mut Session, now: UnixSeconds, rumor: Rumor) -> Result<SentMessage> {
    let send_plan = session.plan_send(&rumor, now)?;
    let sent = session.apply_send(send_plan);
    let event = codec::direct_message_event(&sent.envelope)?;
    let incoming = codec::parse_direct_message_event(&event)?;
    Ok(SentMessage {
        rumor: sent.rumor,
        event,
        incoming,
    })
}

pub fn receive_message<R>(
    session: &mut Session,
    ctx: &mut ProtocolContext<'_, R>,
    incoming: &IncomingDirectMessageEnvelope,
) -> Result<Rumor>
where
    R: RngCore + CryptoRng,
{
    let plan = session.plan_receive(ctx, incoming)?;
    Ok(session.apply_receive(plan).rumor)
}

pub fn receive_event<R>(
    session: &mut Session,
    ctx: &mut ProtocolContext<'_, R>,
    event: &Event,
) -> Result<Rumor>
where
    R: RngCore + CryptoRng,
{
    let incoming = codec::parse_direct_message_event(event)?;
    receive_message(session, ctx, &incoming)
}

pub fn restore_session(state: &SessionState, name: &str) -> Session {
    let restored: SessionState = serde_json::from_str(&serde_json::to_string(state).unwrap()).unwrap();
    Session::new(restored, name.to_string())
}

pub fn snapshot<T>(value: &T) -> String
where
    T: Serialize,
{
    serde_json::to_string(value).unwrap()
}

pub fn mutate_text(value: &str) -> String {
    let mut chars: Vec<char> = value.chars().collect();
    if chars.is_empty() {
        return "A".to_string();
    }
    let index = chars
        .iter()
        .rposition(|c| *c != '=')
        .unwrap_or(chars.len().saturating_sub(1));
    chars[index] = match chars[index] {
        'A' => 'B',
        'B' => 'C',
        _ => 'A',
    };
    chars.into_iter().collect()
}

pub fn signed_event(
    signer_secret: [u8; 32],
    kind: u32,
    content: &str,
    tags: Vec<Tag>,
    created_at: UnixSeconds,
) -> Event {
    let keys = Keys::new(SecretKey::from_slice(&signer_secret).unwrap());
    EventBuilder::new(Kind::from(kind as u16), content)
        .tags(tags)
        .custom_created_at(Timestamp::from(created_at.get()))
        .build(keys.public_key())
        .sign_with_keys(&keys)
        .unwrap()
}

pub fn header_tag(header: &str) -> Tag {
    Tag::parse(["header".to_string(), header.to_string()]).unwrap()
}

pub fn assert_rumor_eq(actual: &Rumor, expected: &Rumor) {
    assert_eq!(actual.id, expected.id);
    assert_eq!(actual.pubkey, expected.pubkey);
    assert_eq!(actual.created_at, expected.created_at);
    assert_eq!(actual.kind, expected.kind);
    assert_eq!(actual.tags, expected.tags);
    assert_eq!(actual.content, expected.content);
}
