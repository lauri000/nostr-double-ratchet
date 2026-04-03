use crate::{
    secret_key_from_bytes, AppKeys, CodecError, DeviceEntry, DeviceId, DevicePubkey, DomainError,
    IncomingDirectMessageEnvelope, IncomingInviteResponseEnvelope, Invite,
    OutgoingDirectMessageEnvelope, OutgoingInviteResponseEnvelope, OwnerPubkey, Result,
    UnixSeconds, APP_KEYS_EVENT_KIND, INVITE_EVENT_KIND, INVITE_RESPONSE_KIND, MESSAGE_EVENT_KIND,
};
use base64::Engine;
use nostr::nips::nip44;
use nostr::{Event, EventBuilder, Keys, Kind, Tag, Timestamp, UnsignedEvent};
use serde::{Deserialize, Serialize};

const APP_KEYS_D_TAG: &str = "double-ratchet/app-keys";
const APP_KEYS_LABELS_TYPE: &str = "app-keys-labels";
const APP_KEYS_VERSION: &str = "1";
const INVITE_LIST_LABEL: &str = "double-ratchet/invites";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedAppKeysEvent {
    pub owner_pubkey: OwnerPubkey,
    pub created_at: UnixSeconds,
    pub app_keys: AppKeys,
}

pub fn direct_message_event(envelope: &OutgoingDirectMessageEnvelope) -> Result<Event> {
    let author_secret_key = secret_key_from_bytes(&envelope.signer_secret_key)?;
    let author_keys = Keys::new(author_secret_key);
    let derived_sender = DevicePubkey::from_nostr(author_keys.public_key());
    if derived_sender != envelope.sender {
        return Err(
            CodecError::InvalidEvent("sender does not match signer secret".to_string()).into(),
        );
    }

    let unsigned = EventBuilder::new(
        Kind::from(MESSAGE_EVENT_KIND as u16),
        envelope.ciphertext.clone(),
    )
    .tag(tag(["header", envelope.encrypted_header.as_str()])?)
    .custom_created_at(Timestamp::from(envelope.created_at.get()))
    .build(envelope.sender.to_nostr()?);

    Ok(unsigned.sign_with_keys(&author_keys)?)
}

pub fn parse_direct_message_event(event: &Event) -> Result<IncomingDirectMessageEnvelope> {
    verify_event_kind(event, MESSAGE_EVENT_KIND)?;
    event.verify()?;
    let encrypted_header = required_tag_value(event, "header")?;
    if encrypted_header.is_empty() {
        return Err(CodecError::InvalidEvent("empty `header` tag".to_string()).into());
    }

    Ok(IncomingDirectMessageEnvelope {
        sender: DevicePubkey::from_nostr(event.pubkey),
        created_at: UnixSeconds(event.created_at.as_u64()),
        encrypted_header,
        ciphertext: event.content.clone(),
    })
}

pub fn invite_url(invite: &Invite, root: &str) -> Result<String> {
    let mut data = serde_json::Map::new();
    data.insert(
        "inviter".to_string(),
        serde_json::Value::String(invite.inviter.to_string()),
    );
    data.insert(
        "ephemeralKey".to_string(),
        serde_json::Value::String(invite.inviter_ephemeral_public_key.to_string()),
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
    if let Some(owner) = invite.owner_public_key {
        data.insert(
            "owner".to_string(),
            serde_json::Value::String(owner.to_string()),
        );
    }

    Ok(format!(
        "{root}#{}",
        urlencoding::encode(&serde_json::Value::Object(data).to_string())
    ))
}

pub fn parse_invite_url(url: &str) -> Result<Invite> {
    let hash = url
        .split('#')
        .nth(1)
        .ok_or_else(|| CodecError::Invite("no hash in invite URL".to_string()))?;
    let decoded = urlencoding::decode(hash).map_err(|e| CodecError::Invite(e.to_string()))?;
    let data: serde_json::Value = serde_json::from_str(&decoded)?;

    let inviter = parse_owner_pubkey(
        data["inviter"]
            .as_str()
            .ok_or_else(|| CodecError::Invite("missing inviter".to_string()))?,
    )?;
    let inviter_ephemeral_public_key = parse_device_pubkey(
        data["ephemeralKey"]
            .as_str()
            .or_else(|| data["inviterEphemeralPublicKey"].as_str())
            .ok_or_else(|| CodecError::Invite("missing ephemeralKey".to_string()))?,
    )?;
    let shared_secret = parse_hex_32(
        data["sharedSecret"]
            .as_str()
            .ok_or_else(|| CodecError::Invite("missing sharedSecret".to_string()))?,
    )?;

    Ok(Invite {
        inviter_ephemeral_public_key,
        shared_secret,
        inviter,
        inviter_ephemeral_private_key: None,
        device_id: None,
        max_uses: None,
        used_by: Vec::new(),
        created_at: UnixSeconds(0),
        purpose: data["purpose"].as_str().map(str::to_owned),
        owner_public_key: data["owner"]
            .as_str()
            .or_else(|| data["ownerPubkey"].as_str())
            .map(parse_owner_pubkey)
            .transpose()?,
    })
}

pub fn invite_unsigned_event(invite: &Invite) -> Result<UnsignedEvent> {
    let device_id = invite
        .device_id
        .as_ref()
        .ok_or(DomainError::DeviceIdRequired)?;

    let mut builder = EventBuilder::new(Kind::from(INVITE_EVENT_KIND as u16), "")
        .tag(tag([
            "ephemeralKey",
            &invite.inviter_ephemeral_public_key.to_string(),
        ])?)
        .tag(tag(["sharedSecret", &hex::encode(invite.shared_secret)])?)
        .tag(tag([
            "d",
            &format!("double-ratchet/invites/{}", device_id.as_str()),
        ])?)
        .tag(tag(["l", INVITE_LIST_LABEL])?)
        .custom_created_at(Timestamp::from(invite.created_at.get()));

    if let Some(purpose) = &invite.purpose {
        builder = builder.tag(tag(["purpose", purpose])?);
    }
    if let Some(owner_public_key) = invite.owner_public_key {
        builder = builder.tag(tag(["ownerPublicKey", &owner_public_key.to_string()])?);
    }

    Ok(builder.build(invite.inviter.to_nostr()?))
}

pub fn parse_invite_event(event: &Event) -> Result<Invite> {
    verify_event_kind(event, INVITE_EVENT_KIND)?;
    event.verify()?;

    let device_id = required_tag_value(event, "d")?
        .strip_prefix("double-ratchet/invites/")
        .map(DeviceId::new);

    Ok(Invite {
        inviter_ephemeral_public_key: parse_device_pubkey(&required_tag_value(
            event,
            "ephemeralKey",
        )?)?,
        shared_secret: parse_hex_32(&required_tag_value(event, "sharedSecret")?)?,
        inviter: OwnerPubkey::from_nostr(event.pubkey),
        inviter_ephemeral_private_key: None,
        device_id,
        max_uses: None,
        used_by: Vec::new(),
        created_at: UnixSeconds(event.created_at.as_u64()),
        purpose: optional_tag_value(event, "purpose"),
        owner_public_key: optional_tag_value(event, "ownerPublicKey")
            .map(|value| parse_owner_pubkey(&value))
            .transpose()?,
    })
}

pub fn invite_response_event(envelope: &OutgoingInviteResponseEnvelope) -> Result<Event> {
    let author_secret_key = secret_key_from_bytes(&envelope.signer_secret_key)?;
    let author_keys = Keys::new(author_secret_key);
    let derived_sender = DevicePubkey::from_nostr(author_keys.public_key());
    if derived_sender != envelope.sender {
        return Err(
            CodecError::InvalidEvent("sender does not match signer secret".to_string()).into(),
        );
    }

    let unsigned = EventBuilder::new(
        Kind::from(INVITE_RESPONSE_KIND as u16),
        envelope.content.clone(),
    )
    .tag(tag(["p", &envelope.recipient.to_string()])?)
    .custom_created_at(Timestamp::from(envelope.created_at.get()))
    .build(envelope.sender.to_nostr()?);

    Ok(unsigned.sign_with_keys(&author_keys)?)
}

pub fn parse_invite_response_event(event: &Event) -> Result<IncomingInviteResponseEnvelope> {
    verify_event_kind(event, INVITE_RESPONSE_KIND)?;
    event.verify()?;
    let _recipient = parse_device_pubkey(&required_tag_value(event, "p")?)?;

    Ok(IncomingInviteResponseEnvelope {
        sender: DevicePubkey::from_nostr(event.pubkey),
        created_at: UnixSeconds(event.created_at.as_u64()),
        content: event.content.clone(),
    })
}

pub fn is_app_keys_event(event: &Event) -> bool {
    event.kind == Kind::from(APP_KEYS_EVENT_KIND as u16)
        && optional_tag_value(event, "d").as_deref() == Some(APP_KEYS_D_TAG)
}

pub fn app_keys_unsigned_event(
    owner_pubkey: OwnerPubkey,
    created_at: UnixSeconds,
    app_keys: &AppKeys,
) -> Result<UnsignedEvent> {
    build_app_keys_unsigned_event(
        owner_pubkey.to_nostr()?,
        created_at,
        app_keys,
        String::new(),
    )
}

pub fn app_keys_encrypted_unsigned_event(
    owner_keys: &Keys,
    created_at: UnixSeconds,
    app_keys: &AppKeys,
) -> Result<UnsignedEvent> {
    if app_keys.get_all_device_labels().is_empty() {
        return build_app_keys_unsigned_event(
            owner_keys.public_key(),
            created_at,
            app_keys,
            String::new(),
        );
    }

    let conversation_key =
        nip44::v2::ConversationKey::derive(owner_keys.secret_key(), &owner_keys.public_key());
    let payload = EncryptedAppKeysContent {
        payload_type: APP_KEYS_LABELS_TYPE.to_string(),
        v: 1,
        device_labels: app_keys
            .get_all_device_labels()
            .into_iter()
            .map(|(identity_pubkey, labels)| StoredDeviceLabels {
                identity_pubkey,
                device_label: labels.device_label,
                client_label: labels.client_label,
                updated_at: labels.updated_at,
            })
            .collect(),
    };
    let payload_json = serde_json::to_string(&payload)?;
    let encrypted_bytes = nip44::v2::encrypt_to_bytes(&conversation_key, &payload_json)?;
    let content = base64::engine::general_purpose::STANDARD.encode(encrypted_bytes);

    build_app_keys_unsigned_event(owner_keys.public_key(), created_at, app_keys, content)
}

pub fn parse_app_keys_event(event: &Event) -> Result<DecodedAppKeysEvent> {
    verify_event_kind(event, APP_KEYS_EVENT_KIND)?;
    event.verify()?;
    if !is_app_keys_event(event) {
        return Err(CodecError::InvalidEvent("missing app-keys d tag".to_string()).into());
    }

    let mut devices = Vec::new();
    for tag in event.tags.iter() {
        let values = tag.as_slice();
        if values.first().map(|value| value.as_str()) != Some("device") {
            continue;
        }
        let identity_pubkey =
            parse_device_pubkey(values.get(1).ok_or_else(|| {
                CodecError::InvalidEvent("device tag missing pubkey".to_string())
            })?)?;
        let created_at = values
            .get(2)
            .ok_or_else(|| CodecError::InvalidEvent("device tag missing created_at".to_string()))?
            .parse::<u64>()
            .map_err(|e| CodecError::Parse(e.to_string()))?;
        devices.push(DeviceEntry {
            identity_pubkey,
            created_at: UnixSeconds(created_at),
        });
    }

    Ok(DecodedAppKeysEvent {
        owner_pubkey: OwnerPubkey::from_nostr(event.pubkey),
        created_at: UnixSeconds(event.created_at.as_u64()),
        app_keys: AppKeys::new(devices),
    })
}

pub fn parse_app_keys_event_with_labels(
    event: &Event,
    owner_keys: &Keys,
) -> Result<DecodedAppKeysEvent> {
    let mut decoded = parse_app_keys_event(event)?;
    if event.content.is_empty() {
        return Ok(decoded);
    }

    let ciphertext_bytes = base64::engine::general_purpose::STANDARD
        .decode(event.content.as_bytes())
        .map_err(|e| CodecError::Parse(e.to_string()))?;
    let conversation_key =
        nip44::v2::ConversationKey::derive(owner_keys.secret_key(), &owner_keys.public_key());
    let plaintext = String::from_utf8(nip44::v2::decrypt_to_bytes(
        &conversation_key,
        &ciphertext_bytes,
    )?)
    .map_err(|e| CodecError::Parse(e.to_string()))?;
    let payload: EncryptedAppKeysContent = serde_json::from_str(&plaintext)?;
    if payload.payload_type != APP_KEYS_LABELS_TYPE || payload.v != 1 {
        return Err(
            CodecError::InvalidEvent("unsupported app-keys labels payload".to_string()).into(),
        );
    }

    for label in payload.device_labels {
        if decoded
            .app_keys
            .get_device(&label.identity_pubkey)
            .is_none()
        {
            return Err(CodecError::InvalidEvent(
                "device label references unknown device".to_string(),
            )
            .into());
        }
        decoded.app_keys.set_device_labels(
            label.identity_pubkey,
            label.device_label,
            label.client_label,
            label.updated_at,
        );
    }

    Ok(decoded)
}

fn build_app_keys_unsigned_event(
    owner_pubkey: nostr::PublicKey,
    created_at: UnixSeconds,
    app_keys: &AppKeys,
    content: String,
) -> Result<UnsignedEvent> {
    let mut builder = EventBuilder::new(Kind::from(APP_KEYS_EVENT_KIND as u16), content)
        .tag(tag(["d", APP_KEYS_D_TAG])?)
        .tag(tag(["version", APP_KEYS_VERSION])?)
        .custom_created_at(Timestamp::from(created_at.get()));

    for device in app_keys.get_all_devices() {
        builder = builder.tag(tag([
            "device",
            &device.identity_pubkey.to_string(),
            &device.created_at.get().to_string(),
        ])?);
    }

    Ok(builder.build(owner_pubkey))
}

fn verify_event_kind(event: &Event, expected_kind: u32) -> Result<()> {
    if event.kind != Kind::from(expected_kind as u16) {
        return Err(CodecError::InvalidEvent(format!(
            "unexpected kind {}, expected {}",
            event.kind, expected_kind
        ))
        .into());
    }
    Ok(())
}

fn tag<const N: usize>(parts: [&str; N]) -> Result<Tag> {
    Tag::parse(parts.map(str::to_owned)).map_err(|e| CodecError::InvalidEvent(e.to_string()).into())
}

fn required_tag_value(event: &Event, key: &str) -> Result<String> {
    optional_tag_value(event, key)
        .ok_or_else(|| CodecError::InvalidEvent(format!("missing `{key}` tag")).into())
}

fn optional_tag_value(event: &Event, key: &str) -> Option<String> {
    event
        .tags
        .iter()
        .find(|tag| tag.as_slice().first().map(|value| value.as_str()) == Some(key))
        .and_then(|tag| tag.as_slice().get(1).map(ToOwned::to_owned))
}

fn parse_owner_pubkey(value: &str) -> Result<OwnerPubkey> {
    Ok(OwnerPubkey::from_nostr(nostr::PublicKey::parse(value)?))
}

fn parse_device_pubkey(value: &str) -> Result<DevicePubkey> {
    Ok(DevicePubkey::from_nostr(nostr::PublicKey::parse(value)?))
}

fn parse_hex_32(value: &str) -> Result<[u8; 32]> {
    let bytes = hex::decode(value)?;
    <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| CodecError::Parse("expected 32-byte hex".to_string()).into())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredDeviceLabels {
    identity_pubkey: DevicePubkey,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_label: Option<String>,
    updated_at: UnixSeconds,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EncryptedAppKeysContent {
    #[serde(rename = "type")]
    payload_type: String,
    v: u8,
    device_labels: Vec<StoredDeviceLabels>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{device_pubkey_from_secret_bytes, ProtocolContext, UnixMillis};
    use rand::{rngs::StdRng, SeedableRng};

    fn context(seed: u64) -> ProtocolContext<'static, StdRng> {
        let rng = Box::new(StdRng::seed_from_u64(seed));
        let rng = Box::leak(rng);
        ProtocolContext::new(
            UnixSeconds(1_700_000_500),
            UnixMillis(1_700_000_500_123),
            rng,
        )
    }

    #[test]
    fn direct_message_event_roundtrip() {
        let signer_secret = [21u8; 32];
        let sender = device_pubkey_from_secret_bytes(&signer_secret).unwrap();
        let event = direct_message_event(&OutgoingDirectMessageEnvelope {
            sender,
            signer_secret_key: signer_secret,
            created_at: UnixSeconds(10),
            encrypted_header: "header".to_string(),
            ciphertext: "ciphertext".to_string(),
        })
        .unwrap();

        let parsed = parse_direct_message_event(&event).unwrap();
        assert_eq!(parsed.sender, sender);
        assert_eq!(parsed.encrypted_header, "header");
        assert_eq!(parsed.ciphertext, "ciphertext");
    }

    #[test]
    fn invite_url_and_event_roundtrip() {
        let owner_keys = Keys::generate();
        let owner_pubkey = OwnerPubkey::from_nostr(owner_keys.public_key());
        let mut ctx = context(1);
        let invite = Invite::create_new(
            &mut ctx,
            owner_pubkey,
            Some(DeviceId::new("device-a")),
            None,
        )
        .unwrap();

        let url = invite_url(&invite, "https://chat.iris.to").unwrap();
        let parsed_from_url = parse_invite_url(&url).unwrap();
        assert_eq!(parsed_from_url.inviter, invite.inviter);
        assert_eq!(
            parsed_from_url.inviter_ephemeral_public_key,
            invite.inviter_ephemeral_public_key
        );

        let unsigned = invite_unsigned_event(&invite).unwrap();
        let signed = unsigned.sign_with_keys(&owner_keys).unwrap();
        let parsed_from_event = parse_invite_event(&signed).unwrap();
        assert_eq!(parsed_from_event.inviter, invite.inviter);
        assert_eq!(parsed_from_event.device_id, invite.device_id);
    }

    #[test]
    fn app_keys_event_roundtrip_preserves_labels() {
        let owner_keys = Keys::generate();
        let device = DevicePubkey::from_nostr(Keys::generate().public_key());
        let mut app_keys = AppKeys::new(vec![DeviceEntry {
            identity_pubkey: device,
            created_at: UnixSeconds(100),
        }]);
        app_keys.set_device_labels(
            device,
            Some("Office Laptop".to_string()),
            Some("NDR Desktop".to_string()),
            UnixSeconds(200),
        );

        let unsigned =
            app_keys_encrypted_unsigned_event(&owner_keys, UnixSeconds(300), &app_keys).unwrap();
        let signed = unsigned.sign_with_keys(&owner_keys).unwrap();

        let public = parse_app_keys_event(&signed).unwrap();
        assert!(public.app_keys.get_device_labels(&device).is_none());

        let owner = parse_app_keys_event_with_labels(&signed, &owner_keys).unwrap();
        let labels = owner.app_keys.get_device_labels(&device).unwrap();
        assert_eq!(labels.device_label.as_deref(), Some("Office Laptop"));
        assert_eq!(labels.client_label.as_deref(), Some("NDR Desktop"));
        assert_eq!(labels.updated_at, UnixSeconds(200));
    }

    #[test]
    fn invite_response_event_roundtrip() {
        let sender_secret = [22u8; 32];
        let sender = device_pubkey_from_secret_bytes(&sender_secret).unwrap();
        let recipient = DevicePubkey::from_nostr(Keys::generate().public_key());

        let event = invite_response_event(&OutgoingInviteResponseEnvelope {
            sender,
            signer_secret_key: sender_secret,
            recipient,
            created_at: UnixSeconds(25),
            content: "payload".to_string(),
        })
        .unwrap();

        let parsed = parse_invite_response_event(&event).unwrap();
        assert_eq!(parsed.sender, sender);
        assert_eq!(parsed.created_at, UnixSeconds(25));
        assert_eq!(parsed.content, "payload");
    }
}
