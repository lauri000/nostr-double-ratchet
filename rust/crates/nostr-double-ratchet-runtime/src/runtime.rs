use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Deref;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use nostr::{Event, Filter, Keys, Kind, PublicKey, UnsignedEvent};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};

use crate::{
    nostr_codec, pairwise_codec, AppKeys, AuthorizedDevice, DevicePubkey, DeviceRoster,
    DomainError, Error, GroupCreateResult, GroupIncomingEvent, GroupManagerSnapshot,
    GroupPendingFanout, GroupPreparedPublish, GroupPreparedSend, GroupProtocol,
    GroupSenderKeyHandleResult, GroupSenderKeyMessage, InMemoryStorage, Invite,
    NostrGroupManager as GroupManager, OwnerPubkey, ProtocolContext, RelayGap, Result, SendOptions,
    SessionManager, SessionManagerSnapshot, SessionState, StorageAdapter, UnixSeconds,
    APP_KEYS_EVENT_KIND, INVITE_EVENT_KIND, INVITE_RESPONSE_KIND, MESSAGE_EVENT_KIND,
};

const RUNTIME_STATE_KEY: &str = "v2/runtime-state";
const PROTOCOL_SUBID: &str = "ndr-runtime-protocol";
const MESSAGE_SUBID: &str = "ndr-runtime-messages";

#[derive(Debug, Clone)]
pub enum RuntimeEffect {
    PersistRuntimeState {
        key: String,
        value: String,
    },
    Subscribe {
        subid: String,
        filters: Vec<Filter>,
    },
    Unsubscribe(String),
    PublishUnsigned(UnsignedEvent),
    PublishSigned(Event),
    PublishSignedForInnerEvent {
        event: Event,
        inner_event_id: Option<String>,
        target_device_id: Option<String>,
    },
    EmitDecrypted {
        sender: PublicKey,
        sender_device: Option<PublicKey>,
        conversation_owner: Option<PublicKey>,
        content: String,
        event_id: Option<String>,
    },
}

#[derive(Debug, Clone, Default)]
pub struct RuntimePublishResult {
    pub event_ids: Vec<String>,
    pub effects: Vec<RuntimeEffect>,
}

impl Deref for RuntimePublishResult {
    type Target = Vec<String>;

    fn deref(&self) -> &Self::Target {
        &self.event_ids
    }
}

impl PartialEq<Vec<String>> for RuntimePublishResult {
    fn eq(&self, other: &Vec<String>) -> bool {
        &self.event_ids == other
    }
}

impl PartialEq<RuntimePublishResult> for Vec<String> {
    fn eq(&self, other: &RuntimePublishResult) -> bool {
        self == &other.event_ids
    }
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeTextSendResult {
    pub inner_event_id: String,
    pub event_ids: Vec<String>,
    pub effects: Vec<RuntimeEffect>,
}

#[derive(Debug, Clone)]
pub struct RuntimeAcceptInviteResult {
    pub outcome: AcceptInviteResult,
    pub effects: Vec<RuntimeEffect>,
}

#[derive(Debug, Clone)]
pub struct RuntimeGroupCreateResult {
    pub outcome: GroupCreateResult,
    pub effects: Vec<RuntimeEffect>,
}

impl Deref for RuntimeGroupCreateResult {
    type Target = GroupCreateResult;

    fn deref(&self) -> &Self::Target {
        &self.outcome
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeGroupSnapshotResult {
    pub snapshot: crate::GroupSnapshot,
    pub effects: Vec<RuntimeEffect>,
}

impl Deref for RuntimeGroupSnapshotResult {
    type Target = crate::GroupSnapshot;

    fn deref(&self) -> &Self::Target {
        &self.snapshot
    }
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeGroupIncomingResult {
    pub events: Vec<GroupIncomingEvent>,
    pub effects: Vec<RuntimeEffect>,
}

impl Deref for RuntimeGroupIncomingResult {
    type Target = Vec<GroupIncomingEvent>;

    fn deref(&self) -> &Self::Target {
        &self.events
    }
}

impl IntoIterator for RuntimeGroupIncomingResult {
    type Item = GroupIncomingEvent;
    type IntoIter = std::vec::IntoIter<GroupIncomingEvent>;

    fn into_iter(self) -> Self::IntoIter {
        self.events.into_iter()
    }
}

#[derive(Debug, Clone)]
pub struct AcceptInviteResult {
    pub owner_pubkey: PublicKey,
    pub inviter_device_pubkey: PublicKey,
    pub device_id: String,
    pub created_new_session: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueuedMessageStage {
    Discovery,
    Device,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedMessageDiagnostic {
    pub stage: QueuedMessageStage,
    pub target_key: String,
    pub owner_pubkey: Option<PublicKey>,
    pub inner_event_id: Option<String>,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessagePushSessionStateSnapshot {
    pub state: SessionState,
    pub tracked_sender_pubkeys: Vec<PublicKey>,
    pub has_receiving_capability: bool,
}

#[derive(Debug, Clone, Default)]
pub struct GroupOuterSubscriptionPlan {
    pub authors: Vec<PublicKey>,
    pub added_authors: Vec<PublicKey>,
}

#[derive(Debug, Clone, Default)]
pub struct GroupPairwiseHandleOutcome {
    pub events: Vec<GroupIncomingEvent>,
    pub consumed: bool,
    pub effects: Vec<RuntimeEffect>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredRuntimeState {
    core: SessionManagerSnapshot,
    #[serde(default)]
    group_manager: Option<GroupManagerSnapshot>,
    #[serde(default)]
    pending_prepared_publishes: Vec<PendingPreparedPublish>,
    #[serde(default)]
    pending_group_sender_key_messages: Vec<nostr_codec::ParsedGroupSenderKeyMessageEvent>,
    #[serde(default)]
    pending_group_pairwise_payloads: Vec<PendingGroupPairwisePayload>,
    #[serde(default)]
    pending_group_fanouts: Vec<PendingGroupFanout>,
    #[serde(default)]
    pending_decrypted_deliveries: Vec<PendingDecryptedDelivery>,
    pending_outbound: Vec<PendingOutbound>,
    processed_invite_response_ids: Vec<String>,
    latest_app_keys_created_at: HashMap<String, u64>,
    #[serde(default)]
    tracked_owner_pubkeys: Vec<PublicKey>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingOutbound {
    recipient_owner: OwnerPubkey,
    remote_payload: Vec<u8>,
    local_sibling_payload: Option<Vec<u8>>,
    inner_event_id: Option<String>,
    created_at_ms: u64,
    reason: QueuedMessageStage,
}

fn same_pending_outbound(
    pending: &PendingOutbound,
    recipient_owner: OwnerPubkey,
    inner_event_id: Option<&str>,
    remote_payload: &[u8],
    local_sibling_payload: Option<&[u8]>,
) -> bool {
    if pending.recipient_owner != recipient_owner {
        return false;
    }
    match (pending.inner_event_id.as_deref(), inner_event_id) {
        (Some(existing), Some(next)) => existing == next,
        (None, None) => {
            pending.remote_payload == remote_payload
                && pending.local_sibling_payload.as_deref() == local_sibling_payload
        }
        _ => false,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingPreparedPublish {
    event: Event,
    inner_event_id: Option<String>,
    target_device_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PendingDecryptedDelivery {
    sender_owner: OwnerPubkey,
    sender_device: Option<DevicePubkey>,
    conversation_owner: Option<OwnerPubkey>,
    content: String,
    event_id: Option<String>,
    created_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PendingGroupPairwisePayload {
    sender_owner: OwnerPubkey,
    sender_device: Option<DevicePubkey>,
    payload: Vec<u8>,
    created_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PendingGroupFanout {
    group_id: String,
    fanout: GroupPendingFanout,
    inner_event_id: Option<String>,
    created_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalSiblingPayload {
    protocol: String,
    version: u32,
    conversation_owner: String,
    payload: String,
}

struct RuntimeState {
    core: SessionManager,
    group_manager: GroupManager,
    pending_prepared_publishes: Vec<PendingPreparedPublish>,
    pending_group_sender_key_messages: Vec<nostr_codec::ParsedGroupSenderKeyMessageEvent>,
    pending_group_pairwise_payloads: Vec<PendingGroupPairwisePayload>,
    pending_group_fanouts: Vec<PendingGroupFanout>,
    pending_decrypted_deliveries: Vec<PendingDecryptedDelivery>,
    pending_outbound: Vec<PendingOutbound>,
    processed_invite_response_ids: HashSet<String>,
    latest_app_keys_created_at: HashMap<PublicKey, u64>,
    tracked_owner_pubkeys: BTreeSet<PublicKey>,
}

struct DirectMessageSubscription {
    authors: Vec<PublicKey>,
}

pub struct NdrRuntime {
    state: Mutex<RuntimeState>,
    storage: Arc<dyn StorageAdapter>,
    our_public_key: PublicKey,
    our_identity_key: [u8; 32],
    device_id: String,
    owner_public_key: PublicKey,
    direct_message_subscription: Mutex<Option<DirectMessageSubscription>>,
}

impl NdrRuntime {
    pub fn new(
        our_public_key: PublicKey,
        our_identity_key: [u8; 32],
        device_id: String,
        owner_public_key: PublicKey,
        storage: Option<Arc<dyn StorageAdapter>>,
        invite: Option<Invite>,
    ) -> Self {
        Self::new_with_group_storage(
            our_public_key,
            our_identity_key,
            device_id,
            owner_public_key,
            storage.clone(),
            storage,
            invite,
        )
    }

    pub fn new_with_group_storage(
        our_public_key: PublicKey,
        our_identity_key: [u8; 32],
        device_id: String,
        owner_public_key: PublicKey,
        session_storage: Option<Arc<dyn StorageAdapter>>,
        group_storage: Option<Arc<dyn StorageAdapter>>,
        invite: Option<Invite>,
    ) -> Self {
        let storage = session_storage.unwrap_or_else(|| Arc::new(InMemoryStorage::new()));
        let _group_storage = group_storage.unwrap_or_else(|| Arc::clone(&storage));
        let mut state = RuntimeState::load(storage.as_ref(), owner_public_key, our_identity_key)
            .unwrap_or_else(|| RuntimeState {
                core: SessionManager::new(owner(owner_public_key), our_identity_key),
                group_manager: GroupManager::new(owner(owner_public_key)),
                pending_prepared_publishes: Vec::new(),
                pending_group_sender_key_messages: Vec::new(),
                pending_group_pairwise_payloads: Vec::new(),
                pending_group_fanouts: Vec::new(),
                pending_decrypted_deliveries: Vec::new(),
                pending_outbound: Vec::new(),
                processed_invite_response_ids: HashSet::new(),
                latest_app_keys_created_at: HashMap::new(),
                tracked_owner_pubkeys: BTreeSet::new(),
            });

        if let Some(invite) = invite {
            state.core.replace_local_invite(invite);
        }

        Self {
            state: Mutex::new(state),
            storage,
            our_public_key,
            our_identity_key,
            device_id,
            owner_public_key,
            direct_message_subscription: Mutex::new(None),
        }
    }

    pub fn init(&self) -> Result<Vec<RuntimeEffect>> {
        let now = now();
        let mut effects = Vec::new();
        {
            let mut rng = OsRng;
            let mut ctx = ProtocolContext::new(now, &mut rng);
            let mut state = self.state.lock().unwrap();
            let has_local_roster = state.core.snapshot().users.into_iter().any(|user| {
                user.owner_pubkey == owner(self.owner_public_key) && user.roster.is_some()
            });
            let invite = state.core.ensure_local_invite(&mut ctx)?.clone();
            if !has_local_roster {
                let created_at = invite.created_at;
                let local_roster = DeviceRoster::new(
                    created_at,
                    vec![AuthorizedDevice::new(
                        device(self.our_public_key),
                        created_at,
                    )],
                );
                state.core.apply_local_roster(local_roster);
            }
            drop(state);
            effects.push(self.publish_local_invite_effect(&invite)?);
        }
        self.persist_state_effect(&mut effects)?;
        effects.extend(self.replay_pending_decrypted_deliveries());
        effects.extend(self.replay_pending_prepared_publishes());
        effects.extend(self.refresh_protocol_subscriptions()?);
        effects.extend(self.sync_direct_message_subscriptions()?);
        Ok(effects)
    }

    pub fn setup_user(&self, user_pubkey: PublicKey) -> Result<Vec<RuntimeEffect>> {
        self.observe_owner_if_absent(user_pubkey);
        let mut effects = self.refresh_protocol_subscriptions()?;
        effects.extend(self.sync_direct_message_subscriptions()?);
        Ok(effects)
    }

    pub fn reload_from_storage(&self) -> Result<Vec<RuntimeEffect>> {
        if let Some(loaded) = RuntimeState::load(
            self.storage.as_ref(),
            self.owner_public_key,
            self.our_identity_key,
        ) {
            *self.state.lock().unwrap() = loaded;
        }
        let mut effects = self.replay_pending_decrypted_deliveries();
        effects.extend(self.replay_pending_prepared_publishes());
        effects.extend(self.refresh_protocol_subscriptions()?);
        effects.extend(self.sync_direct_message_subscriptions()?);
        Ok(effects)
    }

    pub fn persist_runtime_state(&self, key: &str, value: String) -> Result<()> {
        self.storage.put(key, value)
    }

    pub fn prepared_publish_effects(&self) -> Vec<RuntimeEffect> {
        self.replay_pending_prepared_publishes()
    }

    pub fn ack_prepared_publish(&self, event_id: &str) -> Result<Vec<RuntimeEffect>> {
        let mut state = self.state.lock().unwrap();
        let before = state.pending_prepared_publishes.len();
        state
            .pending_prepared_publishes
            .retain(|pending| pending.event.id.to_string() != event_id);
        let changed = state.pending_prepared_publishes.len() != before;
        drop(state);
        let mut effects = Vec::new();
        if changed {
            self.persist_state_effect(&mut effects)?;
        }
        Ok(effects)
    }

    pub fn ack_decrypted_delivery(&self, event_id: &str) -> Result<Vec<RuntimeEffect>> {
        let mut state = self.state.lock().unwrap();
        let before = state.pending_decrypted_deliveries.len();
        state
            .pending_decrypted_deliveries
            .retain(|pending| pending.event_id.as_deref() != Some(event_id));
        let changed = state.pending_decrypted_deliveries.len() != before;
        drop(state);
        let mut effects = Vec::new();
        if changed {
            self.persist_state_effect(&mut effects)?;
        }
        Ok(effects)
    }

    pub fn delete_chat(&self, user_pubkey: PublicKey) -> Result<Vec<RuntimeEffect>> {
        self.state
            .lock()
            .unwrap()
            .core
            .delete_user(owner(user_pubkey));
        let mut effects = Vec::new();
        self.persist_state_effect(&mut effects)?;
        effects.extend(self.refresh_protocol_subscriptions()?);
        effects.extend(self.sync_direct_message_subscriptions()?);
        Ok(effects)
    }

    pub fn cleanup_discovery_queue(&self, _max_age_ms: u64) -> Result<usize> {
        Ok(0)
    }

    pub fn accept_invite(
        &self,
        invite: &Invite,
        owner_pubkey_hint: Option<PublicKey>,
    ) -> Result<RuntimeAcceptInviteResult> {
        let invite_owner = owner_pubkey_hint
            .or_else(|| invite.inviter_owner_pubkey.map(public_owner))
            .unwrap_or_else(|| public_device(invite.inviter_device_pubkey));
        let mut invite = invite.clone();
        invite.inviter_owner_pubkey = Some(owner(invite_owner));
        {
            let mut state = self.state.lock().unwrap();
            state
                .core
                .observe_device_invite(owner(invite_owner), invite.clone())?;
            state.core.observe_peer_roster(
                owner(invite_owner),
                DeviceRoster::new(
                    now(),
                    vec![AuthorizedDevice::new(
                        invite.inviter_device_pubkey,
                        invite.created_at,
                    )],
                ),
            );
        }
        let mut effects = Vec::new();
        self.persist_state_effect(&mut effects)?;
        effects.extend(self.refresh_protocol_subscriptions()?);

        if invite.purpose.as_deref() == Some("link") {
            effects.extend(self.send_link_bootstrap(invite_owner)?.effects);
        }

        Ok(RuntimeAcceptInviteResult {
            outcome: AcceptInviteResult {
                owner_pubkey: invite_owner,
                inviter_device_pubkey: public_device(invite.inviter_device_pubkey),
                device_id: public_device(invite.inviter_device_pubkey).to_hex(),
                created_new_session: false,
            },
            effects,
        })
    }

    pub fn send_text(
        &self,
        recipient: PublicKey,
        text: String,
        options: Option<SendOptions>,
    ) -> Result<RuntimePublishResult> {
        if text.trim().is_empty() {
            return Ok(RuntimePublishResult::default());
        }
        let result = self.send_text_with_inner_id(recipient, text, options)?;
        Ok(RuntimePublishResult {
            event_ids: result.event_ids,
            effects: result.effects,
        })
    }

    pub fn send_text_with_inner_id(
        &self,
        recipient: PublicKey,
        text: String,
        options: Option<SendOptions>,
    ) -> Result<RuntimeTextSendResult> {
        if text.trim().is_empty() {
            return Ok(RuntimeTextSendResult::default());
        }
        let now = now();
        let now_ms = current_unix_millis();
        let expiration = options
            .as_ref()
            .map(|options| crate::utils::resolve_expiration_seconds(options, now.get()))
            .transpose()?
            .flatten();
        let encode_options = expiration
            .map(|expiration| {
                pairwise_codec::EncodeOptions::new(now.get(), now_ms).with_expiration(expiration)
            })
            .unwrap_or_else(|| pairwise_codec::EncodeOptions::new(now.get(), now_ms));
        let event = pairwise_codec::message_event(self.owner_public_key, text, encode_options)
            .map_err(|e| Error::InvalidEvent(e.to_string()))?;
        let inner_id = event
            .id
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        let result = self.send_event(recipient, event)?;
        Ok(RuntimeTextSendResult {
            inner_event_id: inner_id,
            event_ids: result.event_ids,
            effects: result.effects,
        })
    }

    pub fn send_event(
        &self,
        recipient: PublicKey,
        mut event: UnsignedEvent,
    ) -> Result<RuntimePublishResult> {
        event.ensure_id();
        let inner_id = event.id.as_ref().map(ToString::to_string);
        let remote_payload = serde_json::to_vec(&event)?;
        let local_payload = local_sibling_payload(recipient, &remote_payload)?;
        self.prepare_and_publish(
            owner(recipient),
            remote_payload,
            Some(local_payload),
            inner_id,
        )
    }

    pub fn send_reaction(
        &self,
        recipient: PublicKey,
        message_id: String,
        emoji: String,
        options: Option<SendOptions>,
    ) -> Result<RuntimePublishResult> {
        let now = now();
        let now_ms = current_unix_millis();
        let expiration = options
            .as_ref()
            .map(|options| crate::utils::resolve_expiration_seconds(options, now.get()))
            .transpose()?
            .flatten();
        let encode_options = expiration
            .map(|expiration| {
                pairwise_codec::EncodeOptions::new(now.get(), now_ms).with_expiration(expiration)
            })
            .unwrap_or_else(|| pairwise_codec::EncodeOptions::new(now.get(), now_ms));
        let event = pairwise_codec::reaction_event(
            self.owner_public_key,
            message_id,
            emoji,
            encode_options,
        )
        .map_err(|e| Error::InvalidEvent(e.to_string()))?;
        self.send_event(recipient, event)
    }

    pub fn send_receipt(
        &self,
        recipient: PublicKey,
        receipt_type: &str,
        message_ids: Vec<String>,
        options: Option<SendOptions>,
    ) -> Result<RuntimePublishResult> {
        if message_ids.is_empty() {
            return Ok(RuntimePublishResult::default());
        }
        let now = now();
        let now_ms = current_unix_millis();
        let receipt_type = pairwise_codec::ReceiptType::try_from(receipt_type)
            .map_err(|e| Error::InvalidEvent(e.to_string()))?;
        let expiration = options
            .as_ref()
            .map(|options| crate::utils::resolve_expiration_seconds(options, now.get()))
            .transpose()?
            .flatten();
        let encode_options = expiration
            .map(|expiration| {
                pairwise_codec::EncodeOptions::new(now.get(), now_ms).with_expiration(expiration)
            })
            .unwrap_or_else(|| pairwise_codec::EncodeOptions::new(now.get(), now_ms));
        let event = pairwise_codec::receipt_event(
            self.owner_public_key,
            receipt_type,
            message_ids,
            encode_options,
        )
        .map_err(|e| Error::InvalidEvent(e.to_string()))?;
        self.send_event(recipient, event)
    }

    pub fn send_typing(
        &self,
        recipient: PublicKey,
        options: Option<SendOptions>,
    ) -> Result<RuntimePublishResult> {
        let now = now();
        let now_ms = current_unix_millis();
        let expiration = options
            .as_ref()
            .map(|options| crate::utils::resolve_expiration_seconds(options, now.get()))
            .transpose()?
            .flatten();
        let encode_options = expiration
            .map(|expiration| {
                pairwise_codec::EncodeOptions::new(now.get(), now_ms).with_expiration(expiration)
            })
            .unwrap_or_else(|| pairwise_codec::EncodeOptions::new(now.get(), now_ms));
        let event = pairwise_codec::typing_event(self.owner_public_key, encode_options)
            .map_err(|e| Error::InvalidEvent(e.to_string()))?;
        self.send_event(recipient, event)
    }

    pub fn send_chat_settings(
        &self,
        recipient: PublicKey,
        ttl_seconds: u64,
    ) -> Result<RuntimePublishResult> {
        let ttl = if ttl_seconds == 0 {
            pairwise_codec::ChatSettingsTtl::DisablePeerExpiration
        } else {
            pairwise_codec::ChatSettingsTtl::Seconds(ttl_seconds)
        };
        let event = pairwise_codec::chat_settings_event(
            self.owner_public_key,
            ttl,
            now().get(),
            current_unix_millis(),
        )
        .map_err(|e| Error::InvalidEvent(e.to_string()))?;
        self.send_event(recipient, event)
    }

    pub fn import_session_state(
        &self,
        peer_pubkey: PublicKey,
        device_id: Option<String>,
        state: SessionState,
    ) -> Result<Vec<RuntimeEffect>> {
        let device_pubkey = device_id
            .as_deref()
            .and_then(|value| PublicKey::parse(value).ok())
            .map(device)
            .unwrap_or_else(|| device(peer_pubkey));
        self.state.lock().unwrap().core.import_session_state(
            owner(peer_pubkey),
            device_pubkey,
            state,
            now(),
        );
        let mut effects = Vec::new();
        self.persist_state_effect(&mut effects)?;
        effects.extend(self.sync_direct_message_subscriptions()?);
        Ok(effects)
    }

    pub fn export_active_sessions(&self) -> Vec<(PublicKey, String, SessionState)> {
        let snapshot = self.state.lock().unwrap().core.snapshot();
        snapshot
            .users
            .into_iter()
            .flat_map(|user| {
                user.devices.into_iter().filter_map(move |device_record| {
                    device_record.active_session.map(|state| {
                        (
                            public_owner(user.owner_pubkey),
                            public_device(device_record.device_pubkey).to_hex(),
                            state,
                        )
                    })
                })
            })
            .collect()
    }

    pub fn export_active_session_state(
        &self,
        peer_pubkey: PublicKey,
    ) -> Result<Option<SessionState>> {
        Ok(self
            .export_active_sessions()
            .into_iter()
            .find(|(owner_pubkey, _, _)| *owner_pubkey == peer_pubkey)
            .map(|(_, _, state)| state))
    }

    pub fn get_stored_user_record_json(&self, user_pubkey: PublicKey) -> Result<Option<String>> {
        let snapshot = self.state.lock().unwrap().core.snapshot();
        let user = snapshot
            .users
            .into_iter()
            .find(|user| user.owner_pubkey == owner(user_pubkey));
        user.map(|user| serde_json::to_string(&user).map_err(Into::into))
            .transpose()
    }

    pub fn get_all_message_push_author_pubkeys(&self) -> Vec<PublicKey> {
        let mut authors = BTreeSet::new();
        for (_, _, state) in self.export_all_session_states() {
            collect_expected_senders(&state, &mut authors);
        }
        authors.into_iter().collect()
    }

    pub fn get_message_push_author_pubkeys(&self, peer_owner_pubkey: PublicKey) -> Vec<PublicKey> {
        let mut authors = BTreeSet::new();
        for (owner_pubkey, _, state) in self.export_all_session_states() {
            if owner_pubkey == peer_owner_pubkey {
                collect_expected_senders(&state, &mut authors);
            }
        }
        authors.into_iter().collect()
    }

    pub fn get_message_push_session_states(
        &self,
        peer_owner_pubkey: PublicKey,
    ) -> Vec<MessagePushSessionStateSnapshot> {
        self.export_all_session_states()
            .into_iter()
            .filter(|(owner_pubkey, _, _)| *owner_pubkey == peer_owner_pubkey)
            .map(|(_, _, state)| {
                let mut tracked = BTreeSet::new();
                collect_expected_senders(&state, &mut tracked);
                MessagePushSessionStateSnapshot {
                    has_receiving_capability: state.receiving_chain_key.is_some()
                        || state.their_current_nostr_public_key.is_some(),
                    state,
                    tracked_sender_pubkeys: tracked.into_iter().collect(),
                }
            })
            .collect()
    }

    pub fn known_peer_owner_pubkeys(&self) -> Vec<PublicKey> {
        let mut owners = Vec::new();
        for user in self.state.lock().unwrap().core.snapshot().users {
            owners.push(public_owner(user.owner_pubkey));
            owners.extend(
                user.devices
                    .into_iter()
                    .filter_map(|device| device.claimed_owner_pubkey)
                    .map(public_owner),
            );
        }
        owners.retain(|pubkey| *pubkey != self.owner_public_key);
        owners.sort_by_key(|pubkey| pubkey.to_hex());
        owners.dedup();
        owners
    }

    pub fn known_device_identity_pubkeys_for_owner(
        &self,
        owner_pubkey: PublicKey,
    ) -> Vec<PublicKey> {
        self.state
            .lock()
            .unwrap()
            .core
            .snapshot()
            .users
            .into_iter()
            .find(|user| user.owner_pubkey == owner(owner_pubkey))
            .map(|user| {
                user.devices
                    .into_iter()
                    .filter(|device| device.authorized && !device.is_stale)
                    .map(|device| public_device(device.device_pubkey))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn known_device_identity_pubkeys_for_owners(
        &self,
        owners: impl IntoIterator<Item = PublicKey>,
    ) -> Vec<PublicKey> {
        let mut devices = BTreeSet::new();
        for owner_pubkey in owners {
            devices.extend(self.known_device_identity_pubkeys_for_owner(owner_pubkey));
        }
        devices.into_iter().collect()
    }

    pub fn get_total_sessions(&self) -> usize {
        self.export_all_session_states().len()
    }

    pub fn ingest_app_keys_snapshot(
        &self,
        owner_pubkey: PublicKey,
        app_keys: AppKeys,
        created_at: u64,
    ) -> Result<Vec<RuntimeEffect>> {
        let mut state = self.state.lock().unwrap();
        let latest = state
            .latest_app_keys_created_at
            .get(&owner_pubkey)
            .copied()
            .unwrap_or(0);
        if created_at < latest {
            return Ok(Vec::new());
        }
        state
            .latest_app_keys_created_at
            .insert(owner_pubkey, created_at);
        let roster = DeviceRoster::new(
            UnixSeconds(created_at),
            app_keys
                .get_all_devices()
                .into_iter()
                .map(|entry| {
                    AuthorizedDevice::new(
                        device(entry.identity_pubkey),
                        UnixSeconds(entry.created_at),
                    )
                })
                .collect(),
        );
        if owner_pubkey == self.owner_public_key {
            if should_replace_provisional_local_roster(
                &state.core.snapshot(),
                self.owner_public_key,
                self.our_public_key,
                &roster,
            ) {
                state.core.replace_local_roster(roster);
            } else {
                state.core.apply_local_roster(roster);
            }
        } else {
            state.core.observe_peer_roster(owner(owner_pubkey), roster);
        }
        drop(state);
        let mut effects = Vec::new();
        self.persist_state_effect(&mut effects)?;
        effects.extend(self.retry_pending_outbound()?);
        effects.extend(self.retry_pending_group_fanouts()?);
        effects.extend(self.refresh_protocol_subscriptions()?);
        effects.extend(self.sync_direct_message_subscriptions()?);
        Ok(effects)
    }

    pub fn process_received_event(&self, event: Event) -> Result<Vec<RuntimeEffect>> {
        let kind = event.kind.as_u16() as u32;
        let event_id = event.id.to_string();
        if kind == MESSAGE_EVENT_KIND {
            if let Some(effect) = self.pending_decrypted_delivery_effect_by_event_id(&event_id) {
                return Ok(vec![effect]);
            }
        }
        let mut effects = match kind {
            APP_KEYS_EVENT_KIND if crate::is_app_keys_event(&event) => AppKeys::from_event(&event)
                .map_err(Into::into)
                .and_then(|app_keys| {
                    self.ingest_app_keys_snapshot(
                        event.pubkey,
                        app_keys,
                        event.created_at.as_secs(),
                    )
                }),
            INVITE_EVENT_KIND => self.process_invite_event(&event),
            INVITE_RESPONSE_KIND => self.process_invite_response_event(&event),
            MESSAGE_EVENT_KIND => self.process_message_event(&event, Some(event_id)),
            _ => Ok(Vec::new()),
        }?;
        effects.extend(self.sync_direct_message_subscriptions()?);
        Ok(effects)
    }

    pub fn sync_direct_message_subscriptions(&self) -> Result<Vec<RuntimeEffect>> {
        let next_authors = self.get_all_message_push_author_pubkeys();
        let mut current = self.direct_message_subscription.lock().unwrap();
        if current
            .as_ref()
            .is_some_and(|subscription| subscription.authors == next_authors)
        {
            return Ok(Vec::new());
        }
        let mut effects = Vec::new();
        if current.is_some() {
            effects.push(RuntimeEffect::Unsubscribe(MESSAGE_SUBID.to_string()));
        }
        if next_authors.is_empty() {
            *current = None;
            return Ok(effects);
        }
        let filter = Filter::new()
            .kind(Kind::from(MESSAGE_EVENT_KIND as u16))
            .authors(next_authors.clone());
        effects.push(RuntimeEffect::Subscribe {
            subid: MESSAGE_SUBID.to_string(),
            filters: vec![filter],
        });
        *current = Some(DirectMessageSubscription {
            authors: next_authors,
        });
        Ok(effects)
    }

    pub fn pending_invite_response_owner_pubkeys(&self) -> Vec<PublicKey> {
        Vec::new()
    }

    pub fn current_device_invite_response_pubkey(&self) -> Option<PublicKey> {
        self.state
            .lock()
            .unwrap()
            .core
            .snapshot()
            .local_invite
            .map(|invite| public_device(invite.inviter_ephemeral_public_key))
    }

    pub fn local_invite(&self) -> Option<Invite> {
        self.state.lock().unwrap().core.snapshot().local_invite
    }

    pub fn get_owner_pubkey(&self) -> PublicKey {
        self.owner_public_key
    }

    pub fn get_our_pubkey(&self) -> PublicKey {
        self.our_public_key
    }

    pub fn get_device_id(&self) -> &str {
        &self.device_id
    }

    pub fn set_auto_adopt_chat_settings(&self, _enabled: bool) {}

    pub fn with_group_context<R>(
        &self,
        f: impl FnOnce(&mut SessionManager, &mut GroupManager) -> R,
    ) -> R {
        let mut state = self.state.lock().unwrap();
        let (core, group_manager) = state.core_and_group_mut();
        f(core, group_manager)
    }

    pub fn sync_groups(&self, _groups: Vec<crate::GroupSnapshot>) -> Result<()> {
        // Experimental group state is owned by GroupManagerSnapshot in runtime state.
        Ok(())
    }

    pub fn group_known_sender_event_pubkeys(&self) -> Vec<PublicKey> {
        self.state
            .lock()
            .unwrap()
            .group_manager
            .known_sender_event_pubkeys()
            .into_iter()
            .map(public_device)
            .collect()
    }

    pub fn group_id_for_sender_event_pubkey(
        &self,
        sender_event_pubkey: PublicKey,
    ) -> Option<String> {
        self.state
            .lock()
            .unwrap()
            .group_manager
            .group_id_for_sender_event_pubkey(DevicePubkey::from_bytes(
                sender_event_pubkey.to_bytes(),
            ))
    }

    pub fn group_outer_subscription_plan(&self) -> GroupOuterSubscriptionPlan {
        let authors = self.group_known_sender_event_pubkeys();
        GroupOuterSubscriptionPlan {
            added_authors: authors.clone(),
            authors,
        }
    }

    pub fn group_snapshots(&self) -> Vec<crate::GroupSnapshot> {
        self.state.lock().unwrap().group_manager.groups()
    }

    pub fn create_group(
        &self,
        name: String,
        member_owners: Vec<PublicKey>,
    ) -> Result<RuntimeGroupCreateResult> {
        let now = now();
        let mut rng = OsRng;
        let mut ctx = ProtocolContext::new(now, &mut rng);
        let result = {
            let mut state = self.state.lock().unwrap();
            let (core, group_manager) = state.core_and_group_mut();
            group_manager.create_group_with_protocol(
                core,
                &mut ctx,
                name,
                member_owners.into_iter().map(owner).collect(),
                GroupProtocol::sender_key_v1(),
            )?
        };
        let mut effects = self
            .publish_group_prepared_send(&result.prepared, None)?
            .effects;
        self.persist_state_effect(&mut effects)?;
        effects.extend(self.refresh_protocol_subscriptions()?);
        effects.extend(self.sync_direct_message_subscriptions()?);
        Ok(RuntimeGroupCreateResult {
            outcome: result,
            effects,
        })
    }

    pub fn update_group_name(
        &self,
        group_id: &str,
        name: String,
    ) -> Result<RuntimeGroupSnapshotResult> {
        let now = now();
        let mut rng = OsRng;
        let mut ctx = ProtocolContext::new(now, &mut rng);
        let (snapshot, prepared) = {
            let mut state = self.state.lock().unwrap();
            let (core, group_manager) = state.core_and_group_mut();
            let prepared = group_manager.update_name(core, &mut ctx, group_id, name)?;
            let snapshot = group_manager
                .group(group_id)
                .ok_or_else(|| crate::DomainError::InvalidState("unknown group".to_string()))?;
            (snapshot, prepared)
        };
        let mut effects = self.publish_group_prepared_send(&prepared, None)?.effects;
        self.persist_state_effect(&mut effects)?;
        effects.extend(self.refresh_protocol_subscriptions()?);
        Ok(RuntimeGroupSnapshotResult { snapshot, effects })
    }

    pub fn add_group_members(
        &self,
        group_id: &str,
        members: Vec<PublicKey>,
    ) -> Result<RuntimeGroupSnapshotResult> {
        let now = now();
        let mut rng = OsRng;
        let mut ctx = ProtocolContext::new(now, &mut rng);
        let (snapshot, prepared) = {
            let mut state = self.state.lock().unwrap();
            let (core, group_manager) = state.core_and_group_mut();
            let prepared = group_manager.add_members(
                core,
                &mut ctx,
                group_id,
                members.into_iter().map(owner).collect(),
            )?;
            let snapshot = group_manager
                .group(group_id)
                .ok_or_else(|| crate::DomainError::InvalidState("unknown group".to_string()))?;
            (snapshot, prepared)
        };
        let mut effects = self.publish_group_prepared_send(&prepared, None)?.effects;
        self.persist_state_effect(&mut effects)?;
        effects.extend(self.refresh_protocol_subscriptions()?);
        effects.extend(self.sync_direct_message_subscriptions()?);
        Ok(RuntimeGroupSnapshotResult { snapshot, effects })
    }

    pub fn remove_group_member(
        &self,
        group_id: &str,
        member: PublicKey,
    ) -> Result<RuntimeGroupSnapshotResult> {
        let now = now();
        let mut rng = OsRng;
        let mut ctx = ProtocolContext::new(now, &mut rng);
        let (snapshot, prepared) = {
            let mut state = self.state.lock().unwrap();
            let (core, group_manager) = state.core_and_group_mut();
            let prepared =
                group_manager.remove_members(core, &mut ctx, group_id, vec![owner(member)])?;
            let snapshot = group_manager
                .group(group_id)
                .ok_or_else(|| crate::DomainError::InvalidState("unknown group".to_string()))?;
            (snapshot, prepared)
        };
        let mut effects = self.publish_group_prepared_send(&prepared, None)?.effects;
        self.persist_state_effect(&mut effects)?;
        effects.extend(self.refresh_protocol_subscriptions()?);
        Ok(RuntimeGroupSnapshotResult { snapshot, effects })
    }

    pub fn set_group_admin(
        &self,
        group_id: &str,
        member: PublicKey,
        is_admin: bool,
    ) -> Result<RuntimeGroupSnapshotResult> {
        let now = now();
        let mut rng = OsRng;
        let mut ctx = ProtocolContext::new(now, &mut rng);
        let (snapshot, prepared) = {
            let mut state = self.state.lock().unwrap();
            let (core, group_manager) = state.core_and_group_mut();
            let prepared = if is_admin {
                group_manager.add_admins(core, &mut ctx, group_id, vec![owner(member)])?
            } else {
                group_manager.remove_admins(core, &mut ctx, group_id, vec![owner(member)])?
            };
            let snapshot = group_manager
                .group(group_id)
                .ok_or_else(|| crate::DomainError::InvalidState("unknown group".to_string()))?;
            (snapshot, prepared)
        };
        let mut effects = self.publish_group_prepared_send(&prepared, None)?.effects;
        self.persist_state_effect(&mut effects)?;
        effects.extend(self.refresh_protocol_subscriptions()?);
        Ok(RuntimeGroupSnapshotResult { snapshot, effects })
    }

    pub fn send_group_message(
        &self,
        group_id: &str,
        payload: Vec<u8>,
        inner_event_id: Option<String>,
    ) -> Result<RuntimePublishResult> {
        let now = now();
        let mut rng = OsRng;
        let mut ctx = ProtocolContext::new(now, &mut rng);
        let prepared = {
            let mut state = self.state.lock().unwrap();
            let (core, group_manager) = state.core_and_group_mut();
            group_manager.send_message(core, &mut ctx, group_id, payload)?
        };
        let mut result = self.publish_group_prepared_send(&prepared, inner_event_id)?;
        self.persist_state_effect(&mut result.effects)?;
        result
            .effects
            .extend(self.refresh_protocol_subscriptions()?);
        result
            .effects
            .extend(self.sync_direct_message_subscriptions()?);
        Ok(result)
    }

    pub fn group_handle_incoming_payload(
        &self,
        payload: &[u8],
        from_owner_pubkey: PublicKey,
        from_sender_device_pubkey: Option<PublicKey>,
    ) -> Vec<GroupIncomingEvent> {
        self.group_handle_incoming_payload_outcome(
            payload,
            from_owner_pubkey,
            from_sender_device_pubkey,
        )
        .events
    }

    pub fn group_handle_incoming_payload_outcome(
        &self,
        payload: &[u8],
        from_owner_pubkey: PublicKey,
        from_sender_device_pubkey: Option<PublicKey>,
    ) -> GroupPairwiseHandleOutcome {
        let is_group_payload = self
            .state
            .lock()
            .unwrap()
            .group_manager
            .is_pairwise_payload(payload);
        let sender_owner = owner(from_owner_pubkey);
        let sender_device = from_sender_device_pubkey.map(device);
        let mut events = Vec::new();
        let mut persist = false;
        {
            let mut state = self.state.lock().unwrap();
            let result = match sender_device {
                Some(device_pubkey) => state.group_manager.handle_pairwise_payload(
                    sender_owner,
                    device_pubkey,
                    payload,
                ),
                None => state.group_manager.handle_incoming(sender_owner, payload),
            };
            match result {
                Ok(Some(event)) => {
                    events.push(event);
                    persist = true;
                }
                Ok(None) => {}
                Err(error) => {
                    if is_group_payload && should_queue_group_pairwise_payload(&error) {
                        let pending = PendingGroupPairwisePayload {
                            sender_owner,
                            sender_device,
                            payload: payload.to_vec(),
                            created_at_ms: current_unix_millis(),
                        };
                        if !state.pending_group_pairwise_payloads.contains(&pending) {
                            state.pending_group_pairwise_payloads.push(pending);
                            persist = true;
                        }
                    }
                }
            }
        }
        if !events.is_empty() {
            events.extend(self.retry_pending_group_pairwise_payloads());
            events.extend(self.retry_pending_group_sender_key_messages());
        }
        let mut effects = Vec::new();
        if persist || !events.is_empty() {
            let _ = self.persist_state_effect(&mut effects);
        }
        if !events.is_empty() {
            if let Ok(subscription_effects) = self.refresh_protocol_subscriptions() {
                effects.extend(subscription_effects);
            }
        }
        GroupPairwiseHandleOutcome {
            events,
            consumed: is_group_payload,
            effects,
        }
    }

    pub fn group_handle_outer_event(&self, outer: &Event) -> RuntimeGroupIncomingResult {
        let Ok(parsed) = nostr_codec::parse_group_sender_key_message_event(outer) else {
            return RuntimeGroupIncomingResult::default();
        };
        let Some(message) = self.group_sender_key_message_from_parsed(&parsed) else {
            let mut state = self.state.lock().unwrap();
            if !state.pending_group_sender_key_messages.contains(&parsed) {
                state.pending_group_sender_key_messages.push(parsed);
            }
            drop(state);
            let mut effects = Vec::new();
            let _ = self.persist_state_effect(&mut effects);
            return RuntimeGroupIncomingResult {
                events: Vec::new(),
                effects,
            };
        };
        let mut state = self.state.lock().unwrap();
        match state
            .group_manager
            .handle_sender_key_message(message.clone())
        {
            Ok(GroupSenderKeyHandleResult::Event(event)) => {
                drop(state);
                let mut effects = Vec::new();
                let _ = self.persist_state_effect(&mut effects);
                if let Ok(subscription_effects) = self.refresh_protocol_subscriptions() {
                    effects.extend(subscription_effects);
                }
                RuntimeGroupIncomingResult {
                    events: vec![event],
                    effects,
                }
            }
            Ok(GroupSenderKeyHandleResult::PendingDistribution { .. }) => {
                if !state.pending_group_sender_key_messages.contains(&parsed) {
                    state.pending_group_sender_key_messages.push(parsed);
                }
                drop(state);
                let mut effects = Vec::new();
                let _ = self.persist_state_effect(&mut effects);
                RuntimeGroupIncomingResult {
                    events: Vec::new(),
                    effects,
                }
            }
            Ok(GroupSenderKeyHandleResult::PendingRevision { .. }) => {
                if !state.pending_group_sender_key_messages.contains(&parsed) {
                    state.pending_group_sender_key_messages.push(parsed);
                }
                drop(state);
                let mut effects = Vec::new();
                let _ = self.persist_state_effect(&mut effects);
                RuntimeGroupIncomingResult {
                    events: Vec::new(),
                    effects,
                }
            }
            Ok(GroupSenderKeyHandleResult::Ignored) | Err(_) => {
                RuntimeGroupIncomingResult::default()
            }
        }
    }

    fn retry_pending_group_pairwise_payloads(&self) -> Vec<GroupIncomingEvent> {
        let pending =
            std::mem::take(&mut self.state.lock().unwrap().pending_group_pairwise_payloads);
        if pending.is_empty() {
            return Vec::new();
        }
        let mut events = Vec::new();
        let mut still_pending = Vec::new();
        {
            let mut state = self.state.lock().unwrap();
            for pending_payload in pending {
                let result = match pending_payload.sender_device {
                    Some(sender_device) => state.group_manager.handle_pairwise_payload(
                        pending_payload.sender_owner,
                        sender_device,
                        &pending_payload.payload,
                    ),
                    None => state
                        .group_manager
                        .handle_incoming(pending_payload.sender_owner, &pending_payload.payload),
                };
                match result {
                    Ok(Some(event)) => events.push(event),
                    Ok(None) => {}
                    Err(error) if should_queue_group_pairwise_payload(&error) => {
                        still_pending.push(pending_payload)
                    }
                    Err(_) => {}
                }
            }
            state.pending_group_pairwise_payloads = still_pending;
        }
        events
    }

    fn retry_pending_group_sender_key_messages(&self) -> Vec<GroupIncomingEvent> {
        let pending =
            std::mem::take(&mut self.state.lock().unwrap().pending_group_sender_key_messages);
        if pending.is_empty() {
            return Vec::new();
        }
        let mut events = Vec::new();
        let mut still_pending = Vec::new();
        {
            let mut state = self.state.lock().unwrap();
            for parsed in pending {
                let Some(group_id) = state
                    .group_manager
                    .group_id_for_sender_event_pubkey(parsed.sender_event_pubkey)
                else {
                    still_pending.push(parsed);
                    continue;
                };
                let message = GroupSenderKeyMessage {
                    group_id,
                    sender_event_pubkey: parsed.sender_event_pubkey,
                    key_id: parsed.key_id,
                    message_number: parsed.message_number,
                    created_at: parsed.created_at,
                    ciphertext: parsed.ciphertext.clone(),
                };
                match state
                    .group_manager
                    .handle_sender_key_message(message.clone())
                {
                    Ok(GroupSenderKeyHandleResult::Event(event)) => events.push(event),
                    Ok(GroupSenderKeyHandleResult::PendingDistribution { .. }) => {
                        still_pending.push(parsed)
                    }
                    Ok(GroupSenderKeyHandleResult::PendingRevision { .. }) => {
                        still_pending.push(parsed)
                    }
                    Ok(GroupSenderKeyHandleResult::Ignored) => {}
                    Err(_) => {}
                }
            }
            state.pending_group_sender_key_messages = still_pending;
        }
        events
    }

    fn group_sender_key_message_from_parsed(
        &self,
        parsed: &nostr_codec::ParsedGroupSenderKeyMessageEvent,
    ) -> Option<GroupSenderKeyMessage> {
        let group_id = self
            .state
            .lock()
            .unwrap()
            .group_manager
            .group_id_for_sender_event_pubkey(parsed.sender_event_pubkey)?;
        Some(GroupSenderKeyMessage {
            group_id,
            sender_event_pubkey: parsed.sender_event_pubkey,
            key_id: parsed.key_id,
            message_number: parsed.message_number,
            created_at: parsed.created_at,
            ciphertext: parsed.ciphertext.clone(),
        })
    }

    pub fn queued_message_diagnostics(
        &self,
        inner_event_id: Option<&str>,
    ) -> Result<Vec<QueuedMessageDiagnostic>> {
        let diagnostics = self
            .state
            .lock()
            .unwrap()
            .pending_outbound
            .iter()
            .filter(|pending| {
                inner_event_id
                    .map(|id| pending.inner_event_id.as_deref() == Some(id))
                    .unwrap_or(true)
            })
            .map(|pending| QueuedMessageDiagnostic {
                stage: pending.reason.clone(),
                target_key: public_owner(pending.recipient_owner).to_hex(),
                owner_pubkey: Some(public_owner(pending.recipient_owner)),
                inner_event_id: pending.inner_event_id.clone(),
                created_at_ms: pending.created_at_ms,
            })
            .collect();
        Ok(diagnostics)
    }

    fn process_invite_event(&self, event: &Event) -> Result<Vec<RuntimeEffect>> {
        let invite = nostr_codec::parse_invite_event(event)
            .map_err(|e| Error::InvalidEvent(e.to_string()))?;
        let invite_owner = invite
            .inviter_owner_pubkey
            .map(public_owner)
            .unwrap_or_else(|| public_device(invite.inviter_device_pubkey));
        self.state
            .lock()
            .unwrap()
            .core
            .observe_device_invite(owner(invite_owner), invite)?;
        let mut effects = Vec::new();
        self.persist_state_effect(&mut effects)?;
        effects.extend(self.retry_pending_outbound()?);
        effects.extend(self.retry_pending_group_fanouts()?);
        Ok(effects)
    }

    fn process_invite_response_event(&self, event: &Event) -> Result<Vec<RuntimeEffect>> {
        if self
            .state
            .lock()
            .unwrap()
            .processed_invite_response_ids
            .contains(&event.id.to_string())
        {
            return Ok(Vec::new());
        }
        let envelope = nostr_codec::parse_invite_response_event(event)
            .map_err(|e| Error::InvalidEvent(e.to_string()))?;
        let mut rng = OsRng;
        let mut ctx = ProtocolContext::new(now(), &mut rng);
        let processed = self
            .state
            .lock()
            .unwrap()
            .core
            .observe_invite_response(&mut ctx, &envelope)?;
        if processed.is_some() {
            self.state
                .lock()
                .unwrap()
                .processed_invite_response_ids
                .insert(event.id.to_string());
            let mut effects = Vec::new();
            self.persist_state_effect(&mut effects)?;
            effects.extend(self.retry_pending_outbound()?);
            effects.extend(self.retry_pending_group_fanouts()?);
            effects.extend(self.refresh_protocol_subscriptions()?);
            effects.extend(self.sync_direct_message_subscriptions()?);
            return Ok(effects);
        }
        Ok(Vec::new())
    }

    fn process_message_event(
        &self,
        event: &Event,
        event_id: Option<String>,
    ) -> Result<Vec<RuntimeEffect>> {
        let envelope = nostr_codec::parse_message_event(event)
            .map_err(|e| Error::InvalidEvent(e.to_string()))?;
        let sender_owner = self
            .resolve_message_sender_owner(envelope.sender)
            .unwrap_or_else(|| owner(public_device(envelope.sender)));
        let mut rng = OsRng;
        let mut ctx = ProtocolContext::new(now(), &mut rng);
        let received =
            self.state
                .lock()
                .unwrap()
                .core
                .receive(&mut ctx, sender_owner, &envelope)?;
        let Some(received) = received else {
            return Ok(Vec::new());
        };
        let (conversation_owner, payload) = decode_local_sibling_payload(&received.payload)
            .map(|(owner, payload)| (Some(owner), payload))
            .unwrap_or((None, received.payload));
        let content = String::from_utf8(payload).map_err(|e| Error::Decryption(e.to_string()))?;
        self.stage_pending_decrypted_delivery(PendingDecryptedDelivery {
            sender_owner: received.owner_pubkey,
            sender_device: Some(received.device_pubkey),
            conversation_owner: conversation_owner.map(owner),
            content,
            event_id,
            created_at_ms: current_unix_millis(),
        })
    }

    fn prepare_and_publish(
        &self,
        recipient_owner: OwnerPubkey,
        remote_payload: Vec<u8>,
        local_sibling_payload: Option<Vec<u8>>,
        inner_event_id: Option<String>,
    ) -> Result<RuntimePublishResult> {
        let now = now();
        let mut rng = OsRng;
        let mut ctx = ProtocolContext::new(now, &mut rng);
        let (remote, local) = {
            let mut state = self.state.lock().unwrap();
            let remote = state.core.prepare_remote_send(
                &mut ctx,
                recipient_owner,
                remote_payload.clone(),
            )?;
            let local = match local_sibling_payload.clone() {
                Some(payload) => Some(
                    state
                        .core
                        .prepare_local_sibling_send_refreshing_one_way_sessions(
                            &mut ctx, payload,
                        )?,
                ),
                None => None,
            };
            (remote, local)
        };

        let has_gap = !remote.relay_gaps.is_empty()
            || local
                .as_ref()
                .is_some_and(|prepared| !prepared.relay_gaps.is_empty());
        if has_gap {
            let reason = if remote
                .relay_gaps
                .iter()
                .chain(local.as_ref().into_iter().flat_map(|p| p.relay_gaps.iter()))
                .any(|gap| matches!(gap, RelayGap::MissingRoster { .. }))
            {
                QueuedMessageStage::Discovery
            } else {
                QueuedMessageStage::Device
            };
            {
                let mut state = self.state.lock().unwrap();
                if let Some(existing) = state.pending_outbound.iter_mut().find(|pending| {
                    same_pending_outbound(
                        pending,
                        recipient_owner,
                        inner_event_id.as_deref(),
                        &remote_payload,
                        local_sibling_payload.as_deref(),
                    )
                }) {
                    existing.reason = reason;
                } else {
                    state.pending_outbound.push(PendingOutbound {
                        recipient_owner,
                        remote_payload,
                        local_sibling_payload,
                        inner_event_id,
                        created_at_ms: current_unix_millis(),
                        reason,
                    });
                }
            }
            let mut effects = Vec::new();
            self.persist_state_effect(&mut effects)?;
            effects.extend(self.refresh_protocol_subscriptions()?);
            return Ok(RuntimePublishResult {
                event_ids: Vec::new(),
                effects,
            });
        }

        let mut event_ids = Vec::new();
        let mut pending =
            self.prepared_publishes_from_prepared(&remote, inner_event_id.clone(), &mut event_ids)?;
        if let Some(local) = local.as_ref() {
            pending.extend(self.prepared_publishes_from_prepared(
                local,
                inner_event_id,
                &mut event_ids,
            )?);
        }
        let mut effects = self.stage_prepared_publishes(pending)?;
        effects.extend(self.sync_direct_message_subscriptions()?);
        Ok(RuntimePublishResult { event_ids, effects })
    }

    fn prepared_publishes_from_prepared(
        &self,
        prepared: &crate::PreparedSend,
        inner_event_id: Option<String>,
        event_ids: &mut Vec<String>,
    ) -> Result<Vec<PendingPreparedPublish>> {
        let mut pending = Vec::new();
        for response in &prepared.invite_responses {
            let event = nostr_codec::invite_response_event(response)
                .map_err(|e| Error::InvalidEvent(e.to_string()))?;
            pending.push(PendingPreparedPublish {
                event,
                inner_event_id: None,
                target_device_id: None,
            });
        }
        for delivery in &prepared.deliveries {
            let event = nostr_codec::message_event(&delivery.envelope)
                .map_err(|e| Error::InvalidEvent(e.to_string()))?;
            event_ids.push(event.id.to_string());
            pending.push(PendingPreparedPublish {
                event,
                inner_event_id: inner_event_id.clone(),
                target_device_id: Some(public_device(delivery.device_pubkey).to_hex()),
            });
        }
        Ok(pending)
    }

    fn publish_group_prepared_send(
        &self,
        prepared: &GroupPreparedSend,
        inner_event_id: Option<String>,
    ) -> Result<RuntimePublishResult> {
        let mut event_ids = Vec::new();
        let mut effects = Vec::new();
        let queued_remote = self.queue_group_pending_fanouts(
            &prepared.group_id,
            &prepared.remote,
            inner_event_id.clone(),
        );
        let queued_local = self.queue_group_pending_fanouts(
            &prepared.group_id,
            &prepared.local_sibling,
            inner_event_id.clone(),
        );
        if queued_remote || queued_local {
            self.persist_state_effect(&mut effects)?;
        }
        let mut pending = self.group_prepared_publishes_from_prepared(
            &prepared.remote,
            inner_event_id.clone(),
            &mut event_ids,
        )?;
        pending.extend(self.group_prepared_publishes_from_prepared(
            &prepared.local_sibling,
            inner_event_id,
            &mut event_ids,
        )?);
        effects.extend(self.stage_prepared_publishes(pending)?);
        Ok(RuntimePublishResult { event_ids, effects })
    }

    fn queue_group_pending_fanouts(
        &self,
        group_id: &str,
        prepared: &GroupPreparedPublish,
        inner_event_id: Option<String>,
    ) -> bool {
        if prepared.pending_fanouts.is_empty() {
            return false;
        }
        let mut state = self.state.lock().unwrap();
        let mut changed = false;
        for fanout in &prepared.pending_fanouts {
            let pending = PendingGroupFanout {
                group_id: group_id.to_string(),
                fanout: fanout.clone(),
                inner_event_id: inner_event_id.clone(),
                created_at_ms: current_unix_millis(),
            };
            if !state.pending_group_fanouts.contains(&pending) {
                state.pending_group_fanouts.push(pending);
                changed = true;
            }
        }
        changed
    }

    fn group_prepared_publishes_from_prepared(
        &self,
        prepared: &GroupPreparedPublish,
        inner_event_id: Option<String>,
        event_ids: &mut Vec<String>,
    ) -> Result<Vec<PendingPreparedPublish>> {
        let mut pending = Vec::new();
        for response in &prepared.invite_responses {
            let event = nostr_codec::invite_response_event(response)
                .map_err(|e| Error::InvalidEvent(e.to_string()))?;
            pending.push(PendingPreparedPublish {
                event,
                inner_event_id: None,
                target_device_id: None,
            });
        }
        for delivery in &prepared.deliveries {
            let event = nostr_codec::message_event(&delivery.envelope)
                .map_err(|e| Error::InvalidEvent(e.to_string()))?;
            event_ids.push(event.id.to_string());
            pending.push(PendingPreparedPublish {
                event,
                inner_event_id: inner_event_id.clone(),
                target_device_id: Some(public_device(delivery.device_pubkey).to_hex()),
            });
        }
        for sender_key_message in &prepared.sender_key_messages {
            let event = nostr_codec::group_sender_key_message_event(sender_key_message)
                .map_err(|e| Error::InvalidEvent(e.to_string()))?;
            event_ids.push(event.id.to_string());
            pending.push(PendingPreparedPublish {
                event,
                inner_event_id: None,
                target_device_id: None,
            });
        }
        Ok(pending)
    }

    fn publish_group_prepared(
        &self,
        prepared: &GroupPreparedPublish,
        inner_event_id: Option<String>,
        event_ids: &mut Vec<String>,
    ) -> Result<Vec<RuntimeEffect>> {
        let pending =
            self.group_prepared_publishes_from_prepared(prepared, inner_event_id, event_ids)?;
        self.stage_prepared_publishes(pending)
    }

    fn stage_prepared_publishes(
        &self,
        pending: Vec<PendingPreparedPublish>,
    ) -> Result<Vec<RuntimeEffect>> {
        if pending.is_empty() {
            return Ok(Vec::new());
        }
        {
            let mut state = self.state.lock().unwrap();
            for next in &pending {
                let event_id = next.event.id.to_string();
                if !state
                    .pending_prepared_publishes
                    .iter()
                    .any(|existing| existing.event.id.to_string() == event_id)
                {
                    state.pending_prepared_publishes.push(next.clone());
                }
            }
        }
        let mut effects = Vec::new();
        self.persist_state_effect(&mut effects)?;
        for next in pending {
            effects.push(Self::prepared_publish_effect(&next));
        }
        Ok(effects)
    }

    fn stage_pending_decrypted_delivery(
        &self,
        pending: PendingDecryptedDelivery,
    ) -> Result<Vec<RuntimeEffect>> {
        {
            let mut state = self.state.lock().unwrap();
            let already_pending = pending.event_id.as_ref().is_some_and(|event_id| {
                state
                    .pending_decrypted_deliveries
                    .iter()
                    .any(|existing| existing.event_id.as_ref() == Some(event_id))
            });
            if !already_pending {
                state.pending_decrypted_deliveries.push(pending.clone());
            }
        }
        let mut effects = Vec::new();
        self.persist_state_effect(&mut effects)?;
        effects.push(Self::decrypted_delivery_effect(&pending));
        Ok(effects)
    }

    fn pending_decrypted_delivery_effect_by_event_id(
        &self,
        event_id: &str,
    ) -> Option<RuntimeEffect> {
        self.state
            .lock()
            .unwrap()
            .pending_decrypted_deliveries
            .iter()
            .find(|pending| pending.event_id.as_deref() == Some(event_id))
            .map(Self::decrypted_delivery_effect)
    }

    fn replay_pending_decrypted_deliveries(&self) -> Vec<RuntimeEffect> {
        self.state
            .lock()
            .unwrap()
            .pending_decrypted_deliveries
            .iter()
            .map(Self::decrypted_delivery_effect)
            .collect()
    }

    fn decrypted_delivery_effect(pending: &PendingDecryptedDelivery) -> RuntimeEffect {
        RuntimeEffect::EmitDecrypted {
            sender: public_owner(pending.sender_owner),
            sender_device: pending.sender_device.map(public_device),
            conversation_owner: pending.conversation_owner.map(public_owner),
            content: pending.content.clone(),
            event_id: pending.event_id.clone(),
        }
    }

    fn replay_pending_prepared_publishes(&self) -> Vec<RuntimeEffect> {
        self.state
            .lock()
            .unwrap()
            .pending_prepared_publishes
            .clone()
            .into_iter()
            .map(|pending| Self::prepared_publish_effect(&pending))
            .collect()
    }

    fn prepared_publish_effect(pending: &PendingPreparedPublish) -> RuntimeEffect {
        if pending.inner_event_id.is_some() || pending.target_device_id.is_some() {
            RuntimeEffect::PublishSignedForInnerEvent {
                event: pending.event.clone(),
                inner_event_id: pending.inner_event_id.clone(),
                target_device_id: pending.target_device_id.clone(),
            }
        } else {
            RuntimeEffect::PublishSigned(pending.event.clone())
        }
    }

    fn retry_pending_outbound(&self) -> Result<Vec<RuntimeEffect>> {
        let pending = std::mem::take(&mut self.state.lock().unwrap().pending_outbound);
        let mut still_pending = Vec::new();
        let mut effects = Vec::new();
        for pending_send in pending {
            let result = self.prepare_and_publish(
                pending_send.recipient_owner,
                pending_send.remote_payload.clone(),
                pending_send.local_sibling_payload.clone(),
                pending_send.inner_event_id.clone(),
            );
            match result {
                Ok(result) => effects.extend(result.effects),
                Err(_) => still_pending.push(pending_send),
            }
        }
        self.state
            .lock()
            .unwrap()
            .pending_outbound
            .extend(still_pending);
        self.persist_state_effect(&mut effects)?;
        Ok(effects)
    }

    fn retry_pending_group_fanouts(&self) -> Result<Vec<RuntimeEffect>> {
        let pending = std::mem::take(&mut self.state.lock().unwrap().pending_group_fanouts);
        if pending.is_empty() {
            return Ok(Vec::new());
        }

        let mut still_pending = Vec::new();
        let mut effects = Vec::new();
        for pending_fanout in pending {
            let now = now();
            let mut rng = OsRng;
            let mut ctx = ProtocolContext::new(now, &mut rng);
            let result = {
                let mut state = self.state.lock().unwrap();
                match &pending_fanout.fanout {
                    GroupPendingFanout::Remote {
                        recipient_owner,
                        payload,
                    } => {
                        state
                            .core
                            .prepare_remote_send(&mut ctx, *recipient_owner, payload.clone())
                    }
                    GroupPendingFanout::LocalSiblings { payload } => state
                        .core
                        .prepare_local_sibling_send_reusing_all_sessions(&mut ctx, payload.clone()),
                }
            };

            let prepared = match result {
                Ok(prepared) => {
                    group_publish_from_prepared_send(prepared, pending_fanout.fanout.clone())
                }
                Err(_) => {
                    still_pending.push(pending_fanout);
                    continue;
                }
            };

            let still_has_gap = !prepared.relay_gaps.is_empty();
            let mut event_ids = Vec::new();
            effects.extend(self.publish_group_prepared(
                &prepared,
                pending_fanout.inner_event_id.clone(),
                &mut event_ids,
            )?);
            if still_has_gap {
                still_pending.push(pending_fanout);
            }
        }

        self.state
            .lock()
            .unwrap()
            .pending_group_fanouts
            .extend(still_pending);
        self.persist_state_effect(&mut effects)?;
        effects.extend(self.refresh_protocol_subscriptions()?);
        effects.extend(self.sync_direct_message_subscriptions()?);
        Ok(effects)
    }

    fn send_link_bootstrap(&self, invite_owner: PublicKey) -> Result<RuntimePublishResult> {
        let now = now();
        let expires_at = now.get().saturating_add(60);
        let event = pairwise_codec::typing_event(
            self.owner_public_key,
            pairwise_codec::EncodeOptions::new(now.get(), current_unix_millis())
                .with_expiration(expires_at),
        )
        .map_err(|e| Error::InvalidEvent(e.to_string()))?;
        self.send_event(invite_owner, event)
    }

    fn publish_local_invite_effect(&self, invite: &Invite) -> Result<RuntimeEffect> {
        let event = nostr_codec::invite_unsigned_event(invite)
            .map_err(|e| Error::InvalidEvent(e.to_string()))?;
        let keys = Keys::new(nostr::SecretKey::from_slice(&self.our_identity_key)?);
        let signed = event.sign_with_keys(&keys)?;
        Ok(RuntimeEffect::PublishSigned(signed))
    }

    fn refresh_protocol_subscriptions(&self) -> Result<Vec<RuntimeEffect>> {
        let mut filters = Vec::new();
        let owners = self.protocol_owner_pubkeys();
        if !owners.is_empty() {
            filters.push(
                Filter::new()
                    .kind(Kind::from(APP_KEYS_EVENT_KIND as u16))
                    .authors(owners.clone()),
            );
        }
        let invite_authors = self.known_device_identity_pubkeys_for_owners(owners);
        if !invite_authors.is_empty() {
            filters.push(
                Filter::new()
                    .kind(Kind::from(INVITE_EVENT_KIND as u16))
                    .authors(invite_authors),
            );
        }
        if let Some(invite_response_pubkey) = self.current_device_invite_response_pubkey() {
            filters.push(
                Filter::new()
                    .kind(Kind::from(INVITE_RESPONSE_KIND as u16))
                    .pubkey(invite_response_pubkey),
            );
        }
        if filters.is_empty() {
            return Ok(vec![RuntimeEffect::Unsubscribe(PROTOCOL_SUBID.to_string())]);
        }
        Ok(vec![RuntimeEffect::Subscribe {
            subid: PROTOCOL_SUBID.to_string(),
            filters,
        }])
    }

    fn protocol_owner_pubkeys(&self) -> Vec<PublicKey> {
        let mut owners = BTreeSet::new();
        owners.insert(self.owner_public_key);
        owners.extend(self.known_peer_owner_pubkeys());
        owners.extend(
            self.state
                .lock()
                .unwrap()
                .tracked_owner_pubkeys
                .iter()
                .copied(),
        );
        let pending_owners = self
            .state
            .lock()
            .unwrap()
            .pending_outbound
            .iter()
            .map(|pending| public_owner(pending.recipient_owner))
            .collect::<Vec<_>>();
        owners.extend(pending_owners);
        let pending_group_owners = self
            .state
            .lock()
            .unwrap()
            .pending_group_fanouts
            .iter()
            .filter_map(|pending| match &pending.fanout {
                GroupPendingFanout::Remote {
                    recipient_owner, ..
                } => Some(public_owner(*recipient_owner)),
                GroupPendingFanout::LocalSiblings { .. } => None,
            })
            .collect::<Vec<_>>();
        owners.extend(pending_group_owners);
        owners.into_iter().collect()
    }

    fn observe_owner_if_absent(&self, owner_pubkey: PublicKey) {
        self.state
            .lock()
            .unwrap()
            .tracked_owner_pubkeys
            .insert(owner_pubkey);
    }

    fn resolve_message_sender_owner(&self, sender: DevicePubkey) -> Option<OwnerPubkey> {
        let snapshot = self.state.lock().unwrap().core.snapshot();
        for user in snapshot.users {
            for record in user.devices {
                if record
                    .active_session
                    .as_ref()
                    .is_some_and(|state| session_matches_sender(state, sender))
                    || record
                        .inactive_sessions
                        .iter()
                        .any(|state| session_matches_sender(state, sender))
                {
                    return Some(user.owner_pubkey);
                }
            }
        }
        None
    }

    fn export_all_session_states(&self) -> Vec<(PublicKey, PublicKey, SessionState)> {
        self.state
            .lock()
            .unwrap()
            .core
            .snapshot()
            .users
            .into_iter()
            .flat_map(|user| {
                user.devices.into_iter().flat_map(move |device_record| {
                    device_record
                        .active_session
                        .into_iter()
                        .chain(device_record.inactive_sessions)
                        .map(move |state| {
                            (
                                public_owner(user.owner_pubkey),
                                public_device(device_record.device_pubkey),
                                state,
                            )
                        })
                })
            })
            .collect()
    }

    fn persist_state_effect(&self, effects: &mut Vec<RuntimeEffect>) -> Result<()> {
        let value = self.persist_state()?;
        effects.push(RuntimeEffect::PersistRuntimeState {
            key: RUNTIME_STATE_KEY.to_string(),
            value,
        });
        Ok(())
    }

    fn persist_state(&self) -> Result<String> {
        let state = self.state.lock().unwrap();
        let stored = StoredRuntimeState {
            core: state.core.snapshot(),
            group_manager: Some(state.group_manager.snapshot()),
            pending_prepared_publishes: state.pending_prepared_publishes.clone(),
            pending_group_sender_key_messages: state.pending_group_sender_key_messages.clone(),
            pending_group_pairwise_payloads: state.pending_group_pairwise_payloads.clone(),
            pending_group_fanouts: state.pending_group_fanouts.clone(),
            pending_decrypted_deliveries: state.pending_decrypted_deliveries.clone(),
            pending_outbound: state.pending_outbound.clone(),
            processed_invite_response_ids: state
                .processed_invite_response_ids
                .iter()
                .cloned()
                .collect(),
            latest_app_keys_created_at: state
                .latest_app_keys_created_at
                .iter()
                .map(|(k, v)| (k.to_hex(), *v))
                .collect(),
            tracked_owner_pubkeys: state.tracked_owner_pubkeys.iter().copied().collect(),
        };
        Ok(serde_json::to_string(&stored)?)
    }
}

impl RuntimeState {
    fn core_and_group_mut(&mut self) -> (&mut SessionManager, &mut GroupManager) {
        (&mut self.core, &mut self.group_manager)
    }

    fn load(
        storage: &dyn StorageAdapter,
        owner_public_key: PublicKey,
        identity_key: [u8; 32],
    ) -> Option<Self> {
        let raw = storage.get(RUNTIME_STATE_KEY).ok().flatten()?;
        let stored: StoredRuntimeState = serde_json::from_str(&raw).ok()?;
        let core = SessionManager::from_snapshot(stored.core, identity_key).ok()?;
        let group_manager = stored
            .group_manager
            .map(GroupManager::from_snapshot)
            .transpose()
            .ok()
            .flatten()
            .unwrap_or_else(|| GroupManager::new(owner(owner_public_key)));
        Some(Self {
            core,
            group_manager,
            pending_prepared_publishes: stored.pending_prepared_publishes,
            pending_group_sender_key_messages: stored.pending_group_sender_key_messages,
            pending_group_pairwise_payloads: stored.pending_group_pairwise_payloads,
            pending_group_fanouts: stored.pending_group_fanouts,
            pending_decrypted_deliveries: stored.pending_decrypted_deliveries,
            pending_outbound: stored.pending_outbound,
            processed_invite_response_ids: stored
                .processed_invite_response_ids
                .into_iter()
                .collect(),
            latest_app_keys_created_at: stored
                .latest_app_keys_created_at
                .into_iter()
                .filter_map(|(k, v)| PublicKey::parse(&k).ok().map(|pk| (pk, v)))
                .collect(),
            tracked_owner_pubkeys: stored.tracked_owner_pubkeys.into_iter().collect(),
        })
        .or_else(|| {
            Some(Self {
                core: SessionManager::new(owner(owner_public_key), identity_key),
                group_manager: GroupManager::new(owner(owner_public_key)),
                pending_prepared_publishes: Vec::new(),
                pending_group_sender_key_messages: Vec::new(),
                pending_group_pairwise_payloads: Vec::new(),
                pending_group_fanouts: Vec::new(),
                pending_decrypted_deliveries: Vec::new(),
                pending_outbound: Vec::new(),
                processed_invite_response_ids: HashSet::new(),
                latest_app_keys_created_at: HashMap::new(),
                tracked_owner_pubkeys: BTreeSet::new(),
            })
        })
    }
}

fn group_publish_from_prepared_send(
    prepared: crate::PreparedSend,
    fanout: GroupPendingFanout,
) -> GroupPreparedPublish {
    let pending_fanouts = if prepared.relay_gaps.is_empty() {
        Vec::new()
    } else {
        vec![fanout]
    };
    GroupPreparedPublish {
        deliveries: prepared.deliveries,
        invite_responses: prepared.invite_responses,
        sender_key_messages: Vec::new(),
        relay_gaps: prepared.relay_gaps,
        pending_fanouts,
    }
}

fn local_sibling_payload(conversation_owner: PublicKey, payload: &[u8]) -> Result<Vec<u8>> {
    use base64::Engine;
    let wrapper = LocalSiblingPayload {
        protocol: "ndr-local-sibling-copy".to_string(),
        version: 1,
        conversation_owner: conversation_owner.to_hex(),
        payload: base64::engine::general_purpose::STANDARD.encode(payload),
    };
    Ok(serde_json::to_vec(&wrapper)?)
}

fn decode_local_sibling_payload(payload: &[u8]) -> Option<(PublicKey, Vec<u8>)> {
    use base64::Engine;
    let wrapper: LocalSiblingPayload = serde_json::from_slice(payload).ok()?;
    if wrapper.protocol != "ndr-local-sibling-copy" || wrapper.version != 1 {
        return None;
    }
    let owner = PublicKey::parse(&wrapper.conversation_owner).ok()?;
    let payload = base64::engine::general_purpose::STANDARD
        .decode(wrapper.payload)
        .ok()?;
    Some((owner, payload))
}

fn collect_expected_senders(state: &SessionState, out: &mut BTreeSet<PublicKey>) {
    if let Some(current) = state.their_current_nostr_public_key {
        out.insert(public_device(current));
    }
    if let Some(next) = state.their_next_nostr_public_key {
        out.insert(public_device(next));
    }
    out.extend(state.skipped_keys.keys().copied().map(public_device));
}

fn session_matches_sender(state: &SessionState, sender: DevicePubkey) -> bool {
    state.their_current_nostr_public_key == Some(sender)
        || state.their_next_nostr_public_key == Some(sender)
        || state.skipped_keys.contains_key(&sender)
}

fn should_replace_provisional_local_roster(
    snapshot: &SessionManagerSnapshot,
    owner_pubkey: PublicKey,
    local_device_pubkey: PublicKey,
    incoming_roster: &DeviceRoster,
) -> bool {
    let incoming_devices = incoming_roster.devices();
    if incoming_devices.len() <= 1
        || !incoming_devices
            .iter()
            .any(|entry| entry.device_pubkey == device(local_device_pubkey))
    {
        return false;
    }

    let Some(current_roster) = snapshot
        .users
        .iter()
        .find(|user| user.owner_pubkey == owner(owner_pubkey))
        .and_then(|user| user.roster.as_ref())
    else {
        return false;
    };
    let current_devices = current_roster.devices();
    current_devices.len() == 1
        && current_devices[0].device_pubkey == device(local_device_pubkey)
        && current_roster.created_at > incoming_roster.created_at
}

fn owner(public_key: PublicKey) -> OwnerPubkey {
    OwnerPubkey::from_bytes(public_key.to_bytes())
}

fn device(public_key: PublicKey) -> DevicePubkey {
    DevicePubkey::from_bytes(public_key.to_bytes())
}

fn public_owner(owner: OwnerPubkey) -> PublicKey {
    PublicKey::from_slice(&owner.to_bytes()).expect("owner pubkey bytes must be valid")
}

fn public_device(device: DevicePubkey) -> PublicKey {
    PublicKey::from_slice(&device.to_bytes()).expect("device pubkey bytes must be valid")
}

fn should_queue_group_pairwise_payload(error: &Error) -> bool {
    match error {
        Error::Domain(DomainError::PendingGroupRevision { .. }) => true,
        Error::Domain(DomainError::InvalidGroupOperation(message)) => {
            message.starts_with("unknown group `")
        }
        _ => false,
    }
}

fn now() -> UnixSeconds {
    UnixSeconds(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
}

fn current_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
