use crate::{session, Error, Invite, Result, INVITE_EVENT_KIND, INVITE_RESPONSE_KIND};
use base64::Engine;
use nostr::nips::nip44::{self, Version};
use nostr::types::filter::{Alphabet, SingleLetterTag};
use nostr::{EventBuilder, Filter, Keys, Kind, PublicKey, Tag, Timestamp, UnsignedEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InviteCreateRuntime {
    pub inviter_ephemeral_private_key: [u8; 32],
    pub shared_secret: [u8; 32],
    pub created_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InviteAcceptRuntime {
    pub invitee_session_private_key: [u8; 32],
    pub random_sender_private_key: [u8; 32],
    pub inner_created_at: u64,
    pub envelope_created_at: u64,
    pub session_init: session::domain::SessionInitRuntime,
}

pub struct InviteAcceptResult {
    pub next_invite: Invite,
    pub session_state: crate::SessionState,
    pub response_event: nostr::Event,
}

pub struct InviteResponseState {
    pub session_state: crate::SessionState,
    pub invitee_identity: PublicKey,
    pub device_id: Option<String>,
    pub owner_public_key: Option<PublicKey>,
}

pub struct InviteResponseProcessing {
    pub next_invite: Invite,
    pub response: InviteResponseState,
}

pub fn create_invite(
    inviter: PublicKey,
    device_id: Option<String>,
    max_uses: Option<usize>,
    runtime: InviteCreateRuntime,
) -> Result<Invite> {
    let inviter_ephemeral_public_key = Keys::new(nostr::SecretKey::from_slice(
        &runtime.inviter_ephemeral_private_key,
    )?)
    .public_key();

    Ok(Invite {
        inviter_ephemeral_public_key,
        shared_secret: runtime.shared_secret,
        inviter,
        inviter_ephemeral_private_key: Some(runtime.inviter_ephemeral_private_key),
        device_id,
        max_uses,
        used_by: Vec::new(),
        created_at: runtime.created_at,
        purpose: None,
        owner_public_key: None,
    })
}

pub fn invite_url(invite: &Invite, root: &str) -> Result<String> {
    let mut data = serde_json::Map::new();
    data.insert(
        "inviter".to_string(),
        serde_json::Value::String(hex::encode(invite.inviter.to_bytes())),
    );
    data.insert(
        "ephemeralKey".to_string(),
        serde_json::Value::String(hex::encode(invite.inviter_ephemeral_public_key.to_bytes())),
    );
    data.insert(
        "sharedSecret".to_string(),
        serde_json::Value::String(hex::encode(invite.shared_secret)),
    );
    if let Some(purpose) = &invite.purpose {
        data.insert(
            "purpose".to_string(),
            serde_json::Value::String(purpose.clone()),
        );
    }
    if let Some(owner_pk) = &invite.owner_public_key {
        data.insert(
            "owner".to_string(),
            serde_json::Value::String(hex::encode(owner_pk.to_bytes())),
        );
    }

    Ok(format!(
        "{}#{}",
        root,
        urlencoding::encode(&serde_json::Value::Object(data).to_string())
    ))
}

pub fn invite_from_url(url: &str) -> Result<Invite> {
    let hash = url
        .split('#')
        .nth(1)
        .ok_or(Error::Invite("No hash in URL".to_string()))?;
    let decoded = urlencoding::decode(hash).map_err(|e| Error::Invite(e.to_string()))?;
    let data: serde_json::Value = serde_json::from_str(&decoded)?;

    let inviter = crate::utils::pubkey_from_hex(
        data["inviter"]
            .as_str()
            .ok_or(Error::Invite("Missing inviter".to_string()))?,
    )?;
    let ephemeral_key_str = data["ephemeralKey"]
        .as_str()
        .or_else(|| data["inviterEphemeralPublicKey"].as_str())
        .ok_or(Error::Invite("Missing ephemeralKey".to_string()))?;
    let inviter_ephemeral_public_key = crate::utils::pubkey_from_hex(ephemeral_key_str)?;
    let shared_secret_hex = data["sharedSecret"]
        .as_str()
        .ok_or(Error::Invite("Missing sharedSecret".to_string()))?;
    let shared_secret_bytes = hex::decode(shared_secret_hex)?;
    let mut shared_secret = [0u8; 32];
    shared_secret.copy_from_slice(&shared_secret_bytes);

    let purpose = data["purpose"].as_str().map(|s| s.to_string());
    let owner_public_key = data["owner"]
        .as_str()
        .or_else(|| data["ownerPubkey"].as_str())
        .and_then(|s| crate::utils::pubkey_from_hex(s).ok());

    Ok(Invite {
        inviter_ephemeral_public_key,
        shared_secret,
        inviter,
        inviter_ephemeral_private_key: None,
        device_id: None,
        max_uses: None,
        used_by: Vec::new(),
        created_at: 0,
        purpose,
        owner_public_key,
    })
}

pub fn invite_event(invite: &Invite) -> Result<UnsignedEvent> {
    let device_id = invite.device_id.as_ref().ok_or(Error::DeviceIdRequired)?;

    let tags = vec![
        Tag::parse(&[
            "ephemeralKey".to_string(),
            hex::encode(invite.inviter_ephemeral_public_key.to_bytes()),
        ])
        .map_err(|e| Error::InvalidEvent(e.to_string()))?,
        Tag::parse(&[
            "sharedSecret".to_string(),
            hex::encode(invite.shared_secret),
        ])
        .map_err(|e| Error::InvalidEvent(e.to_string()))?,
        Tag::parse(&[
            "d".to_string(),
            format!("double-ratchet/invites/{}", device_id),
        ])
        .map_err(|e| Error::InvalidEvent(e.to_string()))?,
        Tag::parse(&["l".to_string(), "double-ratchet/invites".to_string()])
            .map_err(|e| Error::InvalidEvent(e.to_string()))?,
    ];

    Ok(EventBuilder::new(Kind::from(INVITE_EVENT_KIND as u16), "")
        .tags(tags)
        .custom_created_at(Timestamp::from(invite.created_at))
        .build(invite.inviter))
}

pub fn invite_from_event(event: &nostr::Event) -> Result<Invite> {
    let inviter = event.pubkey;

    let ephemeral_key = event
        .tags
        .iter()
        .find(|t| t.as_slice().first().map(|s| s.as_str()) == Some("ephemeralKey"))
        .and_then(|t| t.as_slice().get(1).map(|s| s.to_string()))
        .ok_or(Error::Invite("Missing ephemeralKey tag".to_string()))?;

    let shared_secret_hex = event
        .tags
        .iter()
        .find(|t| t.as_slice().first().map(|s| s.as_str()) == Some("sharedSecret"))
        .and_then(|t| t.as_slice().get(1).map(|s| s.to_string()))
        .ok_or(Error::Invite("Missing sharedSecret tag".to_string()))?;

    let device_tag = event
        .tags
        .iter()
        .find(|t| t.as_slice().first().map(|s| s.as_str()) == Some("d"))
        .and_then(|t| t.as_slice().get(1).map(|s| s.to_string()));

    let device_id = device_tag
        .and_then(|d| d.split('/').nth(2).map(String::from))
        .filter(|device_id| device_id != "public");

    let inviter_ephemeral_public_key = crate::utils::pubkey_from_hex(&ephemeral_key)?;
    let shared_secret_bytes = hex::decode(&shared_secret_hex)?;
    let mut shared_secret = [0u8; 32];
    shared_secret.copy_from_slice(&shared_secret_bytes);

    Ok(Invite {
        inviter_ephemeral_public_key,
        shared_secret,
        inviter,
        inviter_ephemeral_private_key: None,
        device_id,
        max_uses: None,
        used_by: Vec::new(),
        created_at: event.created_at.as_u64(),
        purpose: None,
        owner_public_key: None,
    })
}

pub fn accept_invite(
    invite: &Invite,
    invitee_public_key: PublicKey,
    invitee_private_key: [u8; 32],
    device_id: Option<String>,
    owner_public_key: Option<PublicKey>,
    runtime: InviteAcceptRuntime,
) -> Result<InviteAcceptResult> {
    let invitee_session_keys = Keys::new(nostr::SecretKey::from_slice(
        &runtime.invitee_session_private_key,
    )?);
    let invitee_session_public_key = invitee_session_keys.public_key();

    let session_state = session::domain::init_state(
        invite.inviter_ephemeral_public_key,
        runtime.invitee_session_private_key,
        true,
        invite.shared_secret,
        runtime.session_init,
    )?;

    let mut payload = serde_json::Map::new();
    payload.insert(
        "sessionKey".to_string(),
        serde_json::Value::String(hex::encode(invitee_session_public_key.to_bytes())),
    );
    if let Some(device_id) = device_id {
        payload.insert("deviceId".to_string(), serde_json::Value::String(device_id));
    }
    if let Some(owner_pk) = owner_public_key {
        payload.insert(
            "ownerPublicKey".to_string(),
            serde_json::Value::String(hex::encode(owner_pk.to_bytes())),
        );
    }
    let payload = serde_json::Value::Object(payload);

    let invitee_sk = nostr::SecretKey::from_slice(&invitee_private_key)?;
    let dh_encrypted = nip44::encrypt(
        &invitee_sk,
        &invite.inviter,
        payload.to_string(),
        Version::V2,
    )?;

    let conversation_key = nip44::v2::ConversationKey::new(invite.shared_secret);
    let encrypted_bytes = nip44::v2::encrypt_to_bytes(&conversation_key, &dh_encrypted)?;
    let inner_encrypted = base64::engine::general_purpose::STANDARD.encode(encrypted_bytes);

    let inner_event = serde_json::json!({
        "pubkey": hex::encode(invitee_public_key.to_bytes()),
        "content": inner_encrypted,
        "created_at": runtime.inner_created_at,
    });

    let random_sender_keys = Keys::new(nostr::SecretKey::from_slice(
        &runtime.random_sender_private_key,
    )?);
    let envelope_content = nip44::encrypt(
        random_sender_keys.secret_key(),
        &invite.inviter_ephemeral_public_key,
        inner_event.to_string(),
        Version::V2,
    )?;

    let unsigned_envelope =
        EventBuilder::new(Kind::from(INVITE_RESPONSE_KIND as u16), envelope_content)
            .tag(
                Tag::parse(&[
                    "p".to_string(),
                    hex::encode(invite.inviter_ephemeral_public_key.to_bytes()),
                ])
                .map_err(|e| Error::InvalidEvent(e.to_string()))?,
            )
            .custom_created_at(Timestamp::from(runtime.envelope_created_at))
            .build(random_sender_keys.public_key());

    let response_event = unsigned_envelope
        .sign_with_keys(&random_sender_keys)
        .map_err(|e| Error::InvalidEvent(e.to_string()))?;

    Ok(InviteAcceptResult {
        next_invite: invite.clone(),
        session_state,
        response_event,
    })
}

pub fn process_invite_response(
    invite: &Invite,
    event: &nostr::Event,
    inviter_private_key: [u8; 32],
) -> Result<Option<InviteResponseProcessing>> {
    let inviter_ephemeral_private_key = invite
        .inviter_ephemeral_private_key
        .ok_or(Error::Invite("Ephemeral key not available".to_string()))?;

    let inviter_ephemeral_sk = nostr::SecretKey::from_slice(&inviter_ephemeral_private_key)?;
    let sender_pk = event.pubkey;
    let decrypted = nip44::decrypt(&inviter_ephemeral_sk, &sender_pk, &event.content)?;
    let inner_event: serde_json::Value = serde_json::from_str(&decrypted)?;

    let invitee_identity_hex = inner_event["pubkey"]
        .as_str()
        .ok_or(Error::Invite("Missing pubkey".to_string()))?;
    let invitee_identity = crate::utils::pubkey_from_hex(invitee_identity_hex)?;

    let inner_content = inner_event["content"]
        .as_str()
        .ok_or(Error::Invite("Missing content".to_string()))?;

    let conversation_key = nip44::v2::ConversationKey::new(invite.shared_secret);
    let ciphertext_bytes = base64::engine::general_purpose::STANDARD
        .decode(inner_content)
        .map_err(|e| Error::Serialization(e.to_string()))?;
    let dh_encrypted_ciphertext = String::from_utf8(nip44::v2::decrypt_to_bytes(
        &conversation_key,
        &ciphertext_bytes,
    )?)
    .map_err(|e| Error::Serialization(e.to_string()))?;

    let inviter_sk = nostr::SecretKey::from_slice(&inviter_private_key)?;
    let dh_decrypted = nip44::decrypt(&inviter_sk, &invitee_identity, &dh_encrypted_ciphertext)?;

    let (session_state, device_id, owner_public_key) =
        match serde_json::from_str::<serde_json::Value>(&dh_decrypted) {
            Ok(payload) => {
                let invitee_session_key_hex = payload["sessionKey"]
                    .as_str()
                    .ok_or(Error::Invite("Missing sessionKey".to_string()))?;
                let invitee_session_pubkey =
                    crate::utils::pubkey_from_hex(invitee_session_key_hex)?;
                let session_state = session::domain::init_state(
                    invitee_session_pubkey,
                    inviter_ephemeral_private_key,
                    false,
                    invite.shared_secret,
                    session::domain::SessionInitRuntime::default(),
                )?;
                let device_id = payload["deviceId"].as_str().map(String::from);
                let owner_public_key = payload["ownerPublicKey"]
                    .as_str()
                    .and_then(|s| crate::utils::pubkey_from_hex(s).ok());
                (session_state, device_id, owner_public_key)
            }
            Err(_) => {
                let invitee_session_pubkey = crate::utils::pubkey_from_hex(&dh_decrypted)?;
                let session_state = session::domain::init_state(
                    invitee_session_pubkey,
                    inviter_ephemeral_private_key,
                    false,
                    invite.shared_secret,
                    session::domain::SessionInitRuntime::default(),
                )?;
                (session_state, None, None)
            }
        };

    let mut next_invite = invite.clone();
    if !next_invite.used_by.contains(&invitee_identity) {
        next_invite.used_by.push(invitee_identity);
    }

    Ok(Some(InviteResponseProcessing {
        next_invite,
        response: InviteResponseState {
            session_state,
            invitee_identity,
            device_id,
            owner_public_key,
        },
    }))
}

pub fn listen_filter(invite: &Invite) -> Filter {
    Filter::new()
        .kind(Kind::from(INVITE_RESPONSE_KIND as u16))
        .pubkeys(vec![invite.inviter_ephemeral_public_key])
}

pub fn from_user_filter(user_pubkey: PublicKey) -> Filter {
    Filter::new()
        .kind(Kind::from(INVITE_EVENT_KIND as u16))
        .authors(vec![user_pubkey])
        .custom_tag(
            SingleLetterTag::lowercase(Alphabet::L),
            ["double-ratchet/invites"],
        )
}
