use crate::{
    device_pubkey_from_secret_bytes, kdf, random_secret_key_bytes, secret_key_from_bytes,
    CodecError, DevicePubkey, DomainError, ProtocolContext, Result, UnixSeconds, CHAT_MESSAGE_KIND,
    MAX_SKIP, REACTION_KIND, RECEIPT_KIND, TYPING_KIND,
};
use base64::Engine;
use nostr::nips::nip44::{self, Version};
use rand::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Header {
    pub number: u32,
    pub previous_chain_length: u32,
    pub next_public_key: DevicePubkey,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SerializableKeyPair {
    pub public_key: DevicePubkey,
    #[serde(with = "serde_bytes_array")]
    pub private_key: [u8; 32],
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SkippedKeysEntry {
    #[serde(with = "serde_btreemap_u32_bytes")]
    pub message_keys: BTreeMap<u32, [u8; 32]>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionState {
    #[serde(with = "serde_bytes_array")]
    pub root_key: [u8; 32],
    pub their_current_nostr_public_key: Option<DevicePubkey>,
    pub their_next_nostr_public_key: Option<DevicePubkey>,
    pub our_current_nostr_key: Option<SerializableKeyPair>,
    pub our_next_nostr_key: SerializableKeyPair,
    #[serde(default, with = "serde_option_bytes_array")]
    pub receiving_chain_key: Option<[u8; 32]>,
    #[serde(default, with = "serde_option_bytes_array")]
    pub sending_chain_key: Option<[u8; 32]>,
    pub sending_chain_message_number: u32,
    pub receiving_chain_message_number: u32,
    pub previous_sending_chain_message_count: u32,
    pub skipped_keys: BTreeMap<DevicePubkey, SkippedKeysEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rumor {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub pubkey: DevicePubkey,
    pub created_at: UnixSeconds,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectMessageContent {
    Text(String),
    Reaction {
        message_id: String,
        emoji: String,
    },
    Reply {
        reply_to: String,
        text: String,
    },
    Receipt {
        receipt_type: String,
        message_ids: Vec<String>,
    },
    Typing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingDirectMessageEnvelope {
    pub sender: DevicePubkey,
    pub signer_secret_key: [u8; 32],
    pub created_at: UnixSeconds,
    pub encrypted_header: String,
    pub ciphertext: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingDirectMessageEnvelope {
    pub sender: DevicePubkey,
    pub created_at: UnixSeconds,
    pub encrypted_header: String,
    pub ciphertext: String,
}

#[derive(Debug, Clone)]
pub struct SendPlan {
    pub next_state: SessionState,
    pub envelope: OutgoingDirectMessageEnvelope,
    pub rumor: Rumor,
}

#[derive(Debug, Clone)]
pub struct SendOutcome {
    pub envelope: OutgoingDirectMessageEnvelope,
    pub rumor: Rumor,
}

#[derive(Debug, Clone)]
pub struct ReceivePlan {
    pub next_state: SessionState,
    pub rumor: Rumor,
    pub sender: DevicePubkey,
}

#[derive(Debug, Clone)]
pub struct ReceiveOutcome {
    pub rumor: Rumor,
    pub sender: DevicePubkey,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub state: SessionState,
    pub name: String,
}

impl Rumor {
    pub fn new(
        pubkey: DevicePubkey,
        created_at: UnixSeconds,
        kind: u32,
        tags: Vec<Vec<String>>,
        content: String,
    ) -> Result<Self> {
        let mut rumor = Self {
            id: None,
            pubkey,
            created_at,
            kind,
            tags,
            content,
        };
        rumor.id = Some(rumor.compute_id()?);
        Ok(rumor)
    }

    pub fn from_content<R>(
        ctx: &mut ProtocolContext<'_, R>,
        content: DirectMessageContent,
    ) -> Result<Self>
    where
        R: RngCore + CryptoRng,
    {
        let random_secret = random_secret_key_bytes(ctx.rng)?;
        let pubkey = device_pubkey_from_secret_bytes(&random_secret)?;
        let ms_tag = vec!["ms".to_string(), ctx.now_millis.get().to_string()];
        match content {
            DirectMessageContent::Text(text) => {
                Self::new(pubkey, ctx.now_secs, 1, vec![ms_tag], text)
            }
            DirectMessageContent::Reaction { message_id, emoji } => Self::new(
                pubkey,
                ctx.now_secs,
                REACTION_KIND,
                vec![vec!["e".to_string(), message_id], ms_tag],
                emoji,
            ),
            DirectMessageContent::Reply { reply_to, text } => Self::new(
                pubkey,
                ctx.now_secs,
                CHAT_MESSAGE_KIND,
                vec![vec!["e".to_string(), reply_to], ms_tag],
                text,
            ),
            DirectMessageContent::Receipt {
                receipt_type,
                message_ids,
            } => {
                let mut tags: Vec<Vec<String>> = message_ids
                    .into_iter()
                    .map(|id| vec!["e".to_string(), id])
                    .collect();
                tags.push(ms_tag);
                Self::new(pubkey, ctx.now_secs, RECEIPT_KIND, tags, receipt_type)
            }
            DirectMessageContent::Typing => Self::new(
                pubkey,
                ctx.now_secs,
                TYPING_KIND,
                vec![ms_tag],
                "typing".to_string(),
            ),
        }
    }

    pub fn from_json(json: &str) -> Result<Self> {
        let mut rumor: Self = serde_json::from_str(json)?;
        rumor.id = Some(rumor.compute_id()?);
        Ok(rumor)
    }

    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    fn compute_id(&self) -> Result<String> {
        let canonical = serde_json::json!([
            0,
            self.pubkey.to_string(),
            self.created_at.get(),
            self.kind,
            self.tags,
            self.content
        ]);
        let canonical_json = serde_json::to_string(&canonical)?;
        Ok(hex::encode(Sha256::digest(canonical_json.as_bytes())))
    }
}

impl Session {
    pub fn new(state: SessionState, name: String) -> Self {
        Self { state, name }
    }

    pub fn init<R>(
        ctx: &mut ProtocolContext<'_, R>,
        their_ephemeral_nostr_public_key: DevicePubkey,
        our_ephemeral_nostr_private_key: [u8; 32],
        is_initiator: bool,
        shared_secret: [u8; 32],
        name: Option<String>,
    ) -> Result<Self>
    where
        R: RngCore + CryptoRng,
    {
        let our_keys = nostr::Keys::new(secret_key_from_bytes(&our_ephemeral_nostr_private_key)?);
        let our_next_private_key = random_secret_key_bytes(ctx.rng)?;
        let our_next_keys = nostr::Keys::new(secret_key_from_bytes(&our_next_private_key)?);

        let (root_key, sending_chain_key, our_current_nostr_key, our_next_nostr_key);

        if is_initiator {
            let our_current_pubkey = DevicePubkey::from_nostr(our_keys.public_key());
            let conversation_key = nip44::v2::ConversationKey::derive(
                our_next_keys.secret_key(),
                &their_ephemeral_nostr_public_key.to_nostr()?,
            );
            let kdf_outputs = kdf(&shared_secret, conversation_key.as_bytes(), 2);
            root_key = kdf_outputs[0];
            sending_chain_key = Some(kdf_outputs[1]);
            our_current_nostr_key = Some(SerializableKeyPair {
                public_key: our_current_pubkey,
                private_key: our_ephemeral_nostr_private_key,
            });
            our_next_nostr_key = SerializableKeyPair {
                public_key: DevicePubkey::from_nostr(our_next_keys.public_key()),
                private_key: our_next_private_key,
            };
        } else {
            root_key = shared_secret;
            sending_chain_key = None;
            our_current_nostr_key = None;
            our_next_nostr_key = SerializableKeyPair {
                public_key: DevicePubkey::from_nostr(our_keys.public_key()),
                private_key: our_ephemeral_nostr_private_key,
            };
        }

        Ok(Self {
            state: SessionState {
                root_key,
                their_current_nostr_public_key: None,
                their_next_nostr_public_key: Some(their_ephemeral_nostr_public_key),
                our_current_nostr_key,
                our_next_nostr_key,
                receiving_chain_key: None,
                sending_chain_key,
                sending_chain_message_number: 0,
                receiving_chain_message_number: 0,
                previous_sending_chain_message_count: 0,
                skipped_keys: BTreeMap::new(),
            },
            name: name.unwrap_or_else(|| "session".to_string()),
        })
    }

    pub fn can_send(&self) -> bool {
        self.state.their_next_nostr_public_key.is_some()
            && self.state.our_current_nostr_key.is_some()
    }

    pub fn matches_sender(&self, sender: DevicePubkey) -> bool {
        self.state.their_current_nostr_public_key == Some(sender)
            || self.state.their_next_nostr_public_key == Some(sender)
    }

    pub fn plan_send(&self, rumor: &Rumor, now: UnixSeconds) -> Result<SendPlan> {
        if !self.can_send() {
            return Err(DomainError::NotInitiator.into());
        }

        let mut next_state = self.state.clone();
        let rumor_json = rumor.to_json()?;
        let (header, ciphertext) = ratchet_encrypt(&mut next_state, &rumor_json)?;
        let our_current = self
            .state
            .our_current_nostr_key
            .as_ref()
            .ok_or(DomainError::SessionNotReady)?;
        let our_secret = secret_key_from_bytes(&our_current.private_key)?;
        let their_next = self
            .state
            .their_next_nostr_public_key
            .ok_or(DomainError::SessionNotReady)?;
        let encrypted_header = nip44::encrypt(
            &our_secret,
            &their_next.to_nostr()?,
            &serde_json::to_string(&header)?,
            Version::V2,
        )?;

        Ok(SendPlan {
            next_state,
            envelope: OutgoingDirectMessageEnvelope {
                sender: our_current.public_key,
                signer_secret_key: our_current.private_key,
                created_at: now,
                encrypted_header,
                ciphertext,
            },
            rumor: rumor.clone(),
        })
    }

    pub fn apply_send(&mut self, plan: SendPlan) -> SendOutcome {
        self.state = plan.next_state;
        SendOutcome {
            envelope: plan.envelope,
            rumor: plan.rumor,
        }
    }

    pub fn plan_receive<R>(
        &self,
        ctx: &mut ProtocolContext<'_, R>,
        envelope: &IncomingDirectMessageEnvelope,
    ) -> Result<ReceivePlan>
    where
        R: RngCore + CryptoRng,
    {
        if !self.matches_sender(envelope.sender) {
            return Err(DomainError::UnexpectedSender.into());
        }

        let mut next_state = self.state.clone();
        let (header, should_ratchet) =
            decrypt_header(&next_state, &envelope.encrypted_header, envelope.sender)?;

        let expected_next = next_state.their_next_nostr_public_key;
        if expected_next != Some(header.next_public_key) {
            next_state.their_current_nostr_public_key = next_state.their_next_nostr_public_key;
            next_state.their_next_nostr_public_key = Some(header.next_public_key);
        }

        if should_ratchet {
            if next_state.receiving_chain_key.is_some() {
                skip_message_keys(
                    &mut next_state,
                    header.previous_chain_length,
                    envelope.sender,
                )?;
            }
            ratchet_step(&mut next_state, ctx.rng)?;
        }

        let plaintext = ratchet_decrypt(
            &mut next_state,
            &header,
            &envelope.ciphertext,
            envelope.sender,
        )?;
        let rumor = Rumor::from_json(&plaintext)?;

        Ok(ReceivePlan {
            next_state,
            rumor,
            sender: envelope.sender,
        })
    }

    pub fn apply_receive(&mut self, plan: ReceivePlan) -> ReceiveOutcome {
        self.state = plan.next_state;
        ReceiveOutcome {
            rumor: plan.rumor,
            sender: plan.sender,
        }
    }
}

fn ratchet_encrypt(state: &mut SessionState, plaintext: &str) -> Result<(Header, String)> {
    let sending_chain_key = state
        .sending_chain_key
        .ok_or(DomainError::SessionNotReady)?;

    let kdf_outputs = kdf(&sending_chain_key, &[1u8], 2);
    state.sending_chain_key = Some(kdf_outputs[0]);
    let message_key = kdf_outputs[1];

    let header = Header {
        number: state.sending_chain_message_number,
        next_public_key: state.our_next_nostr_key.public_key,
        previous_chain_length: state.previous_sending_chain_message_count,
    };

    state.sending_chain_message_number += 1;

    let conversation_key = nip44::v2::ConversationKey::new(message_key);
    let encrypted_bytes = nip44::v2::encrypt_to_bytes(&conversation_key, plaintext)?;
    let ciphertext = base64::engine::general_purpose::STANDARD.encode(encrypted_bytes);
    Ok((header, ciphertext))
}

fn ratchet_decrypt(
    state: &mut SessionState,
    header: &Header,
    ciphertext: &str,
    sender: DevicePubkey,
) -> Result<String> {
    if let Some(plaintext) = try_skipped_message_keys(state, header, ciphertext, sender)? {
        return Ok(plaintext);
    }

    if state.receiving_chain_key.is_none() {
        return Err(DomainError::SessionNotReady.into());
    }

    skip_message_keys(state, header.number, sender)?;

    let receiving_chain_key = state
        .receiving_chain_key
        .ok_or(DomainError::SessionNotReady)?;

    let kdf_outputs = kdf(&receiving_chain_key, &[1u8], 2);
    state.receiving_chain_key = Some(kdf_outputs[0]);
    let message_key = kdf_outputs[1];
    state.receiving_chain_message_number += 1;

    let conversation_key = nip44::v2::ConversationKey::new(message_key);
    let ciphertext_bytes = base64::engine::general_purpose::STANDARD
        .decode(ciphertext)
        .map_err(|e| crate::Error::Decryption(e.to_string()))?;

    let plaintext_bytes = nip44::v2::decrypt_to_bytes(&conversation_key, &ciphertext_bytes)?;
    String::from_utf8(plaintext_bytes).map_err(|e| crate::Error::Decryption(e.to_string()))
}

fn ratchet_step<R>(state: &mut SessionState, rng: &mut R) -> Result<()>
where
    R: RngCore + CryptoRng,
{
    state.previous_sending_chain_message_count = state.sending_chain_message_number;
    state.sending_chain_message_number = 0;
    state.receiving_chain_message_number = 0;

    let our_next_sk = secret_key_from_bytes(&state.our_next_nostr_key.private_key)?;
    let their_next_pk = state
        .their_next_nostr_public_key
        .ok_or(DomainError::SessionNotReady)?;

    let conversation_key1 =
        nip44::v2::ConversationKey::derive(&our_next_sk, &their_next_pk.to_nostr()?);
    let kdf_outputs = kdf(&state.root_key, conversation_key1.as_bytes(), 2);
    state.receiving_chain_key = Some(kdf_outputs[1]);
    state.our_current_nostr_key = Some(state.our_next_nostr_key.clone());

    let our_next_private_key = random_secret_key_bytes(rng)?;
    state.our_next_nostr_key = SerializableKeyPair {
        public_key: device_pubkey_from_secret_bytes(&our_next_private_key)?,
        private_key: our_next_private_key,
    };

    let our_next_sk2 = secret_key_from_bytes(&our_next_private_key)?;
    let conversation_key2 =
        nip44::v2::ConversationKey::derive(&our_next_sk2, &their_next_pk.to_nostr()?);
    let kdf_outputs2 = kdf(&kdf_outputs[0], conversation_key2.as_bytes(), 2);
    state.root_key = kdf_outputs2[0];
    state.sending_chain_key = Some(kdf_outputs2[1]);
    Ok(())
}

fn skip_message_keys(state: &mut SessionState, until: u32, sender: DevicePubkey) -> Result<()> {
    if until <= state.receiving_chain_message_number {
        return Ok(());
    }

    if (until - state.receiving_chain_message_number) as usize > MAX_SKIP {
        return Err(DomainError::TooManySkippedMessages.into());
    }

    let entry = state
        .skipped_keys
        .entry(sender)
        .or_insert_with(SkippedKeysEntry::default);

    while state.receiving_chain_message_number < until {
        let receiving_chain_key = state
            .receiving_chain_key
            .ok_or(DomainError::SessionNotReady)?;
        let kdf_outputs = kdf(&receiving_chain_key, &[1u8], 2);
        state.receiving_chain_key = Some(kdf_outputs[0]);
        entry
            .message_keys
            .insert(state.receiving_chain_message_number, kdf_outputs[1]);
        state.receiving_chain_message_number += 1;
    }

    prune_skipped_message_keys(&mut entry.message_keys);
    Ok(())
}

fn try_skipped_message_keys(
    state: &mut SessionState,
    header: &Header,
    ciphertext: &str,
    sender: DevicePubkey,
) -> Result<Option<String>> {
    if let Some(entry) = state.skipped_keys.get_mut(&sender) {
        if let Some(message_key) = entry.message_keys.remove(&header.number) {
            let conversation_key = nip44::v2::ConversationKey::new(message_key);
            let ciphertext_bytes = base64::engine::general_purpose::STANDARD
                .decode(ciphertext)
                .map_err(|e| crate::Error::Decryption(e.to_string()))?;
            let plaintext_bytes =
                nip44::v2::decrypt_to_bytes(&conversation_key, &ciphertext_bytes)?;
            let plaintext = String::from_utf8(plaintext_bytes)
                .map_err(|e| crate::Error::Decryption(e.to_string()))?;
            if entry.message_keys.is_empty() {
                state.skipped_keys.remove(&sender);
            }
            return Ok(Some(plaintext));
        }
    }

    Ok(None)
}

fn decrypt_header(
    state: &SessionState,
    encrypted_header: &str,
    sender: DevicePubkey,
) -> Result<(Header, bool)> {
    if let Some(current) = &state.our_current_nostr_key {
        let current_sk = secret_key_from_bytes(&current.private_key)?;
        if let Ok(decrypted) = nip44::decrypt(&current_sk, &sender.to_nostr()?, encrypted_header) {
            let header: Header = serde_json::from_str(&decrypted)?;
            return Ok((header, false));
        }
    }

    let next_sk = secret_key_from_bytes(&state.our_next_nostr_key.private_key)?;
    let decrypted = nip44::decrypt(&next_sk, &sender.to_nostr()?, encrypted_header)
        .map_err(|_| CodecError::InvalidHeader)?;
    let header: Header = serde_json::from_str(&decrypted)?;
    Ok((header, true))
}

fn prune_skipped_message_keys(map: &mut BTreeMap<u32, [u8; 32]>) {
    while map.len() > MAX_SKIP {
        let Some(first) = map.keys().next().copied() else {
            break;
        };
        map.remove(&first);
    }
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

mod serde_btreemap_u32_bytes {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<S>(map: &BTreeMap<u32, [u8; 32]>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let string_map: BTreeMap<String, String> = map
            .iter()
            .map(|(k, v)| (k.to_string(), hex::encode(v)))
            .collect();
        string_map.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<BTreeMap<u32, [u8; 32]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let string_map: BTreeMap<String, String> = BTreeMap::deserialize(deserializer)?;
        let mut out = BTreeMap::new();
        for (k, v) in string_map {
            let idx: u32 = k.parse().map_err(serde::de::Error::custom)?;
            out.insert(
                idx,
                super::decode_hex_32(&v).map_err(serde::de::Error::custom)?,
            );
        }
        Ok(out)
    }
}

fn decode_hex_32(value: &str) -> std::result::Result<[u8; 32], String> {
    let bytes = hex::decode(value).map_err(|e| e.to_string())?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| "invalid 32-byte hex".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UnixMillis;
    use rand::{rngs::StdRng, SeedableRng};

    fn context(seed: u64) -> ProtocolContext<'static, StdRng> {
        let rng = Box::new(StdRng::seed_from_u64(seed));
        let rng = Box::leak(rng);
        ProtocolContext::new(
            UnixSeconds(1_700_000_000),
            UnixMillis(1_700_000_000_123),
            rng,
        )
    }

    #[test]
    fn plan_send_and_apply_receive_roundtrip() {
        let alice_secret = [1u8; 32];
        let bob_secret = [2u8; 32];
        let alice_pub = device_pubkey_from_secret_bytes(&alice_secret).unwrap();
        let bob_pub = device_pubkey_from_secret_bytes(&bob_secret).unwrap();
        let shared_secret = [7u8; 32];

        let mut init_ctx_alice = context(1);
        let alice = Session::init(
            &mut init_ctx_alice,
            bob_pub,
            alice_secret,
            true,
            shared_secret,
            None,
        )
        .unwrap();
        let mut init_ctx_bob = context(2);
        let mut bob = Session::init(
            &mut init_ctx_bob,
            alice_pub,
            bob_secret,
            false,
            shared_secret,
            None,
        )
        .unwrap();

        let mut ctx = context(9);
        let rumor =
            Rumor::from_content(&mut ctx, DirectMessageContent::Text("hello".to_string())).unwrap();
        let send_plan = alice.plan_send(&rumor, ctx.now_secs).unwrap();
        let send_outcome = alice.clone().apply_send(send_plan.clone());

        let mut recv_ctx = context(10);
        let receive_plan = bob
            .plan_receive(
                &mut recv_ctx,
                &IncomingDirectMessageEnvelope {
                    sender: send_outcome.envelope.sender,
                    created_at: send_outcome.envelope.created_at,
                    encrypted_header: send_outcome.envelope.encrypted_header.clone(),
                    ciphertext: send_outcome.envelope.ciphertext.clone(),
                },
            )
            .unwrap();
        let outcome = bob.apply_receive(receive_plan);
        assert_eq!(outcome.rumor.content, "hello");
    }

    #[test]
    fn plan_receive_does_not_mutate_original_session() {
        let alice_secret = [3u8; 32];
        let bob_secret = [4u8; 32];
        let alice_pub = device_pubkey_from_secret_bytes(&alice_secret).unwrap();
        let bob_pub = device_pubkey_from_secret_bytes(&bob_secret).unwrap();
        let shared_secret = [8u8; 32];

        let mut init_ctx_alice = context(3);
        let alice = Session::init(
            &mut init_ctx_alice,
            bob_pub,
            alice_secret,
            true,
            shared_secret,
            None,
        )
        .unwrap();
        let mut init_ctx_bob = context(4);
        let bob = Session::init(
            &mut init_ctx_bob,
            alice_pub,
            bob_secret,
            false,
            shared_secret,
            None,
        )
        .unwrap();
        let bob_before = bob.state.clone();

        let mut ctx = context(12);
        let rumor = Rumor::from_content(&mut ctx, DirectMessageContent::Typing).unwrap();
        let send_plan = alice.plan_send(&rumor, ctx.now_secs).unwrap();

        let mut recv_ctx = context(13);
        let _ = bob
            .plan_receive(
                &mut recv_ctx,
                &IncomingDirectMessageEnvelope {
                    sender: send_plan.envelope.sender,
                    created_at: send_plan.envelope.created_at,
                    encrypted_header: send_plan.envelope.encrypted_header.clone(),
                    ciphertext: send_plan.envelope.ciphertext.clone(),
                },
            )
            .unwrap();

        assert_eq!(bob.state, bob_before);
    }

    #[test]
    fn duplicate_receive_fails_without_corrupting_state() {
        let alice_secret = [5u8; 32];
        let bob_secret = [6u8; 32];
        let alice_pub = device_pubkey_from_secret_bytes(&alice_secret).unwrap();
        let bob_pub = device_pubkey_from_secret_bytes(&bob_secret).unwrap();
        let shared_secret = [9u8; 32];

        let mut init_ctx_alice = context(5);
        let alice = Session::init(
            &mut init_ctx_alice,
            bob_pub,
            alice_secret,
            true,
            shared_secret,
            None,
        )
        .unwrap();
        let mut init_ctx_bob = context(6);
        let mut bob = Session::init(
            &mut init_ctx_bob,
            alice_pub,
            bob_secret,
            false,
            shared_secret,
            None,
        )
        .unwrap();

        let mut send_ctx = context(14);
        let rumor = Rumor::from_content(
            &mut send_ctx,
            DirectMessageContent::Text("hello".to_string()),
        )
        .unwrap();
        let send_plan = alice.plan_send(&rumor, send_ctx.now_secs).unwrap();
        let envelope = alice.clone().apply_send(send_plan).envelope;
        let incoming = IncomingDirectMessageEnvelope {
            sender: envelope.sender,
            created_at: envelope.created_at,
            encrypted_header: envelope.encrypted_header.clone(),
            ciphertext: envelope.ciphertext.clone(),
        };

        let mut recv_ctx = context(15);
        let first_plan = bob.plan_receive(&mut recv_ctx, &incoming).unwrap();
        let _ = bob.apply_receive(first_plan);
        let after_first = bob.state.clone();

        let mut replay_ctx = context(16);
        let replay = bob.plan_receive(&mut replay_ctx, &incoming);
        assert!(replay.is_err());
        assert_eq!(bob.state, after_first);
    }

    #[test]
    fn invalid_sender_is_rejected() {
        let alice_secret = [7u8; 32];
        let bob_secret = [8u8; 32];
        let alice_pub = device_pubkey_from_secret_bytes(&alice_secret).unwrap();
        let bob_pub = device_pubkey_from_secret_bytes(&bob_secret).unwrap();
        let shared_secret = [10u8; 32];

        let mut init_ctx_bob = context(7);
        let bob = Session::init(
            &mut init_ctx_bob,
            alice_pub,
            bob_secret,
            false,
            shared_secret,
            None,
        )
        .unwrap();

        let mut recv_ctx = context(17);
        let err = bob
            .plan_receive(
                &mut recv_ctx,
                &IncomingDirectMessageEnvelope {
                    sender: bob_pub,
                    created_at: UnixSeconds(1),
                    encrypted_header: "bad".to_string(),
                    ciphertext: "bad".to_string(),
                },
            )
            .unwrap_err();
        assert!(matches!(
            err,
            crate::Error::Domain(DomainError::UnexpectedSender)
        ));
    }
}
