#![allow(dead_code)]

use base64::Engine;
use nostr::nips::nip44::{self, Version};
use nostr::{Event, EventBuilder, Keys, Kind, PublicKey, SecretKey, Tag, Timestamp};
use nostr_double_ratchet::{
    codec::nostr as codec, DeviceDelivery, DeviceId, DevicePubkey, DeviceRecordSnapshot,
    DirectMessageContent, IncomingDirectMessageEnvelope, IncomingInviteResponseEnvelope, Invite,
    InviteResponse, OutgoingInviteResponseEnvelope, OwnerPubkey, PreparedFanout,
    ProcessedInviteResponse, ProtocolContext, ReceivedDirectMessage, Result, Rumor, Session,
    SessionManager, SessionManagerPolicy, SessionManagerSnapshot, SessionState, UnixMillis,
    UnixSeconds, UserRecordSnapshot,
};
use rand::{rngs::StdRng, CryptoRng, RngCore, SeedableRng};
use serde::{Deserialize, Serialize};

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

pub struct ManagerDevice {
    pub owner_pubkey: OwnerPubkey,
    pub secret_key: [u8; 32],
    pub keys: Keys,
    pub device_pubkey: DevicePubkey,
    pub device_id: DeviceId,
}

pub struct InviteResponseFixture {
    pub alice: Actor,
    pub bob: Actor,
    pub owned_invite: Invite,
    pub public_invite: Invite,
    pub response_envelope: OutgoingInviteResponseEnvelope,
    pub incoming_response: IncomingInviteResponseEnvelope,
    pub bob_session: Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Alice,
    Bob,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InviteResponseCorruption {
    OuterEnvelope,
    InnerBase64,
    InnerJson,
    PayloadJson,
    InvalidSessionKey,
}

#[derive(Clone)]
pub struct SentMessage {
    pub rumor: Rumor,
    pub event: Event,
    pub incoming: IncomingDirectMessageEnvelope,
}

pub struct HeldMessage {
    pub from: Side,
    pub sent: SentMessage,
}

pub struct DeliveryScript {
    next_seed: u64,
    next_secs: u64,
    held: Vec<HeldMessage>,
}

pub fn manager_device(owner_fill: u8, device_fill: u8, device_id: &str) -> ManagerDevice {
    let owner_secret = [owner_fill; 32];
    let owner_keys = Keys::new(SecretKey::from_slice(&owner_secret).unwrap());
    let secret_key = [device_fill; 32];
    let keys = Keys::new(SecretKey::from_slice(&secret_key).unwrap());
    let device_pubkey = DevicePubkey::from_bytes(keys.public_key().to_bytes());
    ManagerDevice {
        owner_pubkey: OwnerPubkey::from_bytes(owner_keys.public_key().to_bytes()),
        secret_key,
        keys,
        device_pubkey,
        device_id: DeviceId::new(device_id),
    }
}

pub fn session_manager(device: &ManagerDevice, max_relay_latency_secs: u64) -> SessionManager {
    SessionManager::new(
        device.owner_pubkey,
        device.secret_key,
        Some(device.device_id.clone()),
        SessionManagerPolicy {
            max_relay_latency: UnixSeconds(max_relay_latency_secs),
        },
    )
}

pub fn app_keys_for(devices: &[&ManagerDevice], created_at: u64) -> nostr_double_ratchet::AppKeys {
    nostr_double_ratchet::AppKeys::new(
        devices
            .iter()
            .map(|device| nostr_double_ratchet::DeviceEntry {
                identity_pubkey: device.device_pubkey,
                created_at: UnixSeconds(created_at),
            })
            .collect(),
    )
}

pub fn public_invite_via_url(invite: &Invite) -> Result<Invite> {
    codec::parse_invite_url(&codec::invite_url(invite, ROOT_URL)?)
}

pub fn public_invite_via_event(invite: &Invite, signer_secret: [u8; 32]) -> Result<Invite> {
    let keys = Keys::new(SecretKey::from_slice(&signer_secret).unwrap());
    let event = codec::invite_unsigned_event(invite)?.sign_with_keys(&keys)?;
    codec::parse_invite_event(&event)
}

pub fn manager_public_device_invite(
    manager: &mut SessionManager,
    device: &ManagerDevice,
    seed: u64,
    now_secs: u64,
) -> Result<Invite> {
    let mut ctx = context(seed, now_secs);
    let invite = manager.ensure_local_device_invite(&mut ctx)?.clone();
    public_invite_via_event(&invite, device.secret_key)
}

pub fn custom_public_device_invite(
    device: &ManagerDevice,
    seed: u64,
    now_secs: u64,
    device_id: Option<DeviceId>,
) -> Result<Invite> {
    let mut ctx = context(seed, now_secs);
    let mut invite = Invite::create_new(
        &mut ctx,
        device.device_pubkey.as_owner(),
        device_id,
        None,
    )?;
    invite.owner_public_key = Some(device.owner_pubkey);
    if invite.device_id.is_some() {
        public_invite_via_event(&invite, device.secret_key)
    } else {
        public_invite_via_url(&invite)
    }
}

pub fn incoming_invite_response(
    envelope: &OutgoingInviteResponseEnvelope,
) -> Result<IncomingInviteResponseEnvelope> {
    let event = codec::invite_response_event(envelope)?;
    codec::parse_invite_response_event(&event)
}

pub fn manager_receive_delivery<R>(
    manager: &mut SessionManager,
    ctx: &mut ProtocolContext<'_, R>,
    sender_owner: OwnerPubkey,
    envelope: &nostr_double_ratchet::DeviceDelivery,
) -> Result<Option<ReceivedDirectMessage>>
where
    R: RngCore + CryptoRng,
{
    let event = codec::direct_message_event(&envelope.envelope)?;
    let incoming = codec::parse_direct_message_event(&event)?;
    manager.receive_direct_message(ctx, sender_owner, &incoming)
}

pub fn manager_observe_invite_response<R>(
    manager: &mut SessionManager,
    ctx: &mut ProtocolContext<'_, R>,
    envelope: &OutgoingInviteResponseEnvelope,
) -> Result<Option<ProcessedInviteResponse>>
where
    R: RngCore + CryptoRng,
{
    let incoming = incoming_invite_response(envelope)?;
    manager.observe_invite_response(ctx, &incoming)
}

pub fn restore_manager(
    snapshot: &SessionManagerSnapshot,
    local_device_secret_key: [u8; 32],
    max_relay_latency_secs: u64,
) -> Result<SessionManager> {
    let restored: SessionManagerSnapshot =
        serde_json::from_str(&serde_json::to_string(snapshot).unwrap()).unwrap();
    SessionManager::from_snapshot(
        restored,
        local_device_secret_key,
        SessionManagerPolicy {
            max_relay_latency: UnixSeconds(max_relay_latency_secs),
        },
    )
}

pub fn observe_device_invites(
    manager: &mut SessionManager,
    owner_pubkey: OwnerPubkey,
    invites: &[Invite],
) -> Result<()> {
    for invite in invites {
        manager.observe_device_invite(owner_pubkey, invite.clone())?;
    }
    Ok(())
}

pub fn prepared_targets(prepared: &PreparedFanout) -> Vec<(OwnerPubkey, DevicePubkey)> {
    prepared
        .deliveries
        .iter()
        .map(|delivery| (delivery.owner_pubkey, delivery.device_pubkey))
        .collect()
}

pub fn delivery_by_target(
    prepared: &PreparedFanout,
    owner_pubkey: OwnerPubkey,
    device_pubkey: DevicePubkey,
) -> DeviceDelivery {
    prepared
        .deliveries
        .iter()
        .find(|delivery| {
            delivery.owner_pubkey == owner_pubkey && delivery.device_pubkey == device_pubkey
        })
        .cloned()
        .expect("target delivery must exist")
}

pub fn manager_user_snapshot(
    snapshot: &SessionManagerSnapshot,
    owner_pubkey: OwnerPubkey,
) -> &UserRecordSnapshot {
    snapshot
        .users
        .iter()
        .find(|user| user.owner_pubkey == owner_pubkey)
        .expect("owner snapshot must exist")
}

pub fn manager_device_snapshot(
    user: &UserRecordSnapshot,
    device_pubkey: DevicePubkey,
) -> &DeviceRecordSnapshot {
    user.devices
        .iter()
        .find(|device| device.device_pubkey == device_pubkey)
        .expect("device snapshot must exist")
}

pub fn received_contents(received: &[ReceivedDirectMessage]) -> Vec<String> {
    received
        .iter()
        .map(|message| message.rumor.content.clone())
        .collect()
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
    let mixed_seed = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(17)
        ^ now_secs.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let rng = Box::new(StdRng::seed_from_u64(mixed_seed));
    let rng = Box::leak(rng);
    ProtocolContext::new(
        UnixSeconds(now_secs),
        UnixMillis(now_secs.saturating_mul(1000).saturating_add(seed % 997)),
        rng,
    )
}

impl Side {
    pub fn opposite(self) -> Self {
        match self {
            Self::Alice => Self::Bob,
            Self::Bob => Self::Alice,
        }
    }
}

impl DeliveryScript {
    pub fn new(start_seed: u64, start_secs: u64) -> Self {
        Self {
            next_seed: start_seed,
            next_secs: start_secs,
            held: Vec::new(),
        }
    }

    fn next_context(&mut self) -> ProtocolContext<'static, StdRng> {
        let ctx = context(self.next_seed, self.next_secs);
        self.next_seed = self.next_seed.saturating_add(1);
        self.next_secs = self.next_secs.saturating_add(1);
        ctx
    }

    pub fn send_text(
        &mut self,
        from: Side,
        alice: &mut Session,
        bob: &mut Session,
        text: impl Into<String>,
    ) -> Result<usize> {
        let mut ctx = self.next_context();
        let sent = match from {
            Side::Alice => send_message(alice, &mut ctx, DirectMessageContent::Text(text.into()))?,
            Side::Bob => send_message(bob, &mut ctx, DirectMessageContent::Text(text.into()))?,
        };
        let id = self.held.len();
        self.held.push(HeldMessage { from, sent });
        Ok(id)
    }

    pub fn deliver(&mut self, id: usize, alice: &mut Session, bob: &mut Session) -> Result<Rumor> {
        let mut ctx = self.next_context();
        let held = self
            .held
            .get(id)
            .expect("delivery script message id must exist");
        match held.from {
            Side::Alice => receive_event(bob, &mut ctx, &held.sent.event),
            Side::Bob => receive_event(alice, &mut ctx, &held.sent.event),
        }
    }

    pub fn replay(&mut self, id: usize, alice: &mut Session, bob: &mut Session) -> Result<Rumor> {
        self.deliver(id, alice, bob)
    }

    pub fn sent(&self, id: usize) -> &SentMessage {
        &self.held[id].sent
    }
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

pub fn invite_response_fixture(
    base_secs: u64,
    max_uses: Option<usize>,
) -> Result<InviteResponseFixture> {
    let alice = actor(51, "alice-device");
    let bob = actor(52, "bob-device");

    let mut invite_ctx = context(300, base_secs);
    let owned_invite = Invite::create_new(
        &mut invite_ctx,
        alice.owner_pubkey,
        Some(alice.device_id.clone()),
        max_uses,
    )?;
    let public_invite = codec::parse_invite_url(&codec::invite_url(&owned_invite, ROOT_URL)?)?;

    let mut accept_ctx = context(301, base_secs + 1);
    let (bob_session, response_envelope) = public_invite.accept(
        &mut accept_ctx,
        bob.device_pubkey,
        bob.secret_key,
        Some(bob.device_id.clone()),
    )?;
    let response_event = codec::invite_response_event(&response_envelope)?;
    let incoming_response = codec::parse_invite_response_event(&response_event)?;

    Ok(InviteResponseFixture {
        alice,
        bob,
        owned_invite,
        public_invite,
        response_envelope,
        incoming_response,
        bob_session,
    })
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
    let mut owned_invite = Invite::create_new(
        &mut invite_ctx,
        alice.owner_pubkey,
        Some(alice.device_id.clone()),
        None,
    )?;

    let public_invite = if via_url {
        let url = codec::invite_url(&owned_invite, ROOT_URL)?;
        codec::parse_invite_url(&url)?
    } else {
        let signed_event =
            codec::invite_unsigned_event(&owned_invite)?.sign_with_keys(&alice.keys)?;
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
    let restored: SessionState =
        serde_json::from_str(&serde_json::to_string(state).unwrap()).unwrap();
    Session::new(restored, name.to_string())
}

pub fn checkpoint_session(session: &Session) -> SessionState {
    session.state.clone()
}

pub fn corrupt_invite_response_layer(
    invite: &Invite,
    response: &OutgoingInviteResponseEnvelope,
    invitee: &Actor,
    corruption: InviteResponseCorruption,
) -> Result<IncomingInviteResponseEnvelope> {
    match corruption {
        InviteResponseCorruption::OuterEnvelope => Ok(IncomingInviteResponseEnvelope {
            sender: response.sender,
            created_at: response.created_at,
            content: mutate_text(&response.content),
        }),
        InviteResponseCorruption::InnerJson => {
            reencrypt_outer_response(response, "\"not-json\"".to_string())
        }
        InviteResponseCorruption::InnerBase64 => {
            let mut inner = decrypt_outer_response(invite, response)?;
            inner.content = "***not-base64***".to_string();
            reencrypt_outer_response(response, serde_json::to_string(&inner)?)
        }
        InviteResponseCorruption::PayloadJson => {
            let mut inner = decrypt_outer_response(invite, response)?;
            inner.content = build_invite_payload_ciphertext(invite, invitee, "{")?;
            reencrypt_outer_response(response, serde_json::to_string(&inner)?)
        }
        InviteResponseCorruption::InvalidSessionKey => {
            let mut inner = decrypt_outer_response(invite, response)?;
            inner.content = build_invite_payload_ciphertext(
                invite,
                invitee,
                r#"{"sessionKey":"deadbeef","deviceId":"broken-device"}"#,
            )?;
            reencrypt_outer_response(response, serde_json::to_string(&inner)?)
        }
    }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TestInviteResponseInnerEvent {
    pubkey: DevicePubkey,
    content: String,
    created_at: UnixSeconds,
}

fn decrypt_outer_response(
    invite: &Invite,
    response: &OutgoingInviteResponseEnvelope,
) -> Result<TestInviteResponseInnerEvent> {
    let inviter_ephemeral_private_key = invite
        .inviter_ephemeral_private_key
        .expect("owned invite must have ephemeral private key");
    let decrypted = nip44::decrypt(
        &SecretKey::from_slice(&inviter_ephemeral_private_key).unwrap(),
        &nostr_pubkey(response.sender),
        &response.content,
    )?;
    Ok(serde_json::from_str(&decrypted)?)
}

fn reencrypt_outer_response(
    response: &OutgoingInviteResponseEnvelope,
    plaintext: String,
) -> Result<IncomingInviteResponseEnvelope> {
    let content = nip44::encrypt(
        &SecretKey::from_slice(&response.signer_secret_key).unwrap(),
        &nostr_pubkey(response.recipient),
        plaintext,
        Version::V2,
    )?;
    Ok(IncomingInviteResponseEnvelope {
        sender: response.sender,
        created_at: response.created_at,
        content,
    })
}

fn build_invite_payload_ciphertext(
    invite: &Invite,
    invitee: &Actor,
    payload_json: &str,
) -> Result<String> {
    let dh_encrypted = nip44::encrypt(
        &SecretKey::from_slice(&invitee.secret_key).unwrap(),
        &nostr_pubkey(invite.inviter.as_device()),
        payload_json,
        Version::V2,
    )?;
    let conversation_key = nip44::v2::ConversationKey::new(invite.shared_secret);
    let encrypted_bytes = nip44::v2::encrypt_to_bytes(&conversation_key, &dh_encrypted)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(encrypted_bytes))
}

fn nostr_pubkey(pubkey: DevicePubkey) -> PublicKey {
    PublicKey::from_slice(&pubkey.to_bytes()).unwrap()
}
