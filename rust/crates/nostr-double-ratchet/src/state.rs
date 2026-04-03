use crate::{
    AppKeys, DeviceId, DeviceLabels, DevicePubkey, DomainError, IncomingDirectMessageEnvelope,
    IncomingInviteResponseEnvelope, Invite, InviteResponse, OutgoingDirectMessageEnvelope,
    OutgoingInviteResponseEnvelope, OwnerPubkey, PeerBook, Result, Rumor, StoredPeerBook,
    UnixSeconds,
};
use rand::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub use crate::types::ProtocolContext;

pub struct NdrState {
    local: LocalState,
    peers: BTreeMap<OwnerPubkey, PeerState>,
}

#[derive(Debug, Clone)]
struct LocalState {
    owner_pubkey: OwnerPubkey,
    device_pubkey: DevicePubkey,
    device_id: Option<DeviceId>,
    app_keys: Option<AppKeys>,
    app_keys_created_at: UnixSeconds,
}

#[derive(Debug, Clone)]
struct PeerState {
    owner_pubkey: OwnerPubkey,
    sessions: PeerBook,
    app_keys: Option<AppKeys>,
    app_keys_created_at: UnixSeconds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppKeysSnapshotDecision {
    Advanced,
    Stale,
    MergedEqualTimestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NdrSnapshot {
    pub local: LocalSnapshot,
    pub peers: Vec<PeerSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalSnapshot {
    pub owner_pubkey: OwnerPubkey,
    pub device_pubkey: DevicePubkey,
    pub device_id: Option<DeviceId>,
    pub app_keys_created_at: UnixSeconds,
    pub authorized_devices: Vec<DeviceSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerSnapshot {
    pub owner_pubkey: OwnerPubkey,
    pub app_keys_created_at: UnixSeconds,
    pub authorized_devices: Vec<DeviceSnapshot>,
    pub sessions: StoredPeerBook,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceSnapshot {
    pub identity_pubkey: DevicePubkey,
    pub created_at: UnixSeconds,
    pub device_label: Option<String>,
    pub client_label: Option<String>,
    pub labels_updated_at: Option<UnixSeconds>,
}

#[derive(Debug, Clone)]
pub struct PreparedDirectMessage {
    pub peer_owner_pubkey: OwnerPubkey,
    pub peer_device_id: DeviceId,
    pub envelope: OutgoingDirectMessageEnvelope,
    pub rumor: Rumor,
}

#[derive(Debug, Clone)]
pub struct ReceivedDirectMessage {
    pub peer_owner_pubkey: OwnerPubkey,
    pub peer_device_id: DeviceId,
    pub rumor: Rumor,
}

#[derive(Debug, Clone)]
pub struct InviteAcceptance {
    pub inviter_pubkey: OwnerPubkey,
    pub inviter_device_id: Option<DeviceId>,
    pub response: OutgoingInviteResponseEnvelope,
}

#[derive(Debug, Clone)]
pub struct ProcessedInviteResponse {
    pub peer_owner_pubkey: OwnerPubkey,
    pub peer_device_id: Option<DeviceId>,
    pub invitee_identity: DevicePubkey,
}

impl NdrState {
    pub fn new(
        owner_pubkey: OwnerPubkey,
        device_pubkey: DevicePubkey,
        device_id: Option<DeviceId>,
    ) -> Self {
        Self {
            local: LocalState {
                owner_pubkey,
                device_pubkey,
                device_id,
                app_keys: None,
                app_keys_created_at: UnixSeconds(0),
            },
            peers: BTreeMap::new(),
        }
    }

    pub fn single_device(identity_pubkey: DevicePubkey, device_id: Option<DeviceId>) -> Self {
        Self::new(identity_pubkey.as_owner(), identity_pubkey, device_id)
    }

    pub fn owner_pubkey(&self) -> OwnerPubkey {
        self.local.owner_pubkey
    }

    pub fn device_pubkey(&self) -> DevicePubkey {
        self.local.device_pubkey
    }

    pub fn snapshot(&self) -> NdrSnapshot {
        NdrSnapshot {
            local: LocalSnapshot {
                owner_pubkey: self.local.owner_pubkey,
                device_pubkey: self.local.device_pubkey,
                device_id: self.local.device_id.clone(),
                app_keys_created_at: self.local.app_keys_created_at,
                authorized_devices: snapshot_devices(self.local.app_keys.as_ref()),
            },
            peers: self.peers.values().map(PeerState::snapshot).collect(),
        }
    }

    pub fn apply_local_app_keys(
        &mut self,
        incoming_app_keys: AppKeys,
        created_at: UnixSeconds,
    ) -> AppKeysSnapshotDecision {
        let (decision, app_keys, created_at) = apply_app_keys_snapshot(
            self.local.app_keys.as_ref(),
            self.local.app_keys_created_at,
            &incoming_app_keys,
            created_at,
        );
        self.local.app_keys = Some(app_keys);
        self.local.app_keys_created_at = created_at;
        decision
    }

    pub fn apply_peer_app_keys(
        &mut self,
        owner_pubkey: OwnerPubkey,
        incoming_app_keys: AppKeys,
        created_at: UnixSeconds,
    ) -> AppKeysSnapshotDecision {
        let peer = self.peer_state_mut(owner_pubkey);
        let (decision, app_keys, created_at) = apply_app_keys_snapshot(
            peer.app_keys.as_ref(),
            peer.app_keys_created_at,
            &incoming_app_keys,
            created_at,
        );
        peer.app_keys = Some(app_keys);
        peer.app_keys_created_at = created_at;
        decision
    }

    pub fn upsert_peer_session(
        &mut self,
        owner_pubkey: OwnerPubkey,
        device_id: Option<DeviceId>,
        session: crate::Session,
        now: UnixSeconds,
    ) {
        self.peer_state_mut(owner_pubkey)
            .sessions
            .upsert_session(device_id, session, now);
    }

    pub fn send_direct_message<R>(
        &mut self,
        ctx: &mut ProtocolContext<'_, R>,
        owner_pubkey: OwnerPubkey,
        content: crate::DirectMessageContent,
    ) -> Result<PreparedDirectMessage>
    where
        R: RngCore + CryptoRng,
    {
        let rumor = Rumor::from_content(ctx, content)?;
        let peer = self
            .peers
            .get_mut(&owner_pubkey)
            .ok_or_else(|| DomainError::UnknownPeer(owner_pubkey.to_string()))?;
        let selection = peer
            .sessions
            .plan_send()
            .ok_or_else(|| DomainError::NoSendableSession(owner_pubkey.to_string()))?;
        let outcome = peer
            .sessions
            .apply_send(ctx.now_secs, &selection, |session| {
                let plan = session.plan_send(&rumor, ctx.now_secs)?;
                Ok(session.apply_send(plan))
            })?;
        Ok(PreparedDirectMessage {
            peer_owner_pubkey: owner_pubkey,
            peer_device_id: selection.device_id,
            envelope: outcome.envelope,
            rumor: outcome.rumor,
        })
    }

    pub fn send_text<R>(
        &mut self,
        ctx: &mut ProtocolContext<'_, R>,
        owner_pubkey: OwnerPubkey,
        text: String,
    ) -> Result<PreparedDirectMessage>
    where
        R: RngCore + CryptoRng,
    {
        self.send_direct_message(ctx, owner_pubkey, crate::DirectMessageContent::Text(text))
    }

    pub fn receive_direct_message<R>(
        &mut self,
        ctx: &mut ProtocolContext<'_, R>,
        owner_pubkey: OwnerPubkey,
        preferred_device_id: Option<&DeviceId>,
        envelope: &IncomingDirectMessageEnvelope,
    ) -> Result<Option<ReceivedDirectMessage>>
    where
        R: RngCore + CryptoRng,
    {
        let peer = self
            .peers
            .get_mut(&owner_pubkey)
            .ok_or_else(|| DomainError::UnknownPeer(owner_pubkey.to_string()))?;
        let Some(plan) = peer
            .sessions
            .plan_receive(ctx, envelope, preferred_device_id)?
        else {
            return Ok(None);
        };
        let device_id = plan.device_id.clone();
        let outcome = peer.sessions.commit_receive(ctx.now_secs, plan)?;
        Ok(Some(ReceivedDirectMessage {
            peer_owner_pubkey: owner_pubkey,
            peer_device_id: device_id,
            rumor: outcome.rumor,
        }))
    }

    pub fn create_invite<R>(
        &self,
        ctx: &mut ProtocolContext<'_, R>,
        max_uses: Option<usize>,
    ) -> Result<Invite>
    where
        R: RngCore + CryptoRng,
    {
        Invite::create_new(
            ctx,
            self.local.owner_pubkey,
            self.local.device_id.clone(),
            max_uses,
        )
    }

    pub fn accept_invite<R>(
        &mut self,
        ctx: &mut ProtocolContext<'_, R>,
        invite: &Invite,
        invitee_private_key: [u8; 32],
    ) -> Result<InviteAcceptance>
    where
        R: RngCore + CryptoRng,
    {
        let claimed_owner = (self.local.owner_pubkey != self.local.device_pubkey.as_owner())
            .then_some(self.local.owner_pubkey);
        let (session, response) = invite.accept_with_owner(
            ctx,
            self.local.device_pubkey,
            invitee_private_key,
            self.local.device_id.clone(),
            claimed_owner,
        )?;
        self.upsert_peer_session(
            invite.inviter,
            invite.device_id.clone(),
            session,
            ctx.now_secs,
        );
        Ok(InviteAcceptance {
            inviter_pubkey: invite.inviter,
            inviter_device_id: invite.device_id.clone(),
            response,
        })
    }

    pub fn process_invite_response<R>(
        &mut self,
        ctx: &mut ProtocolContext<'_, R>,
        invite: &mut Invite,
        envelope: &IncomingInviteResponseEnvelope,
        inviter_private_key: [u8; 32],
    ) -> Result<Option<ProcessedInviteResponse>>
    where
        R: RngCore + CryptoRng,
    {
        let Some(InviteResponse {
            session,
            invitee_identity,
            device_id,
            owner_public_key,
        }) = invite.process_invite_response(ctx, envelope, inviter_private_key)?
        else {
            return Ok(None);
        };
        let peer_owner_pubkey = owner_public_key.unwrap_or_else(|| invitee_identity.as_owner());
        self.upsert_peer_session(peer_owner_pubkey, device_id.clone(), session, ctx.now_secs);
        Ok(Some(ProcessedInviteResponse {
            peer_owner_pubkey,
            peer_device_id: device_id,
            invitee_identity,
        }))
    }

    fn peer_state_mut(&mut self, owner_pubkey: OwnerPubkey) -> &mut PeerState {
        self.peers.entry(owner_pubkey).or_insert_with(|| PeerState {
            owner_pubkey,
            sessions: PeerBook::new(),
            app_keys: None,
            app_keys_created_at: UnixSeconds(0),
        })
    }
}

impl PeerState {
    fn snapshot(&self) -> PeerSnapshot {
        PeerSnapshot {
            owner_pubkey: self.owner_pubkey,
            app_keys_created_at: self.app_keys_created_at,
            authorized_devices: snapshot_devices(self.app_keys.as_ref()),
            sessions: self.sessions.snapshot(),
        }
    }
}

fn snapshot_devices(app_keys: Option<&AppKeys>) -> Vec<DeviceSnapshot> {
    let Some(app_keys) = app_keys else {
        return Vec::new();
    };

    app_keys
        .get_all_devices()
        .into_iter()
        .map(|device| {
            let labels: Option<&DeviceLabels> = app_keys.get_device_labels(&device.identity_pubkey);
            DeviceSnapshot {
                identity_pubkey: device.identity_pubkey,
                created_at: device.created_at,
                device_label: labels.and_then(|labels| labels.device_label.clone()),
                client_label: labels.and_then(|labels| labels.client_label.clone()),
                labels_updated_at: labels.map(|labels| labels.updated_at),
            }
        })
        .collect()
}

fn apply_app_keys_snapshot(
    current_app_keys: Option<&AppKeys>,
    current_created_at: UnixSeconds,
    incoming_app_keys: &AppKeys,
    incoming_created_at: UnixSeconds,
) -> (AppKeysSnapshotDecision, AppKeys, UnixSeconds) {
    if current_app_keys.is_none() || incoming_created_at > current_created_at {
        return (
            AppKeysSnapshotDecision::Advanced,
            incoming_app_keys.clone(),
            incoming_created_at,
        );
    }

    let Some(current_app_keys) = current_app_keys else {
        return (
            AppKeysSnapshotDecision::Advanced,
            incoming_app_keys.clone(),
            incoming_created_at,
        );
    };
    if incoming_created_at < current_created_at {
        return (
            AppKeysSnapshotDecision::Stale,
            current_app_keys.clone(),
            current_created_at,
        );
    }

    (
        AppKeysSnapshotDecision::MergedEqualTimestamp,
        current_app_keys.merge(incoming_app_keys),
        current_created_at,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        device_pubkey_from_secret_bytes, AppKeysSnapshotDecision, DeviceEntry,
        DirectMessageContent, IncomingDirectMessageEnvelope, ProtocolContext, UnixMillis,
    };
    use rand::{rngs::StdRng, SeedableRng};

    fn context(seed: u64, now_secs: u64, now_millis: u64) -> ProtocolContext<'static, StdRng> {
        let rng = Box::new(StdRng::seed_from_u64(seed));
        let rng = Box::leak(rng);
        ProtocolContext::new(UnixSeconds(now_secs), UnixMillis(now_millis), rng)
    }

    fn single_device_state(secret: [u8; 32], device_id: &str) -> NdrState {
        let device_pubkey = device_pubkey_from_secret_bytes(&secret).unwrap();
        NdrState::single_device(device_pubkey, Some(DeviceId::new(device_id)))
    }

    #[test]
    fn invite_bootstrap_and_dm_roundtrip() {
        let alice_secret = [31u8; 32];
        let bob_secret = [32u8; 32];
        let alice_owner = device_pubkey_from_secret_bytes(&alice_secret)
            .unwrap()
            .as_owner();
        let bob_owner = device_pubkey_from_secret_bytes(&bob_secret)
            .unwrap()
            .as_owner();

        let mut alice = single_device_state(alice_secret, "alice-device");
        let mut bob = single_device_state(bob_secret, "bob-device");

        let mut invite_ctx = context(1, 1_700_001_000, 1_700_001_000_000);
        let mut invite = alice.create_invite(&mut invite_ctx, None).unwrap();

        let mut accept_ctx = context(2, 1_700_001_005, 1_700_001_005_000);
        let acceptance = bob
            .accept_invite(&mut accept_ctx, &invite, bob_secret)
            .unwrap();

        let mut process_ctx = context(3, 1_700_001_010, 1_700_001_010_000);
        let processed = alice
            .process_invite_response(
                &mut process_ctx,
                &mut invite,
                &IncomingInviteResponseEnvelope {
                    sender: acceptance.response.sender,
                    created_at: acceptance.response.created_at,
                    content: acceptance.response.content.clone(),
                },
                alice_secret,
            )
            .unwrap()
            .expect("processed invite response");
        assert_eq!(processed.peer_owner_pubkey, bob_owner);

        let mut send_ctx = context(4, 1_700_001_020, 1_700_001_020_000);
        let outbound = bob
            .send_text(&mut send_ctx, alice_owner, "hello alice".to_string())
            .unwrap();

        let mut receive_ctx = context(5, 1_700_001_021, 1_700_001_021_000);
        let received = alice
            .receive_direct_message(
                &mut receive_ctx,
                bob_owner,
                processed.peer_device_id.as_ref(),
                &IncomingDirectMessageEnvelope {
                    sender: outbound.envelope.sender,
                    created_at: outbound.envelope.created_at,
                    encrypted_header: outbound.envelope.encrypted_header.clone(),
                    ciphertext: outbound.envelope.ciphertext.clone(),
                },
            )
            .unwrap()
            .expect("received direct message");
        assert_eq!(received.rumor.content, "hello alice");
        assert_eq!(received.peer_owner_pubkey, bob_owner);

        let mut reply_ctx = context(6, 1_700_001_030, 1_700_001_030_000);
        let reply = alice
            .send_direct_message(
                &mut reply_ctx,
                bob_owner,
                DirectMessageContent::Reply {
                    reply_to: received.rumor.id.clone().unwrap(),
                    text: "hello bob".to_string(),
                },
            )
            .unwrap();
        assert_eq!(reply.peer_owner_pubkey, bob_owner);
        assert_eq!(reply.rumor.content, "hello bob");
    }

    #[test]
    fn app_keys_convergence_is_explicit() {
        let secret = [33u8; 32];
        let peer_secret = [34u8; 32];
        let device = device_pubkey_from_secret_bytes(&secret).unwrap();
        let peer_device = device_pubkey_from_secret_bytes(&peer_secret).unwrap();
        let peer_owner = peer_device.as_owner();
        let mut state = single_device_state(secret, "local-device");

        let first = AppKeys::new(vec![DeviceEntry {
            identity_pubkey: device,
            created_at: UnixSeconds(10),
        }]);
        let newer = AppKeys::new(vec![
            DeviceEntry {
                identity_pubkey: device,
                created_at: UnixSeconds(10),
            },
            DeviceEntry {
                identity_pubkey: peer_device,
                created_at: UnixSeconds(20),
            },
        ]);
        let older = AppKeys::new(vec![DeviceEntry {
            identity_pubkey: device,
            created_at: UnixSeconds(5),
        }]);

        assert_eq!(
            state.apply_local_app_keys(first.clone(), UnixSeconds(100)),
            AppKeysSnapshotDecision::Advanced
        );
        assert_eq!(
            state.apply_local_app_keys(older, UnixSeconds(50)),
            AppKeysSnapshotDecision::Stale
        );
        assert_eq!(
            state.apply_local_app_keys(newer.clone(), UnixSeconds(100)),
            AppKeysSnapshotDecision::MergedEqualTimestamp
        );
        assert_eq!(
            state.apply_peer_app_keys(peer_owner, newer, UnixSeconds(200)),
            AppKeysSnapshotDecision::Advanced
        );

        let snapshot = state.snapshot();
        assert_eq!(snapshot.local.authorized_devices.len(), 2);
        assert_eq!(snapshot.peers.len(), 1);
    }

    #[test]
    fn snapshot_is_deterministic() {
        let secret = [37u8; 32];
        let device_a = device_pubkey_from_secret_bytes(&secret).unwrap();
        let device_b = device_pubkey_from_secret_bytes(&[38u8; 32]).unwrap();

        let mut state_a = single_device_state(secret, "device");
        let mut state_b = single_device_state(secret, "device");

        let app_keys_ab = AppKeys::new(vec![
            DeviceEntry {
                identity_pubkey: device_a,
                created_at: UnixSeconds(10),
            },
            DeviceEntry {
                identity_pubkey: device_b,
                created_at: UnixSeconds(20),
            },
        ]);
        let app_keys_ba = AppKeys::new(vec![
            DeviceEntry {
                identity_pubkey: device_b,
                created_at: UnixSeconds(20),
            },
            DeviceEntry {
                identity_pubkey: device_a,
                created_at: UnixSeconds(10),
            },
        ]);

        state_a.apply_local_app_keys(app_keys_ab, UnixSeconds(100));
        state_b.apply_local_app_keys(app_keys_ba, UnixSeconds(100));
        assert_eq!(state_a.snapshot(), state_b.snapshot());
    }
}
