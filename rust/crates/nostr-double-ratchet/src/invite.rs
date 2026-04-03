use crate::{
    random_secret_key_bytes, secret_key_from_bytes, DeviceId, DevicePubkey, OwnerPubkey,
    ProtocolContext, Result, Session, UnixSeconds,
};
use base64::Engine;
use nostr::nips::nip44::{self, Version};
use rand::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Invite {
    pub inviter_ephemeral_public_key: DevicePubkey,
    #[serde(with = "serde_bytes_array")]
    pub shared_secret: [u8; 32],
    pub inviter: OwnerPubkey,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "serde_option_bytes_array"
    )]
    pub inviter_ephemeral_private_key: Option<[u8; 32]>,
    pub device_id: Option<DeviceId>,
    pub max_uses: Option<usize>,
    pub used_by: Vec<DevicePubkey>,
    pub created_at: UnixSeconds,
    pub purpose: Option<String>,
    pub owner_public_key: Option<OwnerPubkey>,
}

#[derive(Debug, Clone)]
pub struct InviteResponse {
    pub session: Session,
    pub invitee_identity: DevicePubkey,
    pub device_id: Option<DeviceId>,
    pub owner_public_key: Option<OwnerPubkey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingInviteResponseEnvelope {
    pub sender: DevicePubkey,
    pub signer_secret_key: [u8; 32],
    pub recipient: DevicePubkey,
    pub created_at: UnixSeconds,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingInviteResponseEnvelope {
    pub sender: DevicePubkey,
    pub created_at: UnixSeconds,
    pub content: String,
}

impl InviteResponse {
    pub fn resolved_owner_pubkey(&self) -> OwnerPubkey {
        self.owner_public_key
            .unwrap_or_else(|| self.invitee_identity.as_owner())
    }

    pub fn has_verified_owner_claim(&self, app_keys: Option<&crate::AppKeys>) -> bool {
        let owner = self.resolved_owner_pubkey();
        if owner == self.invitee_identity.as_owner() {
            return true;
        }
        app_keys
            .and_then(|keys| keys.get_device(&self.invitee_identity))
            .is_some()
    }
}

impl Invite {
    pub fn create_new<R>(
        ctx: &mut ProtocolContext<'_, R>,
        inviter: OwnerPubkey,
        device_id: Option<DeviceId>,
        max_uses: Option<usize>,
    ) -> Result<Self>
    where
        R: RngCore + CryptoRng,
    {
        let inviter_ephemeral_private_key = random_secret_key_bytes(ctx.rng)?;
        let inviter_ephemeral_public_key =
            crate::device_pubkey_from_secret_bytes(&inviter_ephemeral_private_key)?;
        let shared_secret = random_secret_key_bytes(ctx.rng)?;

        Ok(Self {
            inviter_ephemeral_public_key,
            shared_secret,
            inviter,
            inviter_ephemeral_private_key: Some(inviter_ephemeral_private_key),
            device_id,
            max_uses,
            used_by: Vec::new(),
            created_at: ctx.now_secs,
            purpose: None,
            owner_public_key: None,
        })
    }

    pub fn accept<R>(
        &self,
        ctx: &mut ProtocolContext<'_, R>,
        invitee_public_key: DevicePubkey,
        invitee_private_key: [u8; 32],
        device_id: Option<DeviceId>,
    ) -> Result<(Session, OutgoingInviteResponseEnvelope)>
    where
        R: RngCore + CryptoRng,
    {
        self.accept_with_owner(
            ctx,
            invitee_public_key,
            invitee_private_key,
            device_id,
            None,
        )
    }

    pub fn accept_with_owner<R>(
        &self,
        ctx: &mut ProtocolContext<'_, R>,
        invitee_public_key: DevicePubkey,
        invitee_private_key: [u8; 32],
        device_id: Option<DeviceId>,
        owner_public_key: Option<OwnerPubkey>,
    ) -> Result<(Session, OutgoingInviteResponseEnvelope)>
    where
        R: RngCore + CryptoRng,
    {
        let invitee_session_key = random_secret_key_bytes(ctx.rng)?;
        let invitee_session_public_key =
            crate::device_pubkey_from_secret_bytes(&invitee_session_key)?;

        let session = Session::init(
            ctx,
            self.inviter_ephemeral_public_key,
            invitee_session_key,
            true,
            self.shared_secret,
            None,
        )?;

        let payload = InviteResponsePayload {
            session_key: invitee_session_public_key,
            device_id: device_id.clone(),
            owner_public_key,
        };

        let invitee_sk = secret_key_from_bytes(&invitee_private_key)?;
        let dh_encrypted = nip44::encrypt(
            &invitee_sk,
            &self.inviter.as_device().to_nostr()?,
            serde_json::to_string(&payload)?,
            Version::V2,
        )?;

        let conversation_key = nip44::v2::ConversationKey::new(self.shared_secret);
        let encrypted_bytes = nip44::v2::encrypt_to_bytes(&conversation_key, &dh_encrypted)?;
        let inner_event = InviteResponseInnerEvent {
            pubkey: invitee_public_key,
            content: base64::engine::general_purpose::STANDARD.encode(encrypted_bytes),
            created_at: ctx.now_secs,
        };

        let random_sender_secret = random_secret_key_bytes(ctx.rng)?;
        let random_sender_pubkey = crate::device_pubkey_from_secret_bytes(&random_sender_secret)?;
        let envelope_content = nip44::encrypt(
            &secret_key_from_bytes(&random_sender_secret)?,
            &self.inviter_ephemeral_public_key.to_nostr()?,
            serde_json::to_string(&inner_event)?,
            Version::V2,
        )?;

        let jitter = if ctx.now_secs.get() == 0 {
            0
        } else {
            (ctx.rng.next_u64() % (2 * 24 * 60 * 60)) as u64
        };
        let created_at = UnixSeconds(ctx.now_secs.get().saturating_sub(jitter));

        Ok((
            session,
            OutgoingInviteResponseEnvelope {
                sender: random_sender_pubkey,
                signer_secret_key: random_sender_secret,
                recipient: self.inviter_ephemeral_public_key,
                created_at,
                content: envelope_content,
            },
        ))
    }

    pub fn process_invite_response<R>(
        &self,
        ctx: &mut ProtocolContext<'_, R>,
        envelope: &IncomingInviteResponseEnvelope,
        inviter_private_key: [u8; 32],
    ) -> Result<Option<InviteResponse>>
    where
        R: RngCore + CryptoRng,
    {
        let inviter_ephemeral_private_key =
            self.inviter_ephemeral_private_key.ok_or_else(|| {
                crate::error::CodecError::Invite("ephemeral key not available".to_string())
            })?;

        let inviter_ephemeral_sk = secret_key_from_bytes(&inviter_ephemeral_private_key)?;
        let decrypted = nip44::decrypt(
            &inviter_ephemeral_sk,
            &envelope.sender.to_nostr()?,
            &envelope.content,
        )?;
        let inner_event: InviteResponseInnerEvent = serde_json::from_str(&decrypted)?;

        let ciphertext_bytes = base64::engine::general_purpose::STANDARD
            .decode(inner_event.content.as_bytes())
            .map_err(|e| crate::Error::Decryption(e.to_string()))?;
        let conversation_key = nip44::v2::ConversationKey::new(self.shared_secret);
        let dh_encrypted_ciphertext = String::from_utf8(nip44::v2::decrypt_to_bytes(
            &conversation_key,
            &ciphertext_bytes,
        )?)
        .map_err(|e| crate::Error::Decryption(e.to_string()))?;

        let inviter_sk = secret_key_from_bytes(&inviter_private_key)?;
        let dh_decrypted = nip44::decrypt(
            &inviter_sk,
            &inner_event.pubkey.to_nostr()?,
            &dh_encrypted_ciphertext,
        )?;

        let payload: InviteResponsePayload = serde_json::from_str(&dh_decrypted)?;
        let session = Session::init(
            ctx,
            payload.session_key,
            inviter_ephemeral_private_key,
            false,
            self.shared_secret,
            None,
        )?;

        Ok(Some(InviteResponse {
            session,
            invitee_identity: inner_event.pubkey,
            device_id: payload.device_id,
            owner_public_key: payload.owner_public_key,
        }))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InviteResponseInnerEvent {
    pubkey: DevicePubkey,
    content: String,
    created_at: UnixSeconds,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InviteResponsePayload {
    #[serde(rename = "sessionKey")]
    session_key: DevicePubkey,
    #[serde(rename = "deviceId", skip_serializing_if = "Option::is_none")]
    device_id: Option<DeviceId>,
    #[serde(rename = "ownerPublicKey", skip_serializing_if = "Option::is_none")]
    owner_public_key: Option<OwnerPubkey>,
}

mod serde_bytes_array {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u8; 32], D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        super::decode_hex_32(&s).map_err(serde::de::Error::custom)
    }
}

mod serde_option_bytes_array {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &Option<[u8; 32]>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match bytes {
            Some(b) => serializer.serialize_str(&hex::encode(b)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<[u8; 32]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt: Option<String> = Option::deserialize(deserializer)?;
        match opt {
            Some(s) => super::decode_hex_32(&s)
                .map(Some)
                .map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }
}

fn decode_hex_32(value: &str) -> std::result::Result<[u8; 32], String> {
    let bytes = hex::decode(value).map_err(|e| e.to_string())?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| "invalid 32-byte hex".to_string())
}
