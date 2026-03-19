pub mod domain;

use crate::{pubsub::NostrPubSub, Error, Result, Session};
use nostr::PublicKey;
use nostr::{Keys, UnsignedEvent};

#[derive(Clone)]
pub struct Invite {
    pub inviter_ephemeral_public_key: PublicKey,
    pub shared_secret: [u8; 32],
    pub inviter: PublicKey,
    pub inviter_ephemeral_private_key: Option<[u8; 32]>,
    pub device_id: Option<String>,
    pub max_uses: Option<usize>,
    pub used_by: Vec<PublicKey>,
    pub created_at: u64,
    pub purpose: Option<String>,
    pub owner_public_key: Option<PublicKey>,
}

pub struct InviteResponse {
    pub session: Session,
    pub invitee_identity: PublicKey,
    pub device_id: Option<String>,
    pub owner_public_key: Option<PublicKey>,
}

impl InviteResponse {
    /// Resolve the chat owner identity from the response.
    /// Falls back to invitee identity when owner is not provided.
    pub fn resolved_owner_pubkey(&self) -> PublicKey {
        self.owner_public_key.unwrap_or(self.invitee_identity)
    }

    /// Validate that the claimed owner can legitimately represent invitee_identity.
    ///
    /// Rules:
    /// - Single-device users are always valid: owner == invitee_identity.
    /// - Multi-device claims require AppKeys proof listing invitee_identity as a device.
    pub fn has_verified_owner_claim(&self, app_keys: Option<&crate::AppKeys>) -> bool {
        let owner = self.resolved_owner_pubkey();
        if owner == self.invitee_identity {
            return true;
        }
        app_keys
            .and_then(|keys| keys.get_device(&self.invitee_identity))
            .is_some()
    }
}

impl Invite {
    pub fn create_new(
        inviter: PublicKey,
        device_id: Option<String>,
        max_uses: Option<usize>,
    ) -> Result<Self> {
        let inviter_ephemeral_private_key = Keys::generate().secret_key().to_secret_bytes();
        let shared_secret = Keys::generate().secret_key().to_secret_bytes();
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        domain::create_invite(
            inviter,
            device_id,
            max_uses,
            domain::InviteCreateRuntime {
                inviter_ephemeral_private_key,
                shared_secret,
                created_at,
            },
        )
    }

    pub fn get_url(&self, root: &str) -> Result<String> {
        domain::invite_url(self, root)
    }

    pub fn from_url(url: &str) -> Result<Self> {
        domain::invite_from_url(url)
    }

    pub fn get_event(&self) -> Result<UnsignedEvent> {
        domain::invite_event(self)
    }

    pub fn from_event(event: &nostr::Event) -> Result<Self> {
        domain::invite_from_event(event)
    }

    pub fn accept(
        &self,
        invitee_public_key: PublicKey,
        invitee_private_key: [u8; 32],
        device_id: Option<String>,
    ) -> Result<(Session, nostr::Event)> {
        self.accept_with_owner(invitee_public_key, invitee_private_key, device_id, None)
    }

    pub fn accept_with_owner(
        &self,
        invitee_public_key: PublicKey,
        invitee_private_key: [u8; 32],
        device_id: Option<String>,
        owner_public_key: Option<PublicKey>,
    ) -> Result<(Session, nostr::Event)> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let two_days = 2 * 24 * 60 * 60;
        let random_now = now - (rand::random::<u64>() % two_days);
        let result = domain::accept_invite(
            self,
            invitee_public_key,
            invitee_private_key,
            device_id,
            owner_public_key,
            domain::InviteAcceptRuntime {
                invitee_session_private_key: Keys::generate().secret_key().to_secret_bytes(),
                random_sender_private_key: Keys::generate().secret_key().to_secret_bytes(),
                inner_created_at: now,
                envelope_created_at: random_now,
                session_init: crate::session::domain::SessionInitRuntime {
                    our_next_private_key: Some(Keys::generate().secret_key().to_secret_bytes()),
                },
            },
        )?;

        Ok((
            Session::new(result.session_state, "session".to_string()),
            result.response_event,
        ))
    }

    pub fn serialize(&self) -> Result<String> {
        let data = serde_json::json!({
            "inviterEphemeralPublicKey": hex::encode(self.inviter_ephemeral_public_key.to_bytes()),
            "sharedSecret": hex::encode(self.shared_secret),
            "inviter": hex::encode(self.inviter.to_bytes()),
            "inviterEphemeralPrivateKey": self.inviter_ephemeral_private_key.map(hex::encode),
            "deviceId": self.device_id,
            "maxUses": self.max_uses,
            "usedBy": self.used_by.iter().map(|pk| hex::encode(pk.to_bytes())).collect::<Vec<_>>(),
            "createdAt": self.created_at,
            "purpose": self.purpose.clone(),
            "ownerPublicKey": self
                .owner_public_key
                .as_ref()
                .map(|pk| hex::encode(pk.to_bytes())),
        });
        Ok(data.to_string())
    }

    pub fn deserialize(json: &str) -> Result<Self> {
        let data: serde_json::Value = serde_json::from_str(json)?;

        let inviter_ephemeral_public_key = crate::utils::pubkey_from_hex(
            data["inviterEphemeralPublicKey"]
                .as_str()
                .ok_or(Error::Invite("Missing field".to_string()))?,
        )?;

        let shared_secret_hex = data["sharedSecret"]
            .as_str()
            .ok_or(Error::Invite("Missing sharedSecret".to_string()))?;
        let shared_secret_bytes = hex::decode(shared_secret_hex)?;
        let mut shared_secret = [0u8; 32];
        shared_secret.copy_from_slice(&shared_secret_bytes);

        let inviter = crate::utils::pubkey_from_hex(
            data["inviter"]
                .as_str()
                .ok_or(Error::Invite("Missing inviter".to_string()))?,
        )?;

        let inviter_ephemeral_private_key =
            if let Some(hex_str) = data["inviterEphemeralPrivateKey"].as_str() {
                let bytes = hex::decode(hex_str)?;
                let mut array = [0u8; 32];
                array.copy_from_slice(&bytes);
                Some(array)
            } else {
                None
            };

        let used_by = if let Some(arr) = data["usedBy"].as_array() {
            arr.iter()
                .filter_map(|v| v.as_str())
                .filter_map(|s| crate::utils::pubkey_from_hex(s).ok())
                .collect()
        } else {
            Vec::new()
        };

        let purpose = data["purpose"].as_str().map(|s| s.to_string());
        let owner_public_key = data["ownerPublicKey"]
            .as_str()
            .and_then(|s| crate::utils::pubkey_from_hex(s).ok());

        Ok(Self {
            inviter_ephemeral_public_key,
            shared_secret,
            inviter,
            inviter_ephemeral_private_key,
            device_id: data["deviceId"].as_str().map(String::from),
            max_uses: data["maxUses"].as_u64().map(|u| u as usize),
            used_by,
            created_at: data["createdAt"].as_u64().unwrap_or(0),
            purpose,
            owner_public_key,
        })
    }

    pub fn listen_with_pubsub(&self, pubsub: &dyn NostrPubSub) -> Result<String> {
        let filter_json = serde_json::to_string(&domain::listen_filter(self))?;
        let subid = format!("invite-response-{}", uuid::Uuid::new_v4());
        pubsub.subscribe(subid.clone(), filter_json)?;
        Ok(subid)
    }

    pub fn listen(
        &self,
        event_tx: &crossbeam_channel::Sender<crate::SessionManagerEvent>,
    ) -> Result<()> {
        let _ = self.listen_with_pubsub(event_tx)?;
        Ok(())
    }

    pub fn from_user_with_pubsub(
        user_pubkey: PublicKey,
        pubsub: &dyn crate::NostrPubSub,
    ) -> Result<String> {
        let filter_json = serde_json::to_string(&domain::from_user_filter(user_pubkey))?;
        let subid = format!("invite-user-{}", uuid::Uuid::new_v4());
        pubsub.subscribe(subid.clone(), filter_json)?;
        Ok(subid)
    }

    pub fn from_user(
        user_pubkey: PublicKey,
        event_tx: &crossbeam_channel::Sender<crate::SessionManagerEvent>,
    ) -> Result<()> {
        let _ = Self::from_user_with_pubsub(user_pubkey, event_tx)?;
        Ok(())
    }

    pub fn process_invite_response(
        &self,
        event: &nostr::Event,
        inviter_private_key: [u8; 32],
    ) -> Result<Option<InviteResponse>> {
        let Some(processing) = domain::process_invite_response(self, event, inviter_private_key)?
        else {
            return Ok(None);
        };

        Ok(Some(InviteResponse {
            session: Session::new(processing.response.session_state, event.id.to_string()),
            invitee_identity: processing.response.invitee_identity,
            device_id: processing.response.device_id,
            owner_public_key: processing.response.owner_public_key,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::Invite;
    use nostr::Keys;

    #[test]
    fn from_event_preserves_device_scoped_invites() {
        let keys = Keys::generate();
        let device_id = keys.public_key().to_hex();
        let invite = Invite::create_new(keys.public_key(), Some(device_id.clone()), None).unwrap();
        let event = invite.get_event().unwrap().sign_with_keys(&keys).unwrap();

        let parsed = Invite::from_event(&event).unwrap();

        assert_eq!(parsed.device_id, Some(device_id));
    }

    #[test]
    fn from_event_maps_public_invites_back_to_inviter_device() {
        let keys = Keys::generate();
        let invite =
            Invite::create_new(keys.public_key(), Some("public".to_string()), None).unwrap();
        let event = invite.get_event().unwrap().sign_with_keys(&keys).unwrap();

        let parsed = Invite::from_event(&event).unwrap();

        assert_eq!(parsed.device_id, None);
        assert_eq!(parsed.inviter, keys.public_key());
    }
}
