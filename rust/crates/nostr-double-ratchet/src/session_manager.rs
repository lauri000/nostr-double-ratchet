use crate::{
    state::AppKeysSnapshotDecision, AppKeys, DeviceId, DevicePubkey, DirectMessageContent,
    DomainError, IncomingDirectMessageEnvelope, IncomingInviteResponseEnvelope, Invite,
    InviteResponse, OutgoingDirectMessageEnvelope, OutgoingInviteResponseEnvelope, OwnerPubkey,
    ProtocolContext, Result, Rumor, Session, SessionState, UnixSeconds,
};
use rand::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const MAX_INACTIVE_SESSIONS: usize = 10;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionManagerPolicy {
    pub max_relay_latency: UnixSeconds,
}

impl Default for SessionManagerPolicy {
    fn default() -> Self {
        Self {
            max_relay_latency: UnixSeconds(7 * 24 * 60 * 60),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionManager {
    local_owner_pubkey: OwnerPubkey,
    local_device_pubkey: DevicePubkey,
    local_device_secret_key: [u8; 32],
    local_device_id: Option<DeviceId>,
    local_invite: Option<Invite>,
    users: BTreeMap<OwnerPubkey, UserRecord>,
    policy: SessionManagerPolicy,
}

#[derive(Debug, Clone)]
struct UserRecord {
    owner_pubkey: OwnerPubkey,
    app_keys: Option<AppKeys>,
    app_keys_created_at: UnixSeconds,
    devices: BTreeMap<DevicePubkey, DeviceRecord>,
}

#[derive(Debug, Clone)]
struct DeviceRecord {
    device_pubkey: DevicePubkey,
    device_id: Option<DeviceId>,
    authorized: bool,
    is_stale: bool,
    stale_since: Option<UnixSeconds>,
    public_invite: Option<Invite>,
    active_session: Option<Session>,
    inactive_sessions: Vec<Session>,
    last_activity: Option<UnixSeconds>,
    created_at: UnixSeconds,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionManagerSnapshot {
    pub local_owner_pubkey: OwnerPubkey,
    pub local_device_pubkey: DevicePubkey,
    pub local_device_id: Option<DeviceId>,
    pub local_invite: Option<Invite>,
    pub users: Vec<UserRecordSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserRecordSnapshot {
    pub owner_pubkey: OwnerPubkey,
    pub app_keys: Option<AppKeys>,
    pub app_keys_created_at: UnixSeconds,
    pub devices: Vec<DeviceRecordSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceRecordSnapshot {
    pub device_pubkey: DevicePubkey,
    pub device_id: Option<DeviceId>,
    pub authorized: bool,
    pub is_stale: bool,
    pub stale_since: Option<UnixSeconds>,
    pub public_invite: Option<Invite>,
    pub active_session: Option<SessionState>,
    pub inactive_sessions: Vec<SessionState>,
    pub last_activity: Option<UnixSeconds>,
    pub created_at: UnixSeconds,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedFanout {
    pub recipient_owner: OwnerPubkey,
    pub rumor: Rumor,
    pub deliveries: Vec<DeviceDelivery>,
    pub invite_responses: Vec<OutgoingInviteResponseEnvelope>,
    pub relay_gaps: Vec<RelayGap>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceDelivery {
    pub owner_pubkey: OwnerPubkey,
    pub device_pubkey: DevicePubkey,
    pub device_id: Option<DeviceId>,
    pub envelope: OutgoingDirectMessageEnvelope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessedInviteResponse {
    pub owner_pubkey: OwnerPubkey,
    pub device_pubkey: DevicePubkey,
    pub device_id: Option<DeviceId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedDirectMessage {
    pub owner_pubkey: OwnerPubkey,
    pub device_pubkey: DevicePubkey,
    pub device_id: Option<DeviceId>,
    pub rumor: Rumor,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum RelayGap {
    MissingAppKeys {
        owner_pubkey: OwnerPubkey,
    },
    MissingDeviceInvite {
        owner_pubkey: OwnerPubkey,
        device_pubkey: DevicePubkey,
        device_id: Option<DeviceId>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PruneReport {
    pub removed_devices: Vec<(OwnerPubkey, DevicePubkey)>,
    pub removed_users: Vec<OwnerPubkey>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct TargetDevice {
    owner_pubkey: OwnerPubkey,
    device_pubkey: DevicePubkey,
}

enum SendSessionSource {
    Active,
    Inactive(usize),
}

impl SessionManager {
    pub fn new(
        local_owner_pubkey: OwnerPubkey,
        local_device_secret_key: [u8; 32],
        local_device_id: Option<DeviceId>,
        policy: SessionManagerPolicy,
    ) -> Self {
        let local_device_pubkey = crate::device_pubkey_from_secret_bytes(&local_device_secret_key)
            .expect("local device secret key must derive a valid device public key");
        Self {
            local_owner_pubkey,
            local_device_pubkey,
            local_device_secret_key,
            local_device_id,
            local_invite: None,
            users: BTreeMap::new(),
            policy,
        }
    }

    pub fn from_snapshot(
        snapshot: SessionManagerSnapshot,
        local_device_secret_key: [u8; 32],
        policy: SessionManagerPolicy,
    ) -> Result<Self> {
        let derived_local_device_pubkey =
            crate::device_pubkey_from_secret_bytes(&local_device_secret_key)?;
        if derived_local_device_pubkey != snapshot.local_device_pubkey {
            return Err(DomainError::InvalidState(
                "snapshot local device pubkey does not match provided secret key".to_string(),
            )
            .into());
        }

        let users = snapshot
            .users
            .into_iter()
            .map(UserRecord::from_snapshot)
            .map(|record| (record.owner_pubkey, record))
            .collect();

        Ok(Self {
            local_owner_pubkey: snapshot.local_owner_pubkey,
            local_device_pubkey: snapshot.local_device_pubkey,
            local_device_secret_key,
            local_device_id: snapshot.local_device_id,
            local_invite: snapshot.local_invite,
            users,
            policy,
        })
    }

    pub fn snapshot(&self) -> SessionManagerSnapshot {
        SessionManagerSnapshot {
            local_owner_pubkey: self.local_owner_pubkey,
            local_device_pubkey: self.local_device_pubkey,
            local_device_id: self.local_device_id.clone(),
            local_invite: self.local_invite.clone(),
            users: self.users.values().map(UserRecord::snapshot).collect(),
        }
    }

    pub fn ensure_local_device_invite<R>(
        &mut self,
        ctx: &mut ProtocolContext<'_, R>,
    ) -> Result<&Invite>
    where
        R: RngCore + CryptoRng,
    {
        if self.local_invite.is_none() {
            let mut invite = Invite::create_new(
                ctx,
                self.local_device_pubkey.as_owner(),
                self.local_device_id.clone(),
                None,
            )?;
            if self.local_owner_pubkey != self.local_device_pubkey.as_owner() {
                invite.owner_public_key = Some(self.local_owner_pubkey);
            }

            self.observe_public_invite(self.local_owner_pubkey, invite.clone())?;
            self.local_invite = Some(invite);
        }

        Ok(self.local_invite.as_ref().expect("local invite must exist"))
    }

    pub fn apply_local_app_keys(
        &mut self,
        app_keys: AppKeys,
        created_at: UnixSeconds,
    ) -> AppKeysSnapshotDecision {
        self.apply_app_keys_for_owner(self.local_owner_pubkey, app_keys, created_at)
    }

    pub fn observe_peer_app_keys(
        &mut self,
        owner_pubkey: OwnerPubkey,
        app_keys: AppKeys,
        created_at: UnixSeconds,
    ) -> AppKeysSnapshotDecision {
        self.apply_app_keys_for_owner(owner_pubkey, app_keys, created_at)
    }

    pub fn observe_device_invite(
        &mut self,
        owner_pubkey: OwnerPubkey,
        invite: Invite,
    ) -> Result<()> {
        self.observe_public_invite(owner_pubkey, invite)
    }

    pub fn observe_invite_response<R>(
        &mut self,
        ctx: &mut ProtocolContext<'_, R>,
        envelope: &IncomingInviteResponseEnvelope,
    ) -> Result<Option<ProcessedInviteResponse>>
    where
        R: RngCore + CryptoRng,
    {
        if self.local_invite.is_none() {
            return Ok(None);
        }

        let mut draft = self.clone();
        let invite = draft
            .local_invite
            .as_mut()
            .expect("local invite existence already checked");
        let Some(InviteResponse {
            session,
            invitee_identity,
            device_id,
            owner_public_key,
        }) = invite.process_invite_response(ctx, envelope, draft.local_device_secret_key)?
        else {
            return Ok(None);
        };

        let owner_pubkey = owner_public_key.unwrap_or_else(|| invitee_identity.as_owner());
        let user = draft.user_record_mut(owner_pubkey);
        let record = user.device_record_mut(invitee_identity, ctx.now_secs);
        if device_id.is_some() {
            record.device_id = device_id.clone();
        }
        record.upsert_session(session, ctx.now_secs);

        *self = draft;
        Ok(Some(ProcessedInviteResponse {
            owner_pubkey,
            device_pubkey: invitee_identity,
            device_id,
        }))
    }

    pub fn prepare_send_direct_message<R>(
        &mut self,
        ctx: &mut ProtocolContext<'_, R>,
        recipient_owner: OwnerPubkey,
        content: DirectMessageContent,
    ) -> Result<PreparedFanout>
    where
        R: RngCore + CryptoRng,
    {
        let rumor = Rumor::from_content(ctx, content)?;
        self.prepare_send_rumor(ctx, recipient_owner, rumor)
    }

    pub fn prepare_send_text<R>(
        &mut self,
        ctx: &mut ProtocolContext<'_, R>,
        recipient_owner: OwnerPubkey,
        text: String,
    ) -> Result<PreparedFanout>
    where
        R: RngCore + CryptoRng,
    {
        self.prepare_send_direct_message(ctx, recipient_owner, DirectMessageContent::Text(text))
    }

    pub fn receive_direct_message<R>(
        &mut self,
        ctx: &mut ProtocolContext<'_, R>,
        sender_owner: OwnerPubkey,
        envelope: &IncomingDirectMessageEnvelope,
    ) -> Result<Option<ReceivedDirectMessage>>
    where
        R: RngCore + CryptoRng,
    {
        let mut draft = self.clone();
        let Some(received) = draft.receive_inner(ctx, sender_owner, envelope)? else {
            return Ok(None);
        };
        *self = draft;
        Ok(Some(received))
    }

    pub fn prune_stale_records(&mut self, now: UnixSeconds) -> PruneReport {
        let mut removed_devices = Vec::new();
        let mut removed_users = Vec::new();
        let retention = self.policy.max_relay_latency.get();

        self.users.retain(|owner_pubkey, user| {
            user.devices.retain(|device_pubkey, record| {
                let keep = !record.is_stale
                    || record.stale_since.is_none_or(|stale_since| {
                        now.get().saturating_sub(stale_since.get()) <= retention
                    });
                if !keep {
                    removed_devices.push((*owner_pubkey, *device_pubkey));
                }
                keep
            });

            let keep_user = !user.devices.is_empty() || user.app_keys.is_some();
            if !keep_user {
                removed_users.push(*owner_pubkey);
            }
            keep_user
        });

        removed_devices.sort();
        removed_users.sort();

        PruneReport {
            removed_devices,
            removed_users,
        }
    }

    fn prepare_send_rumor<R>(
        &mut self,
        ctx: &mut ProtocolContext<'_, R>,
        recipient_owner: OwnerPubkey,
        rumor: Rumor,
    ) -> Result<PreparedFanout>
    where
        R: RngCore + CryptoRng,
    {
        let mut draft = self.clone();
        let prepared = draft.prepare_send_rumor_inner(ctx, recipient_owner, rumor)?;
        *self = draft;
        Ok(prepared)
    }

    fn prepare_send_rumor_inner<R>(
        &mut self,
        ctx: &mut ProtocolContext<'_, R>,
        recipient_owner: OwnerPubkey,
        rumor: Rumor,
    ) -> Result<PreparedFanout>
    where
        R: RngCore + CryptoRng,
    {
        let mut relay_gaps = Vec::new();
        let mut targets = BTreeSet::new();

        self.collect_recipient_targets(recipient_owner, &mut targets, &mut relay_gaps);
        self.collect_local_sibling_targets(&mut targets);

        let mut deliveries = Vec::new();
        let mut invite_responses = Vec::new();

        for target in targets {
            match self.prepare_device_delivery(
                ctx,
                target.owner_pubkey,
                target.device_pubkey,
                &rumor,
            )? {
                Some((delivery, invite_response)) => {
                    deliveries.push(delivery);
                    if let Some(invite_response) = invite_response {
                        invite_responses.push(invite_response);
                    }
                }
                None => {
                    let record = self
                        .users
                        .get(&target.owner_pubkey)
                        .and_then(|user| user.devices.get(&target.device_pubkey))
                        .expect("target device record must exist");
                    relay_gaps.push(RelayGap::MissingDeviceInvite {
                        owner_pubkey: target.owner_pubkey,
                        device_pubkey: target.device_pubkey,
                        device_id: record.device_id.clone(),
                    });
                }
            }
        }

        relay_gaps.sort();

        Ok(PreparedFanout {
            recipient_owner,
            rumor,
            deliveries,
            invite_responses,
            relay_gaps,
        })
    }

    fn receive_inner<R>(
        &mut self,
        ctx: &mut ProtocolContext<'_, R>,
        sender_owner: OwnerPubkey,
        envelope: &IncomingDirectMessageEnvelope,
    ) -> Result<Option<ReceivedDirectMessage>>
    where
        R: RngCore + CryptoRng,
    {
        let Some(user) = self.users.get_mut(&sender_owner) else {
            return Ok(None);
        };

        let device_pubkeys: Vec<DevicePubkey> = user.devices.keys().copied().collect();
        for device_pubkey in device_pubkeys {
            let record = user
                .devices
                .get_mut(&device_pubkey)
                .expect("device key collected from map");

            if let Some(active_session) = record.active_session.as_mut() {
                if active_session.matches_sender(envelope.sender) {
                    let plan = active_session.plan_receive(ctx, envelope)?;
                    let outcome = active_session.apply_receive(plan);
                    record.last_activity = Some(ctx.now_secs);
                    return Ok(Some(ReceivedDirectMessage {
                        owner_pubkey: sender_owner,
                        device_pubkey,
                        device_id: record.device_id.clone(),
                        rumor: outcome.rumor,
                    }));
                }
            }

            for index in 0..record.inactive_sessions.len() {
                if !record.inactive_sessions[index].matches_sender(envelope.sender) {
                    continue;
                }
                let plan = record.inactive_sessions[index].plan_receive(ctx, envelope)?;
                let mut session = record.inactive_sessions.remove(index);
                let outcome = session.apply_receive(plan);
                record.promote_inactive_session(session);
                record.last_activity = Some(ctx.now_secs);
                return Ok(Some(ReceivedDirectMessage {
                    owner_pubkey: sender_owner,
                    device_pubkey,
                    device_id: record.device_id.clone(),
                    rumor: outcome.rumor,
                }));
            }
        }

        Ok(None)
    }

    fn collect_recipient_targets(
        &self,
        recipient_owner: OwnerPubkey,
        targets: &mut BTreeSet<TargetDevice>,
        relay_gaps: &mut Vec<RelayGap>,
    ) {
        let Some(user) = self.users.get(&recipient_owner) else {
            relay_gaps.push(RelayGap::MissingAppKeys {
                owner_pubkey: recipient_owner,
            });
            return;
        };
        if user.app_keys.is_none() {
            relay_gaps.push(RelayGap::MissingAppKeys {
                owner_pubkey: recipient_owner,
            });
            return;
        }

        for device_pubkey in user.authorized_non_stale_devices() {
            targets.insert(TargetDevice {
                owner_pubkey: recipient_owner,
                device_pubkey,
            });
        }
    }

    fn collect_local_sibling_targets(&self, targets: &mut BTreeSet<TargetDevice>) {
        let Some(user) = self.users.get(&self.local_owner_pubkey) else {
            return;
        };
        if user.app_keys.is_none() {
            return;
        }

        for device_pubkey in user.authorized_non_stale_devices() {
            if device_pubkey == self.local_device_pubkey {
                continue;
            }
            targets.insert(TargetDevice {
                owner_pubkey: self.local_owner_pubkey,
                device_pubkey,
            });
        }
    }

    fn prepare_device_delivery<R>(
        &mut self,
        ctx: &mut ProtocolContext<'_, R>,
        owner_pubkey: OwnerPubkey,
        device_pubkey: DevicePubkey,
        rumor: &Rumor,
    ) -> Result<Option<(DeviceDelivery, Option<OutgoingInviteResponseEnvelope>)>>
    where
        R: RngCore + CryptoRng,
    {
        let local_owner_pubkey = self.local_owner_pubkey;
        let local_device_pubkey = self.local_device_pubkey;
        let local_device_secret_key = self.local_device_secret_key;
        let local_device_id = self.local_device_id.clone();
        let claimed_owner =
            (local_owner_pubkey != local_device_pubkey.as_owner()).then_some(local_owner_pubkey);
        let user = self.user_record_mut(owner_pubkey);
        let record = user.device_record_mut(device_pubkey, ctx.now_secs);

        if let Some(mut session) = record.take_best_send_session() {
            let outcome = session.apply_send(session.plan_send(rumor, ctx.now_secs)?);
            record.upsert_session(session, ctx.now_secs);
            return Ok(Some((
                DeviceDelivery {
                    owner_pubkey,
                    device_pubkey,
                    device_id: record.device_id.clone(),
                    envelope: outcome.envelope,
                },
                None,
            )));
        }

        let Some(public_invite) = record.public_invite.clone() else {
            return Ok(None);
        };
        let (mut session, invite_response) = public_invite.accept_with_owner(
            ctx,
            local_device_pubkey,
            local_device_secret_key,
            local_device_id,
            claimed_owner,
        )?;
        let outcome = session.apply_send(session.plan_send(rumor, ctx.now_secs)?);
        record.upsert_session(session, ctx.now_secs);
        Ok(Some((
            DeviceDelivery {
                owner_pubkey,
                device_pubkey,
                device_id: record.device_id.clone(),
                envelope: outcome.envelope,
            },
            Some(invite_response),
        )))
    }

    fn observe_public_invite(&mut self, owner_pubkey: OwnerPubkey, invite: Invite) -> Result<()> {
        let resolved_owner = invite.owner_public_key.unwrap_or(invite.inviter);
        if resolved_owner != owner_pubkey {
            return Err(DomainError::InvalidState(format!(
                "invite owner mismatch: expected {owner_pubkey}, got {resolved_owner}"
            ))
            .into());
        }

        let device_pubkey = invite.inviter.as_device();
        let mut public_invite = invite;
        public_invite.inviter_ephemeral_private_key = None;

        let user = self.user_record_mut(owner_pubkey);
        if user
            .app_keys
            .as_ref()
            .is_some_and(|app_keys| app_keys.get_device(&device_pubkey).is_none())
        {
            return Ok(());
        }
        let record = user.device_record_mut(device_pubkey, public_invite.created_at);

        let should_replace_invite = record
            .public_invite
            .as_ref()
            .is_none_or(|existing| public_invite.created_at >= existing.created_at);
        if should_replace_invite && public_invite.device_id.is_some() {
            record.device_id = public_invite.device_id.clone();
        } else if record.device_id.is_none() && public_invite.device_id.is_some() {
            record.device_id = public_invite.device_id.clone();
        }
        record.created_at = merge_created_at(record.created_at, public_invite.created_at);
        if should_replace_invite {
            record.public_invite = Some(public_invite);
        }
        Ok(())
    }

    fn apply_app_keys_for_owner(
        &mut self,
        owner_pubkey: OwnerPubkey,
        incoming_app_keys: AppKeys,
        incoming_created_at: UnixSeconds,
    ) -> AppKeysSnapshotDecision {
        let user = self.user_record_mut(owner_pubkey);
        let current_app_keys = user.app_keys.as_ref();
        let current_created_at = user.app_keys_created_at;
        let (decision, next_app_keys, next_created_at) = apply_app_keys_snapshot(
            current_app_keys,
            current_created_at,
            &incoming_app_keys,
            incoming_created_at,
        );

        let previous_authorized = current_app_keys
            .map(authorized_device_set)
            .unwrap_or_default();
        let next_authorized = authorized_device_set(&next_app_keys);

        user.app_keys = Some(next_app_keys.clone());
        user.app_keys_created_at = next_created_at;

        for device in next_app_keys.get_all_devices() {
            let record = user.device_record_mut(device.identity_pubkey, device.created_at);
            record.authorized = true;
            record.is_stale = false;
            record.stale_since = None;
            record.created_at = merge_created_at(record.created_at, device.created_at);
        }

        for removed in previous_authorized.difference(&next_authorized) {
            let record = user.device_record_mut(*removed, next_created_at);
            record.authorized = false;
            record.is_stale = true;
            if record.stale_since.is_none() {
                record.stale_since = Some(next_created_at);
            }
        }

        decision
    }

    fn user_record_mut(&mut self, owner_pubkey: OwnerPubkey) -> &mut UserRecord {
        self.users
            .entry(owner_pubkey)
            .or_insert_with(|| UserRecord::new(owner_pubkey))
    }
}

impl UserRecord {
    fn new(owner_pubkey: OwnerPubkey) -> Self {
        Self {
            owner_pubkey,
            app_keys: None,
            app_keys_created_at: UnixSeconds(0),
            devices: BTreeMap::new(),
        }
    }

    fn from_snapshot(snapshot: UserRecordSnapshot) -> Self {
        Self {
            owner_pubkey: snapshot.owner_pubkey,
            app_keys: snapshot.app_keys,
            app_keys_created_at: snapshot.app_keys_created_at,
            devices: snapshot
                .devices
                .into_iter()
                .map(DeviceRecord::from_snapshot)
                .map(|record| (record.device_pubkey, record))
                .collect(),
        }
    }

    fn snapshot(&self) -> UserRecordSnapshot {
        UserRecordSnapshot {
            owner_pubkey: self.owner_pubkey,
            app_keys: self.app_keys.clone(),
            app_keys_created_at: self.app_keys_created_at,
            devices: self.devices.values().map(DeviceRecord::snapshot).collect(),
        }
    }

    fn device_record_mut(
        &mut self,
        device_pubkey: DevicePubkey,
        created_at: UnixSeconds,
    ) -> &mut DeviceRecord {
        self.devices
            .entry(device_pubkey)
            .or_insert_with(|| DeviceRecord::new(device_pubkey, created_at))
    }

    fn authorized_non_stale_devices(&self) -> Vec<DevicePubkey> {
        self.devices
            .values()
            .filter(|record| record.authorized && !record.is_stale)
            .map(|record| record.device_pubkey)
            .collect()
    }
}

impl DeviceRecord {
    fn new(device_pubkey: DevicePubkey, created_at: UnixSeconds) -> Self {
        Self {
            device_pubkey,
            device_id: None,
            authorized: false,
            is_stale: false,
            stale_since: None,
            public_invite: None,
            active_session: None,
            inactive_sessions: Vec::new(),
            last_activity: None,
            created_at,
        }
    }

    fn from_snapshot(snapshot: DeviceRecordSnapshot) -> Self {
        Self {
            device_pubkey: snapshot.device_pubkey,
            device_id: snapshot.device_id,
            authorized: snapshot.authorized,
            is_stale: snapshot.is_stale,
            stale_since: snapshot.stale_since,
            public_invite: snapshot.public_invite,
            active_session: snapshot
                .active_session
                .map(|state| Session::new(state, "restored-active".to_string())),
            inactive_sessions: snapshot
                .inactive_sessions
                .into_iter()
                .enumerate()
                .map(|(index, state)| Session::new(state, format!("restored-inactive-{index}")))
                .collect(),
            last_activity: snapshot.last_activity,
            created_at: snapshot.created_at,
        }
    }

    fn snapshot(&self) -> DeviceRecordSnapshot {
        DeviceRecordSnapshot {
            device_pubkey: self.device_pubkey,
            device_id: self.device_id.clone(),
            authorized: self.authorized,
            is_stale: self.is_stale,
            stale_since: self.stale_since,
            public_invite: self.public_invite.clone(),
            active_session: self
                .active_session
                .as_ref()
                .map(|session| session.state.clone()),
            inactive_sessions: self
                .inactive_sessions
                .iter()
                .map(|session| session.state.clone())
                .collect(),
            last_activity: self.last_activity,
            created_at: self.created_at,
        }
    }

    fn take_best_send_session(&mut self) -> Option<Session> {
        match self.best_send_session_source()? {
            SendSessionSource::Active => self.active_session.take(),
            SendSessionSource::Inactive(index) => Some(self.inactive_sessions.remove(index)),
        }
    }

    fn best_send_session_source(&self) -> Option<SendSessionSource> {
        let mut best: Option<(SendSessionSource, (u8, u32, u32))> = None;

        if let Some(active_session) = self.active_session.as_ref() {
            if active_session.can_send() {
                best = Some((SendSessionSource::Active, session_priority(active_session)));
            }
        }

        for (index, session) in self.inactive_sessions.iter().enumerate() {
            if !session.can_send() {
                continue;
            }
            let priority = session_priority(session);
            if best
                .as_ref()
                .is_none_or(|(_, current_priority)| priority > *current_priority)
            {
                best = Some((SendSessionSource::Inactive(index), priority));
            }
        }

        best.map(|(source, _)| source)
    }

    fn upsert_session(&mut self, session: Session, now: UnixSeconds) {
        if self.contains_state(&session.state) {
            self.compact_duplicate_sessions();
            self.last_activity = Some(now);
            return;
        }

        let new_priority = session_priority(&session);
        let old_priority = self
            .active_session
            .as_ref()
            .map(session_priority)
            .unwrap_or((0, 0, 0));

        if let Some(old_active) = self.active_session.take() {
            if old_priority >= new_priority {
                self.inactive_sessions.push(session);
                self.active_session = Some(old_active);
            } else {
                self.inactive_sessions.push(old_active);
                self.active_session = Some(session);
            }
        } else {
            self.active_session = Some(session);
        }

        self.compact_duplicate_sessions();
        if self.inactive_sessions.len() > MAX_INACTIVE_SESSIONS {
            self.inactive_sessions.truncate(MAX_INACTIVE_SESSIONS);
        }
        self.last_activity = Some(now);
    }

    fn promote_inactive_session(&mut self, session: Session) {
        let new_priority = session_priority(&session);
        if let Some(old_active) = self.active_session.take() {
            let old_priority = session_priority(&old_active);
            if new_priority > old_priority {
                if old_active.state != session.state {
                    self.inactive_sessions.push(old_active);
                }
                self.active_session = Some(session);
            } else {
                self.inactive_sessions.push(session);
                self.active_session = Some(old_active);
            }
        } else {
            self.active_session = Some(session);
        }
        self.compact_duplicate_sessions();
        if self.inactive_sessions.len() > MAX_INACTIVE_SESSIONS {
            self.inactive_sessions.truncate(MAX_INACTIVE_SESSIONS);
        }
    }

    fn contains_state(&self, state: &SessionState) -> bool {
        self.active_session
            .as_ref()
            .is_some_and(|session| session.state == *state)
            || self
                .inactive_sessions
                .iter()
                .any(|session| session.state == *state)
    }

    fn compact_duplicate_sessions(&mut self) {
        let active_state = self
            .active_session
            .as_ref()
            .map(|session| session.state.clone());
        let mut unique_states = Vec::new();
        let mut inactive_sessions = Vec::with_capacity(self.inactive_sessions.len());

        for session in self.inactive_sessions.drain(..) {
            let is_duplicate = active_state
                .as_ref()
                .is_some_and(|state| *state == session.state)
                || unique_states
                    .iter()
                    .any(|state: &SessionState| *state == session.state);
            if is_duplicate {
                continue;
            }
            unique_states.push(session.state.clone());
            inactive_sessions.push(session);
        }

        self.inactive_sessions = inactive_sessions;
    }
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

fn authorized_device_set(app_keys: &AppKeys) -> BTreeSet<DevicePubkey> {
    app_keys
        .get_all_devices()
        .into_iter()
        .map(|device| device.identity_pubkey)
        .collect()
}

fn session_priority(session: &Session) -> (u8, u32, u32) {
    let can_send = session.can_send();
    let can_receive = session.state.receiving_chain_key.is_some()
        || session.state.their_current_nostr_public_key.is_some()
        || session.state.receiving_chain_message_number > 0;

    let directionality = match (can_send, can_receive) {
        (true, true) => 3,
        (true, false) => 2,
        (false, true) => 1,
        (false, false) => 0,
    };

    (
        directionality,
        session.state.receiving_chain_message_number,
        session.state.sending_chain_message_number,
    )
}

fn merge_created_at(current: UnixSeconds, observed: UnixSeconds) -> UnixSeconds {
    match (current.get(), observed.get()) {
        (0, _) => observed,
        (_, 0) => current,
        _ => current.min(observed),
    }
}
