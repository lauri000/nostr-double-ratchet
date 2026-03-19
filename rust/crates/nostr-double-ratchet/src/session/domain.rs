use crate::{
    utils::{kdf, pubkey_from_hex},
    Error, Header, Result, SerializableKeyPair, SessionState, SkippedKeysEntry, MAX_SKIP,
    MESSAGE_EVENT_KIND,
};
use base64::Engine;
use nostr::nips::nip44::{self, Version};
use nostr::{EventBuilder, Keys, PublicKey, Tag, Timestamp, UnsignedEvent};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionSubscriptions {
    pub current: Option<PublicKey>,
    pub next: Option<PublicKey>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionInitRuntime {
    pub our_next_private_key: Option<[u8; 32]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionSendRuntime {
    pub now_secs: u64,
    pub now_millis: u128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionReceiveRuntime {
    pub our_next_private_key: [u8; 32],
}

pub struct SessionSendTransition {
    pub next_state: SessionState,
    pub event: nostr::Event,
}

pub struct SessionReceiveTransition {
    pub next_state: SessionState,
    pub plaintext: Option<String>,
    pub needs_subscription_update: bool,
}

pub fn init_state(
    their_ephemeral_nostr_public_key: PublicKey,
    our_ephemeral_nostr_private_key: [u8; 32],
    is_initiator: bool,
    shared_secret: [u8; 32],
    runtime: SessionInitRuntime,
) -> Result<SessionState> {
    let our_keys = Keys::new(nostr::SecretKey::from_slice(
        &our_ephemeral_nostr_private_key,
    )?);

    let (root_key, sending_chain_key, our_current_nostr_key, our_next_nostr_key) = if is_initiator {
        let our_next_private_key = runtime.our_next_private_key.ok_or(Error::SessionNotReady)?;
        let our_next_keys = Keys::new(nostr::SecretKey::from_slice(&our_next_private_key)?);
        let conversation_key = nip44::v2::ConversationKey::derive(
            our_next_keys.secret_key(),
            &their_ephemeral_nostr_public_key,
        );
        let kdf_outputs = kdf(&shared_secret, conversation_key.as_bytes(), 2);

        (
            kdf_outputs[0],
            Some(kdf_outputs[1]),
            Some(SerializableKeyPair {
                public_key: our_keys.public_key(),
                private_key: our_ephemeral_nostr_private_key,
            }),
            SerializableKeyPair {
                public_key: our_next_keys.public_key(),
                private_key: our_next_private_key,
            },
        )
    } else {
        (
            shared_secret,
            None,
            None,
            SerializableKeyPair {
                public_key: our_keys.public_key(),
                private_key: our_ephemeral_nostr_private_key,
            },
        )
    };

    Ok(SessionState {
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
        skipped_keys: HashMap::new(),
    })
}

pub fn desired_subscriptions(state: &SessionState) -> SessionSubscriptions {
    SessionSubscriptions {
        current: state.their_current_nostr_public_key,
        next: state.their_next_nostr_public_key,
    }
}

pub fn send_event(
    state: &SessionState,
    mut event: UnsignedEvent,
    runtime: SessionSendRuntime,
) -> Result<SessionSendTransition> {
    if state.their_next_nostr_public_key.is_none() || state.our_current_nostr_key.is_none() {
        return Err(Error::NotInitiator);
    }

    let has_ms_tag = event
        .tags
        .iter()
        .any(|tag| tag.clone().to_vec().first().map(|s| s.as_str()) == Some("ms"));

    if !has_ms_tag {
        let ms_tag = Tag::parse(&["ms".to_string(), runtime.now_millis.to_string()])
            .map_err(|e| Error::InvalidEvent(e.to_string()))?;
        let mut builder = EventBuilder::new(event.kind, &event.content);
        for tag in event.tags.iter() {
            builder = builder.tag(tag.clone());
        }
        builder = builder.tag(ms_tag);
        event = builder
            .custom_created_at(event.created_at)
            .build(event.pubkey);
    }

    event.id = None;
    event.ensure_id();

    let rumor_json = serde_json::to_string(&event)?;
    let mut next_state = state.clone();
    let (header, encrypted_data) = ratchet_encrypt(&mut next_state, &rumor_json)?;

    let our_current = next_state
        .our_current_nostr_key
        .as_ref()
        .ok_or(Error::NotInitiator)?;
    let their_next = next_state
        .their_next_nostr_public_key
        .ok_or(Error::NotInitiator)?;

    let our_sk = nostr::SecretKey::from_slice(&our_current.private_key)?;
    let encrypted_header = nip44::encrypt(
        &our_sk,
        &their_next,
        &serde_json::to_string(&header)?,
        Version::V2,
    )?;

    let tags = vec![Tag::parse(&["header".to_string(), encrypted_header])
        .map_err(|e| Error::InvalidEvent(e.to_string()))?];

    let unsigned_event =
        EventBuilder::new(nostr::Kind::from(MESSAGE_EVENT_KIND as u16), encrypted_data)
            .tags(tags)
            .custom_created_at(Timestamp::from(runtime.now_secs))
            .build(our_current.public_key);

    let author_secret_key = nostr::SecretKey::from_slice(&our_current.private_key)?;
    let author_keys = nostr::Keys::new(author_secret_key);
    let signed_event = unsigned_event
        .sign_with_keys(&author_keys)
        .map_err(|e| Error::InvalidEvent(e.to_string()))?;

    Ok(SessionSendTransition {
        next_state,
        event: signed_event,
    })
}

pub fn receive_event(
    state: &SessionState,
    event: &nostr::Event,
    runtime: SessionReceiveRuntime,
) -> Result<SessionReceiveTransition> {
    let mut next_state = state.clone();

    let header_tag = event
        .tags
        .iter()
        .find(|tag| tag.as_slice().first().map(|s| s.as_str()) == Some("header"))
        .cloned();

    let encrypted_header = match header_tag {
        Some(tag) => {
            let values = tag.to_vec();
            values.get(1).ok_or(Error::InvalidHeader)?.clone()
        }
        None => return Err(Error::InvalidHeader),
    };

    let sender_pubkey = event.pubkey;
    let (header, should_ratchet) = decrypt_header(&next_state, &encrypted_header, &sender_pubkey)?;

    let sender_bytes = sender_pubkey.to_bytes();
    let their_next_matches = next_state
        .their_next_nostr_public_key
        .as_ref()
        .map(|pk| pk.to_bytes() == sender_bytes)
        .unwrap_or(false);
    let their_current_matches = next_state
        .their_current_nostr_public_key
        .as_ref()
        .map(|pk| pk.to_bytes() == sender_bytes)
        .unwrap_or(false);

    if !their_next_matches && !their_current_matches {
        return Err(Error::InvalidEvent("Unexpected sender".to_string()));
    }

    let their_next_pk_hex = next_state
        .their_next_nostr_public_key
        .map(|pk| hex::encode(pk.to_bytes()))
        .unwrap_or_default();

    if header.next_public_key != their_next_pk_hex {
        next_state.their_current_nostr_public_key = next_state.their_next_nostr_public_key;
        next_state.their_next_nostr_public_key = Some(pubkey_from_hex(&header.next_public_key)?);
    }

    let mut needs_subscription_update = false;
    if should_ratchet {
        if next_state.receiving_chain_key.is_some() {
            skip_message_keys(
                &mut next_state,
                header.previous_chain_length,
                &sender_pubkey,
            )?;
        }
        ratchet_step(&mut next_state, runtime.our_next_private_key)?;
        needs_subscription_update = true;
    }

    let plaintext = ratchet_decrypt(&mut next_state, &header, &event.content, &sender_pubkey)?;

    Ok(SessionReceiveTransition {
        next_state,
        plaintext: Some(normalize_inner_rumor_id(&plaintext)),
        needs_subscription_update,
    })
}

pub fn skip_message_keys(
    state: &mut SessionState,
    until: u32,
    nostr_sender: &PublicKey,
) -> Result<()> {
    if until <= state.receiving_chain_message_number {
        return Ok(());
    }

    if (until - state.receiving_chain_message_number) as usize > MAX_SKIP {
        return Err(Error::TooManySkippedMessages);
    }

    let entry = state
        .skipped_keys
        .entry(*nostr_sender)
        .or_insert_with(|| SkippedKeysEntry {
            header_keys: Vec::new(),
            message_keys: HashMap::new(),
        });

    while state.receiving_chain_message_number < until {
        let receiving_chain_key = state.receiving_chain_key.ok_or(Error::SessionNotReady)?;

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

fn ratchet_encrypt(state: &mut SessionState, plaintext: &str) -> Result<(Header, String)> {
    let sending_chain_key = state.sending_chain_key.ok_or(Error::SessionNotReady)?;

    let kdf_outputs = kdf(&sending_chain_key, &[1u8], 2);
    state.sending_chain_key = Some(kdf_outputs[0]);
    let message_key = kdf_outputs[1];

    let header = Header {
        number: state.sending_chain_message_number,
        next_public_key: hex::encode(state.our_next_nostr_key.public_key.to_bytes()),
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
    nostr_sender: &PublicKey,
) -> Result<String> {
    if let Some(plaintext) = try_skipped_message_keys(state, header, ciphertext, nostr_sender)? {
        return Ok(plaintext);
    }

    if state.receiving_chain_key.is_none() {
        return Err(Error::SessionNotReady);
    }

    skip_message_keys(state, header.number, nostr_sender)?;

    let receiving_chain_key = state.receiving_chain_key.ok_or(Error::SessionNotReady)?;
    let kdf_outputs = kdf(&receiving_chain_key, &[1u8], 2);
    state.receiving_chain_key = Some(kdf_outputs[0]);
    let message_key = kdf_outputs[1];

    state.receiving_chain_message_number += 1;

    let conversation_key = nip44::v2::ConversationKey::new(message_key);
    let ciphertext_bytes = base64::engine::general_purpose::STANDARD
        .decode(ciphertext)
        .map_err(|e| Error::Decryption(e.to_string()))?;

    let plaintext_bytes = nip44::v2::decrypt_to_bytes(&conversation_key, &ciphertext_bytes)?;
    String::from_utf8(plaintext_bytes).map_err(|e| Error::Decryption(e.to_string()))
}

fn ratchet_step(state: &mut SessionState, our_next_private_key: [u8; 32]) -> Result<()> {
    state.previous_sending_chain_message_count = state.sending_chain_message_number;
    state.sending_chain_message_number = 0;
    state.receiving_chain_message_number = 0;

    let our_next_sk = nostr::SecretKey::from_slice(&state.our_next_nostr_key.private_key)?;
    let their_next_pk = state
        .their_next_nostr_public_key
        .ok_or(Error::SessionNotReady)?;

    let conversation_key1 = nip44::v2::ConversationKey::derive(&our_next_sk, &their_next_pk);
    let kdf_outputs = kdf(&state.root_key, conversation_key1.as_bytes(), 2);

    state.receiving_chain_key = Some(kdf_outputs[1]);
    state.our_current_nostr_key = Some(state.our_next_nostr_key.clone());

    let our_next_keys = Keys::new(nostr::SecretKey::from_slice(&our_next_private_key)?);
    state.our_next_nostr_key = SerializableKeyPair {
        public_key: our_next_keys.public_key(),
        private_key: our_next_private_key,
    };

    let our_next_sk2 = nostr::SecretKey::from_slice(&our_next_private_key)?;
    let conversation_key2 = nip44::v2::ConversationKey::derive(&our_next_sk2, &their_next_pk);
    let kdf_outputs2 = kdf(&kdf_outputs[0], conversation_key2.as_bytes(), 2);

    state.root_key = kdf_outputs2[0];
    state.sending_chain_key = Some(kdf_outputs2[1]);

    Ok(())
}

fn try_skipped_message_keys(
    state: &mut SessionState,
    header: &Header,
    ciphertext: &str,
    nostr_sender: &PublicKey,
) -> Result<Option<String>> {
    if let Some(entry) = state.skipped_keys.get_mut(nostr_sender) {
        if let Some(message_key) = entry.message_keys.remove(&header.number) {
            let conversation_key = nip44::v2::ConversationKey::new(message_key);
            let ciphertext_bytes = base64::engine::general_purpose::STANDARD
                .decode(ciphertext)
                .map_err(|e| Error::Decryption(e.to_string()))?;

            let plaintext_bytes =
                nip44::v2::decrypt_to_bytes(&conversation_key, &ciphertext_bytes)?;
            let plaintext =
                String::from_utf8(plaintext_bytes).map_err(|e| Error::Decryption(e.to_string()))?;

            if entry.message_keys.is_empty() {
                state.skipped_keys.remove(nostr_sender);
            }

            return Ok(Some(plaintext));
        }
    }

    Ok(None)
}

fn decrypt_header(
    state: &SessionState,
    encrypted_header: &str,
    sender: &PublicKey,
) -> Result<(Header, bool)> {
    if let Some(current) = &state.our_current_nostr_key {
        let current_sk = nostr::SecretKey::from_slice(&current.private_key)?;

        if let Ok(decrypted) = nostr::nips::nip44::decrypt(&current_sk, sender, encrypted_header) {
            let header: Header = serde_json::from_str(&decrypted)
                .map_err(|e| Error::Serialization(e.to_string()))?;
            return Ok((header, false));
        }
    }

    let next_sk = nostr::SecretKey::from_slice(&state.our_next_nostr_key.private_key)?;
    let decrypted = nostr::nips::nip44::decrypt(&next_sk, sender, encrypted_header)?;
    let header: Header =
        serde_json::from_str(&decrypted).map_err(|e| Error::Serialization(e.to_string()))?;
    Ok((header, true))
}

fn normalize_inner_rumor_id(plaintext: &str) -> String {
    let mut value: serde_json::Value = match serde_json::from_str(plaintext) {
        Ok(value) => value,
        Err(_) => return plaintext.to_string(),
    };

    let obj = match value.as_object_mut() {
        Some(obj) => obj,
        None => return plaintext.to_string(),
    };

    let pubkey = match obj.get("pubkey").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return plaintext.to_string(),
    };
    let created_at = match obj.get("created_at").and_then(|v| v.as_u64()) {
        Some(n) => n,
        None => return plaintext.to_string(),
    };
    let kind = match obj.get("kind").and_then(|v| v.as_u64()) {
        Some(n) => n,
        None => return plaintext.to_string(),
    };
    let content = match obj.get("content").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return plaintext.to_string(),
    };
    let tags_value = match obj.get("tags").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return plaintext.to_string(),
    };

    let mut tags: Vec<Vec<String>> = Vec::with_capacity(tags_value.len());
    for tag in tags_value {
        let arr = match tag.as_array() {
            Some(arr) => arr,
            None => return plaintext.to_string(),
        };
        let mut out: Vec<String> = Vec::with_capacity(arr.len());
        for value in arr {
            let s = match value.as_str() {
                Some(s) => s,
                None => return plaintext.to_string(),
            };
            out.push(s.to_string());
        }
        tags.push(out);
    }

    let canonical = serde_json::json!([0, pubkey, created_at, kind, tags, content]);
    let canonical_json = match serde_json::to_string(&canonical) {
        Ok(json) => json,
        Err(_) => return plaintext.to_string(),
    };

    let computed = hex::encode(Sha256::digest(canonical_json.as_bytes()));
    obj.insert("id".to_string(), serde_json::Value::String(computed));

    serde_json::to_string(&value).unwrap_or_else(|_| plaintext.to_string())
}

fn prune_skipped_message_keys(map: &mut HashMap<u32, [u8; 32]>) {
    if map.len() <= MAX_SKIP {
        return;
    }

    let mut keys: Vec<u32> = map.keys().copied().collect();
    keys.sort_unstable();
    let to_remove = map.len().saturating_sub(MAX_SKIP);
    for key in keys.into_iter().take(to_remove) {
        map.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::Keys;

    #[test]
    fn skip_message_keys_prunes_to_max_skip() {
        let our_keys = Keys::generate();
        let mut state = SessionState {
            root_key: [0u8; 32],
            their_current_nostr_public_key: None,
            their_next_nostr_public_key: None,
            our_current_nostr_key: None,
            our_next_nostr_key: SerializableKeyPair {
                public_key: our_keys.public_key(),
                private_key: our_keys.secret_key().to_secret_bytes(),
            },
            receiving_chain_key: Some([7u8; 32]),
            sending_chain_key: None,
            sending_chain_message_number: 0,
            receiving_chain_message_number: 0,
            previous_sending_chain_message_count: 0,
            skipped_keys: HashMap::new(),
        };

        let sender = Keys::generate().public_key();

        skip_message_keys(&mut state, MAX_SKIP as u32, &sender).unwrap();
        skip_message_keys(&mut state, (MAX_SKIP * 2) as u32, &sender).unwrap();

        let entry = state.skipped_keys.get(&sender).unwrap();
        assert!(entry.message_keys.len() <= MAX_SKIP);
        assert!(!entry.message_keys.contains_key(&0));
        assert!(entry
            .message_keys
            .contains_key(&((MAX_SKIP * 2 - 1) as u32)));
    }
}
