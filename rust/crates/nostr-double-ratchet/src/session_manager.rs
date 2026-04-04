use crate::{
    DeviceId, DevicePubkey, DeviceRoster, DomainError, Invite, InviteResponse,
    InviteResponseEnvelope, MessageEnvelope, OwnerPubkey, ProtocolContext, Result,
    RosterSnapshotDecision, Session, SessionState, UnixSeconds,
};
use rand::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const MAX_INACTIVE_SESSIONS: usize = 10;
const DEFAULT_MAX_RELAY_LATENCY_SECS: u64 = 7 * 24 * 60 * 60;

#[derive(Debug, Clone)]
pub struct SessionManager {
    local_owner_pubkey: OwnerPubkey,
    local_device_pubkey: DevicePubkey,
    local_device_secret_key: [u8; 32],
    local_device_id: Option<DeviceId>,
    local_invite: Option<Invite>,
    users: BTreeMap<OwnerPubkey, UserRecord>,
    max_relay_latency: UnixSeconds,
}

#[derive(Debug, Clone)]
struct UserRecord {
    owner_pubkey: OwnerPubkey,
    roster: Option<DeviceRoster>,
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
    pub roster: Option<DeviceRoster>,
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
pub struct PreparedSend {
    pub recipient_owner: OwnerPubkey,
    pub payload: Vec<u8>,
    pub deliveries: Vec<Delivery>,
    pub invite_responses: Vec<InviteResponseEnvelope>,
    pub relay_gaps: Vec<RelayGap>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    pub owner_pubkey: OwnerPubkey,
    pub device_pubkey: DevicePubkey,
    pub device_id: Option<DeviceId>,
    pub envelope: MessageEnvelope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessedInviteResponse {
    pub owner_pubkey: OwnerPubkey,
    pub device_pubkey: DevicePubkey,
    pub device_id: Option<DeviceId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedMessage {
    pub owner_pubkey: OwnerPubkey,
    pub device_pubkey: DevicePubkey,
    pub device_id: Option<DeviceId>,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum RelayGap {
    MissingRoster {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendSessionSource {
    Active,
    Inactive(usize),
}

impl SessionManager {
    pub fn new(
        local_owner_pubkey: OwnerPubkey,
        local_device_secret_key: [u8; 32],
        local_device_id: Option<DeviceId>,
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
            max_relay_latency: UnixSeconds(DEFAULT_MAX_RELAY_LATENCY_SECS),
        }
    }

    pub fn from_snapshot(
        snapshot: SessionManagerSnapshot,
        local_device_secret_key: [u8; 32],
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
            max_relay_latency: UnixSeconds(DEFAULT_MAX_RELAY_LATENCY_SECS),
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

    pub fn ensure_local_invite<R>(&mut self, ctx: &mut ProtocolContext<'_, R>) -> Result<&Invite>
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

    pub fn apply_local_roster(&mut self, roster: DeviceRoster) -> RosterSnapshotDecision {
        self.apply_roster_for_owner(self.local_owner_pubkey, roster)
    }

    pub fn observe_peer_roster(
        &mut self,
        owner_pubkey: OwnerPubkey,
        roster: DeviceRoster,
    ) -> RosterSnapshotDecision {
        self.apply_roster_for_owner(owner_pubkey, roster)
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
        envelope: &InviteResponseEnvelope,
    ) -> Result<Option<ProcessedInviteResponse>>
    where
        R: RngCore + CryptoRng,
    {
        let Some(invite) = self.local_invite.clone() else {
            return Ok(None);
        };

        let mut owned_invite = invite;
        let InviteResponse {
            session,
            invitee_identity,
            device_id,
            owner_public_key,
        } = owned_invite.process_response(ctx, envelope, self.local_device_secret_key)?;

        self.local_invite = Some(owned_invite);

        let owner_pubkey = owner_public_key.unwrap_or_else(|| invitee_identity.as_owner());
        let user = self.user_record_mut(owner_pubkey);
        let record = user.device_record_mut(invitee_identity, ctx.now);
        if device_id.is_some() {
            record.device_id = device_id.clone();
        }
        record.upsert_session(session, ctx.now);

        Ok(Some(ProcessedInviteResponse {
            owner_pubkey,
            device_pubkey: invitee_identity,
            device_id,
        }))
    }

    pub fn prepare_send<R>(
        &mut self,
        ctx: &mut ProtocolContext<'_, R>,
        recipient_owner: OwnerPubkey,
        payload: Vec<u8>,
    ) -> Result<PreparedSend>
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
                &payload,
            )? {
                Some((delivery, maybe_response)) => {
                    deliveries.push(delivery);
                    if let Some(response) = maybe_response {
                        invite_responses.push(response);
                    }
                }
                None => {
                    let record = self
                        .users
                        .get(&target.owner_pubkey)
                        .and_then(|user| user.devices.get(&target.device_pubkey))
                        .expect("target record must exist");
                    relay_gaps.push(RelayGap::MissingDeviceInvite {
                        owner_pubkey: target.owner_pubkey,
                        device_pubkey: target.device_pubkey,
                        device_id: record.device_id.clone(),
                    });
                }
            }
        }

        relay_gaps.sort();

        Ok(PreparedSend {
            recipient_owner,
            payload,
            deliveries,
            invite_responses,
            relay_gaps,
        })
    }

    pub fn receive<R>(
        &mut self,
        ctx: &mut ProtocolContext<'_, R>,
        sender_owner: OwnerPubkey,
        envelope: &MessageEnvelope,
    ) -> Result<Option<ReceivedMessage>>
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

            if let Some(active_session) = record.active_session.as_ref() {
                if active_session.matches_sender(envelope.sender) {
                    let plan = active_session.plan_receive(ctx, envelope)?;
                    let outcome = record
                        .active_session
                        .as_mut()
                        .expect("active session must still exist")
                        .apply_receive(plan);
                    record.last_activity = Some(ctx.now);
                    return Ok(Some(ReceivedMessage {
                        owner_pubkey: sender_owner,
                        device_pubkey,
                        device_id: record.device_id.clone(),
                        payload: outcome.payload,
                    }));
                }
            }

            let mut matched_inactive = None;
            for (index, session) in record.inactive_sessions.iter().enumerate() {
                if !session.matches_sender(envelope.sender) {
                    continue;
                }
                let plan = session.plan_receive(ctx, envelope)?;
                matched_inactive = Some((index, plan));
                break;
            }

            if let Some((index, plan)) = matched_inactive {
                let mut session = record.inactive_sessions.remove(index);
                let outcome = session.apply_receive(plan);
                record.promote_inactive_session(session);
                record.last_activity = Some(ctx.now);
                return Ok(Some(ReceivedMessage {
                    owner_pubkey: sender_owner,
                    device_pubkey,
                    device_id: record.device_id.clone(),
                    payload: outcome.payload,
                }));
            }
        }

        Ok(None)
    }

    pub fn prune_stale(&mut self, now: UnixSeconds) -> PruneReport {
        let mut removed_devices = Vec::new();
        let mut removed_users = Vec::new();
        let retention = self.max_relay_latency.get();

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

            let keep_user = !user.devices.is_empty() || user.roster.is_some();
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

    fn prepare_device_delivery<R>(
        &mut self,
        ctx: &mut ProtocolContext<'_, R>,
        owner_pubkey: OwnerPubkey,
        device_pubkey: DevicePubkey,
        payload: &[u8],
    ) -> Result<Option<(Delivery, Option<InviteResponseEnvelope>)>>
    where
        R: RngCore + CryptoRng,
    {
        let claimed_owner = (self.local_owner_pubkey != self.local_device_pubkey.as_owner())
            .then_some(self.local_owner_pubkey);
        let local_device_pubkey = self.local_device_pubkey;
        let local_device_secret_key = self.local_device_secret_key;
        let local_device_id = self.local_device_id.clone();
        let user = self.user_record_mut(owner_pubkey);
        let record = user.device_record_mut(device_pubkey, ctx.now);

        if !record.authorized || record.is_stale {
            return Ok(None);
        }

        if let Some(source) = record.best_send_session_source() {
            let plan = match source {
                SendSessionSource::Active => record
                    .active_session
                    .as_ref()
                    .expect("active session must exist")
                    .plan_send(payload, ctx.now)?,
                SendSessionSource::Inactive(index) => {
                    record.inactive_sessions[index].plan_send(payload, ctx.now)?
                }
            };

            let envelope = match source {
                SendSessionSource::Active => {
                    record
                        .active_session
                        .as_mut()
                        .expect("active session must exist")
                        .apply_send(plan)
                        .envelope
                }
                SendSessionSource::Inactive(index) => {
                    let mut session = record.inactive_sessions.remove(index);
                    let outcome = session.apply_send(plan);
                    record.upsert_session(session, ctx.now);
                    outcome.envelope
                }
            };

            record.last_activity = Some(ctx.now);
            return Ok(Some((
                Delivery {
                    owner_pubkey,
                    device_pubkey,
                    device_id: record.device_id.clone(),
                    envelope,
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
        let envelope = session
            .apply_send(session.plan_send(payload, ctx.now)?)
            .envelope;
        record.upsert_session(session, ctx.now);

        Ok(Some((
            Delivery {
                owner_pubkey,
                device_pubkey,
                device_id: record.device_id.clone(),
                envelope,
            },
            Some(invite_response),
        )))
    }

    fn collect_recipient_targets(
        &self,
        recipient_owner: OwnerPubkey,
        targets: &mut BTreeSet<TargetDevice>,
        relay_gaps: &mut Vec<RelayGap>,
    ) {
        let Some(user) = self.users.get(&recipient_owner) else {
            relay_gaps.push(RelayGap::MissingRoster {
                owner_pubkey: recipient_owner,
            });
            return;
        };

        if user.roster.is_none() {
            relay_gaps.push(RelayGap::MissingRoster {
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

        if user.roster.is_none() {
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
        let record = user.device_record_mut(device_pubkey, public_invite.created_at);

        let should_replace_invite = record
            .public_invite
            .as_ref()
            .is_none_or(|existing| public_invite.created_at >= existing.created_at);

        if public_invite.device_id.is_some()
            && (should_replace_invite || record.device_id.is_none())
        {
            record.device_id = public_invite.device_id.clone();
        }

        record.created_at = merge_created_at(record.created_at, public_invite.created_at);
        if should_replace_invite {
            record.public_invite = Some(public_invite);
        }
        Ok(())
    }

    fn apply_roster_for_owner(
        &mut self,
        owner_pubkey: OwnerPubkey,
        incoming_roster: DeviceRoster,
    ) -> RosterSnapshotDecision {
        let user = self.user_record_mut(owner_pubkey);
        let current_roster = user.roster.as_ref();
        let (decision, next_roster) = apply_roster_snapshot(current_roster, &incoming_roster);

        let previous_authorized = current_roster
            .map(authorized_device_set)
            .unwrap_or_default();
        let next_authorized = authorized_device_set(&next_roster);

        user.roster = Some(next_roster.clone());

        for device in next_roster.devices() {
            let record = user.device_record_mut(device.device_pubkey, device.created_at);
            record.authorized = true;
            record.is_stale = false;
            record.stale_since = None;
            record.created_at = merge_created_at(record.created_at, device.created_at);
        }

        for removed in previous_authorized.difference(&next_authorized) {
            let record = user.device_record_mut(*removed, next_roster.created_at);
            record.authorized = false;
            record.is_stale = true;
            if record.stale_since.is_none() {
                record.stale_since = Some(next_roster.created_at);
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
            roster: None,
            devices: BTreeMap::new(),
        }
    }

    fn from_snapshot(snapshot: UserRecordSnapshot) -> Self {
        Self {
            owner_pubkey: snapshot.owner_pubkey,
            roster: snapshot.roster,
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
            roster: self.roster.clone(),
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
            active_session: snapshot.active_session.map(Session::new),
            inactive_sessions: snapshot
                .inactive_sessions
                .into_iter()
                .map(Session::new)
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
                || unique_states.contains(&session.state);
            if is_duplicate {
                continue;
            }
            unique_states.push(session.state.clone());
            inactive_sessions.push(session);
        }

        self.inactive_sessions = inactive_sessions;
    }
}

fn apply_roster_snapshot(
    current_roster: Option<&DeviceRoster>,
    incoming_roster: &DeviceRoster,
) -> (RosterSnapshotDecision, DeviceRoster) {
    let Some(current_roster) = current_roster else {
        return (RosterSnapshotDecision::Advanced, incoming_roster.clone());
    };

    if incoming_roster.created_at > current_roster.created_at {
        return (RosterSnapshotDecision::Advanced, incoming_roster.clone());
    }

    if incoming_roster.created_at < current_roster.created_at {
        return (RosterSnapshotDecision::Stale, current_roster.clone());
    }

    (
        RosterSnapshotDecision::MergedEqualTimestamp,
        current_roster.merge(incoming_roster),
    )
}

fn authorized_device_set(roster: &DeviceRoster) -> BTreeSet<DevicePubkey> {
    roster
        .devices()
        .iter()
        .map(|device| device.device_pubkey)
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
