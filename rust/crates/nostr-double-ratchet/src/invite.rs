use crate::{
    random_secret_key_bytes, secret_key_from_bytes, DevicePubkey, DeviceRoster, DomainError,
    OwnerPubkey, ProtocolContext, Result, Session, UnixSeconds,
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
    pub max_uses: Option<usize>,
    pub used_by: Vec<DevicePubkey>,
    pub created_at: UnixSeconds,
    pub owner_public_key: Option<OwnerPubkey>,
}

#[derive(Debug, Clone)]
pub struct InviteResponse {
    pub session: Session,
    pub invitee_identity: DevicePubkey,
    pub owner_public_key: Option<OwnerPubkey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteResponseEnvelope {
    pub sender: DevicePubkey,
    pub signer_secret_key: [u8; 32],
    pub recipient: DevicePubkey,
    pub created_at: UnixSeconds,
    pub content: String,
}

impl InviteResponse {
    pub fn resolved_owner_pubkey(&self) -> OwnerPubkey {
        self.owner_public_key
            .unwrap_or_else(|| self.invitee_identity.as_owner())
    }

    pub fn has_verified_owner_claim(&self, roster: Option<&DeviceRoster>) -> bool {
        let owner = self.resolved_owner_pubkey();
        if owner == self.invitee_identity.as_owner() {
            return true;
        }

        roster
            .and_then(|roster| roster.get_device(&self.invitee_identity))
            .is_some()
    }
}

impl Invite {
    pub fn create_new<R>(
        ctx: &mut ProtocolContext<'_, R>,
        inviter: OwnerPubkey,
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
            max_uses,
            used_by: Vec::new(),
            created_at: ctx.now,
            owner_public_key: None,
        })
    }

    pub fn accept<R>(
        &self,
        ctx: &mut ProtocolContext<'_, R>,
        invitee_public_key: DevicePubkey,
        invitee_private_key: [u8; 32],
    ) -> Result<(Session, InviteResponseEnvelope)>
    where
        R: RngCore + CryptoRng,
    {
        self.accept_with_owner(ctx, invitee_public_key, invitee_private_key, None)
    }

    pub fn accept_with_owner<R>(
        &self,
        ctx: &mut ProtocolContext<'_, R>,
        invitee_public_key: DevicePubkey,
        invitee_private_key: [u8; 32],
        owner_public_key: Option<OwnerPubkey>,
    ) -> Result<(Session, InviteResponseEnvelope)>
    where
        R: RngCore + CryptoRng,
    {
        self.ensure_accept_allowed(invitee_public_key)?;

        let invitee_session_key = random_secret_key_bytes(ctx.rng)?;
        let invitee_session_public_key =
            crate::device_pubkey_from_secret_bytes(&invitee_session_key)?;

        let session = Session::new_initiator(
            ctx,
            self.inviter_ephemeral_public_key,
            invitee_session_key,
            self.shared_secret,
        )?;

        let payload = InviteResponsePayload {
            session_key: invitee_session_public_key,
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
            created_at: ctx.now,
        };

        let random_sender_secret = random_secret_key_bytes(ctx.rng)?;
        let random_sender_pubkey = crate::device_pubkey_from_secret_bytes(&random_sender_secret)?;
        let envelope_content = nip44::encrypt(
            &secret_key_from_bytes(&random_sender_secret)?,
            &self.inviter_ephemeral_public_key.to_nostr()?,
            serde_json::to_string(&inner_event)?,
            Version::V2,
        )?;

        let jitter = if ctx.now.get() == 0 {
            0
        } else {
            ctx.rng.next_u64() % (2 * 24 * 60 * 60)
        };
        let created_at = UnixSeconds(ctx.now.get().saturating_sub(jitter));

        Ok((
            session,
            InviteResponseEnvelope {
                sender: random_sender_pubkey,
                signer_secret_key: random_sender_secret,
                recipient: self.inviter_ephemeral_public_key,
                created_at,
                content: envelope_content,
            },
        ))
    }

    pub fn process_response<R>(
        &mut self,
        ctx: &mut ProtocolContext<'_, R>,
        envelope: &InviteResponseEnvelope,
        inviter_private_key: [u8; 32],
    ) -> Result<InviteResponse>
    where
        R: RngCore + CryptoRng,
    {
        let inviter_ephemeral_private_key = self
            .inviter_ephemeral_private_key
            .ok_or_else(|| crate::Error::Parse("ephemeral key not available".to_string()))?;

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
        self.ensure_accept_allowed(inner_event.pubkey)?;
        let session = Session::new_responder(
            ctx,
            payload.session_key,
            inviter_ephemeral_private_key,
            self.shared_secret,
        )?;
        self.record_use(inner_event.pubkey);

        Ok(InviteResponse {
            session,
            invitee_identity: inner_event.pubkey,
            owner_public_key: payload.owner_public_key,
        })
    }

    fn ensure_accept_allowed(&self, invitee_public_key: DevicePubkey) -> Result<()> {
        if self.used_by.contains(&invitee_public_key) {
            return Err(DomainError::InviteAlreadyUsed.into());
        }
        if self
            .max_uses
            .is_some_and(|max_uses| self.used_by.len() >= max_uses)
        {
            return Err(DomainError::InviteExhausted.into());
        }
        Ok(())
    }

    fn record_use(&mut self, invitee_public_key: DevicePubkey) {
        if self.used_by.contains(&invitee_public_key) {
            return;
        }
        self.used_by.push(invitee_public_key);
        self.used_by.sort();
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
