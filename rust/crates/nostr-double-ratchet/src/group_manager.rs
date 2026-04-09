use crate::{
    DomainError, GroupCreateResult, GroupIncomingEvent, GroupManagerSnapshot, GroupPreparedSend,
    GroupReceivedMessage, GroupSnapshot, OwnerPubkey, ProtocolContext, Result, SessionManager,
    UnixSeconds,
};
use rand::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct GroupManager {
    local_owner_pubkey: OwnerPubkey,
    groups: BTreeMap<String, GroupRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GroupRecord {
    group_id: String,
    name: String,
    created_by: OwnerPubkey,
    members: BTreeSet<OwnerPubkey>,
    admins: BTreeSet<OwnerPubkey>,
    revision: u64,
    created_at: UnixSeconds,
    updated_at: UnixSeconds,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GroupWireEnvelopeV1 {
    version: u8,
    payload: GroupPairwisePayloadV1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum GroupPairwisePayloadV1 {
    CreateGroup {
        group_id: String,
        base_revision: u64,
        new_revision: u64,
        name: String,
        created_by: OwnerPubkey,
        members: Vec<OwnerPubkey>,
        admins: Vec<OwnerPubkey>,
        created_at: UnixSeconds,
        updated_at: UnixSeconds,
    },
    RenameGroup {
        group_id: String,
        base_revision: u64,
        new_revision: u64,
        name: String,
    },
    AddMembers {
        group_id: String,
        base_revision: u64,
        new_revision: u64,
        members: Vec<OwnerPubkey>,
    },
    RemoveMembers {
        group_id: String,
        base_revision: u64,
        new_revision: u64,
        members: Vec<OwnerPubkey>,
    },
    AddAdmins {
        group_id: String,
        base_revision: u64,
        new_revision: u64,
        admins: Vec<OwnerPubkey>,
    },
    RemoveAdmins {
        group_id: String,
        base_revision: u64,
        new_revision: u64,
        admins: Vec<OwnerPubkey>,
    },
    GroupMessage {
        group_id: String,
        revision: u64,
        body: Vec<u8>,
    },
}

impl GroupManager {
    pub fn new(local_owner_pubkey: OwnerPubkey) -> Self {
        Self {
            local_owner_pubkey,
            groups: BTreeMap::new(),
        }
    }

    pub fn from_snapshot(snapshot: GroupManagerSnapshot) -> Result<Self> {
        let mut groups = BTreeMap::new();
        for group in snapshot.groups {
            let record = GroupRecord::from_snapshot(group)?;
            if groups.insert(record.group_id.clone(), record).is_some() {
                return Err(group_error("duplicate group id in snapshot"));
            }
        }
        Ok(Self {
            local_owner_pubkey: snapshot.local_owner_pubkey,
            groups,
        })
    }

    pub fn snapshot(&self) -> GroupManagerSnapshot {
        GroupManagerSnapshot {
            local_owner_pubkey: self.local_owner_pubkey,
            groups: self.groups.values().map(GroupRecord::snapshot).collect(),
        }
    }

    pub fn group(&self, group_id: &str) -> Option<GroupSnapshot> {
        self.groups.get(group_id).map(GroupRecord::snapshot)
    }

    pub fn groups(&self) -> Vec<GroupSnapshot> {
        self.groups.values().map(GroupRecord::snapshot).collect()
    }

    pub fn create_group<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        name: String,
        initial_members: Vec<OwnerPubkey>,
    ) -> Result<GroupCreateResult>
    where
        R: RngCore + CryptoRng,
    {
        let member_set = validate_unique_owners(&initial_members, "initial members")?;
        if member_set.contains(&self.local_owner_pubkey) {
            return Err(group_error("local owner is added automatically"));
        }

        let group_id = random_group_id(ctx);
        let mut members = member_set;
        members.insert(self.local_owner_pubkey);

        let mut admins = BTreeSet::new();
        admins.insert(self.local_owner_pubkey);

        let record = GroupRecord {
            group_id: group_id.clone(),
            name,
            created_by: self.local_owner_pubkey,
            members,
            admins,
            revision: 1,
            created_at: ctx.now,
            updated_at: ctx.now,
        };
        let payload = record.create_payload();
        let recipients = record.remote_members(self.local_owner_pubkey);
        let prepared = self.fanout_payload(session_manager, ctx, &group_id, recipients, &payload)?;
        let snapshot = record.snapshot();

        self.groups.insert(group_id, record);

        Ok(GroupCreateResult {
            group: snapshot,
            prepared,
        })
    }

    pub fn retry_create_group<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        group_id: &str,
        recipients: Vec<OwnerPubkey>,
    ) -> Result<GroupPreparedSend>
    where
        R: RngCore + CryptoRng,
    {
        let record = self.group_record(group_id)?.clone();
        record.ensure_admin(self.local_owner_pubkey)?;

        let recipients = validate_unique_owners(&recipients, "recipients")?
            .into_iter()
            .filter(|owner| *owner != self.local_owner_pubkey)
            .collect::<Vec<_>>();
        for recipient in &recipients {
            record.ensure_member(*recipient)?;
        }

        self.fanout_payload(
            session_manager,
            ctx,
            &record.group_id,
            recipients,
            &record.create_payload(),
        )
    }

    pub fn send_message<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        group_id: &str,
        body: Vec<u8>,
    ) -> Result<GroupPreparedSend>
    where
        R: RngCore + CryptoRng,
    {
        let record = self.group_record(group_id)?.clone();
        record.ensure_member(self.local_owner_pubkey)?;

        let payload = GroupPairwisePayloadV1::GroupMessage {
            group_id: record.group_id.clone(),
            revision: record.revision,
            body,
        };

        self.fanout_payload(
            session_manager,
            ctx,
            &record.group_id,
            record.remote_members(self.local_owner_pubkey),
            &payload,
        )
    }

    pub fn update_name<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        group_id: &str,
        name: String,
    ) -> Result<GroupPreparedSend>
    where
        R: RngCore + CryptoRng,
    {
        let current = self.group_record(group_id)?.clone();
        let mut next = current.clone();
        next.apply_rename(self.local_owner_pubkey, name.clone(), current.revision, current.revision + 1, ctx.now)?;

        let payload = GroupPairwisePayloadV1::RenameGroup {
            group_id: current.group_id.clone(),
            base_revision: current.revision,
            new_revision: next.revision,
            name,
        };

        let prepared = self.fanout_payload(
            session_manager,
            ctx,
            &current.group_id,
            next.remote_members(self.local_owner_pubkey),
            &payload,
        )?;
        self.groups.insert(current.group_id.clone(), next);
        Ok(prepared)
    }

    pub fn retry_update_name<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        group_id: &str,
    ) -> Result<GroupPreparedSend>
    where
        R: RngCore + CryptoRng,
    {
        let current = self.group_record(group_id)?.clone();
        current.ensure_admin(self.local_owner_pubkey)?;
        let (base_revision, new_revision) = current.retry_delta_revisions("rename")?;

        let payload = GroupPairwisePayloadV1::RenameGroup {
            group_id: current.group_id.clone(),
            base_revision,
            new_revision,
            name: current.name.clone(),
        };

        self.fanout_payload(
            session_manager,
            ctx,
            &current.group_id,
            current.remote_members(self.local_owner_pubkey),
            &payload,
        )
    }

    pub fn add_members<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        group_id: &str,
        members: Vec<OwnerPubkey>,
    ) -> Result<GroupPreparedSend>
    where
        R: RngCore + CryptoRng,
    {
        let additions = validate_unique_owners(&members, "members")?;
        let current = self.group_record(group_id)?.clone();
        let mut next = current.clone();
        next.apply_add_members(
            self.local_owner_pubkey,
            &additions,
            current.revision,
            current.revision + 1,
            ctx.now,
        )?;

        let delta_payload = GroupPairwisePayloadV1::AddMembers {
            group_id: current.group_id.clone(),
            base_revision: current.revision,
            new_revision: next.revision,
            members: additions.iter().copied().collect(),
        };
        let bootstrap_payload = next.create_payload();

        let existing_recipients: Vec<_> = current
            .remote_members(self.local_owner_pubkey)
            .into_iter()
            .filter(|owner| !additions.contains(owner))
            .collect();
        let new_recipients: Vec<_> = additions
            .iter()
            .copied()
            .filter(|owner| *owner != self.local_owner_pubkey)
            .collect();

        let mut prepared = self.fanout_payload(
            session_manager,
            ctx,
            &current.group_id,
            existing_recipients,
            &delta_payload,
        )?;
        let bootstrapped = self.fanout_payload(
            session_manager,
            ctx,
            &current.group_id,
            new_recipients,
            &bootstrap_payload,
        )?;
        merge_group_prepared(&mut prepared, bootstrapped);

        self.groups.insert(current.group_id.clone(), next);
        Ok(prepared)
    }

    pub fn retry_add_members<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        group_id: &str,
        members: Vec<OwnerPubkey>,
    ) -> Result<GroupPreparedSend>
    where
        R: RngCore + CryptoRng,
    {
        let additions = validate_unique_owners(&members, "members")?;
        let current = self.group_record(group_id)?.clone();
        current.ensure_admin(self.local_owner_pubkey)?;
        let (base_revision, new_revision) = current.retry_delta_revisions("add members")?;
        for owner in &additions {
            current.ensure_member(*owner)?;
        }

        let delta_payload = GroupPairwisePayloadV1::AddMembers {
            group_id: current.group_id.clone(),
            base_revision,
            new_revision,
            members: additions.iter().copied().collect(),
        };
        let bootstrap_payload = current.create_payload();

        let existing_recipients: Vec<_> = current
            .remote_members(self.local_owner_pubkey)
            .into_iter()
            .filter(|owner| !additions.contains(owner))
            .collect();
        let new_recipients: Vec<_> = additions
            .iter()
            .copied()
            .filter(|owner| *owner != self.local_owner_pubkey)
            .collect();

        let mut prepared = self.fanout_payload(
            session_manager,
            ctx,
            &current.group_id,
            existing_recipients,
            &delta_payload,
        )?;
        let bootstrapped = self.fanout_payload(
            session_manager,
            ctx,
            &current.group_id,
            new_recipients,
            &bootstrap_payload,
        )?;
        merge_group_prepared(&mut prepared, bootstrapped);
        Ok(prepared)
    }

    pub fn remove_members<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        group_id: &str,
        members: Vec<OwnerPubkey>,
    ) -> Result<GroupPreparedSend>
    where
        R: RngCore + CryptoRng,
    {
        let removals = validate_unique_owners(&members, "members")?;
        let current = self.group_record(group_id)?.clone();
        let mut next = current.clone();
        next.apply_remove_members(
            self.local_owner_pubkey,
            &removals,
            current.revision,
            current.revision + 1,
            ctx.now,
        )?;

        let payload = GroupPairwisePayloadV1::RemoveMembers {
            group_id: current.group_id.clone(),
            base_revision: current.revision,
            new_revision: next.revision,
            members: removals.iter().copied().collect(),
        };

        let prepared = self.fanout_payload(
            session_manager,
            ctx,
            &current.group_id,
            current.remote_members(self.local_owner_pubkey),
            &payload,
        )?;
        self.groups.insert(current.group_id.clone(), next);
        Ok(prepared)
    }

    pub fn retry_remove_members<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        group_id: &str,
        members: Vec<OwnerPubkey>,
    ) -> Result<GroupPreparedSend>
    where
        R: RngCore + CryptoRng,
    {
        let removals = validate_unique_owners(&members, "members")?;
        let current = self.group_record(group_id)?.clone();
        current.ensure_admin(self.local_owner_pubkey)?;
        let (base_revision, new_revision) = current.retry_delta_revisions("remove members")?;
        for owner in &removals {
            if current.members.contains(owner) {
                return Err(group_error(format!(
                    "owner {owner} should already be removed before retrying removal"
                )));
            }
        }

        let payload = GroupPairwisePayloadV1::RemoveMembers {
            group_id: current.group_id.clone(),
            base_revision,
            new_revision,
            members: removals.iter().copied().collect(),
        };

        let mut recipients = current
            .remote_members(self.local_owner_pubkey)
            .into_iter()
            .collect::<BTreeSet<_>>();
        recipients.extend(
            removals
                .iter()
                .copied()
                .filter(|owner| *owner != self.local_owner_pubkey),
        );

        self.fanout_payload(
            session_manager,
            ctx,
            &current.group_id,
            recipients.into_iter().collect(),
            &payload,
        )
    }

    pub fn add_admins<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        group_id: &str,
        admins: Vec<OwnerPubkey>,
    ) -> Result<GroupPreparedSend>
    where
        R: RngCore + CryptoRng,
    {
        let additions = validate_unique_owners(&admins, "admins")?;
        let current = self.group_record(group_id)?.clone();
        let mut next = current.clone();
        next.apply_add_admins(
            self.local_owner_pubkey,
            &additions,
            current.revision,
            current.revision + 1,
            ctx.now,
        )?;

        let payload = GroupPairwisePayloadV1::AddAdmins {
            group_id: current.group_id.clone(),
            base_revision: current.revision,
            new_revision: next.revision,
            admins: additions.iter().copied().collect(),
        };

        let prepared = self.fanout_payload(
            session_manager,
            ctx,
            &current.group_id,
            next.remote_members(self.local_owner_pubkey),
            &payload,
        )?;
        self.groups.insert(current.group_id.clone(), next);
        Ok(prepared)
    }

    pub fn remove_admins<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        group_id: &str,
        admins: Vec<OwnerPubkey>,
    ) -> Result<GroupPreparedSend>
    where
        R: RngCore + CryptoRng,
    {
        let removals = validate_unique_owners(&admins, "admins")?;
        let current = self.group_record(group_id)?.clone();
        let mut next = current.clone();
        next.apply_remove_admins(
            self.local_owner_pubkey,
            &removals,
            current.revision,
            current.revision + 1,
            ctx.now,
        )?;

        let payload = GroupPairwisePayloadV1::RemoveAdmins {
            group_id: current.group_id.clone(),
            base_revision: current.revision,
            new_revision: next.revision,
            admins: removals.iter().copied().collect(),
        };

        let prepared = self.fanout_payload(
            session_manager,
            ctx,
            &current.group_id,
            next.remote_members(self.local_owner_pubkey),
            &payload,
        )?;
        self.groups.insert(current.group_id.clone(), next);
        Ok(prepared)
    }

    pub fn handle_incoming(
        &mut self,
        sender_owner: OwnerPubkey,
        payload: &[u8],
    ) -> Result<Option<GroupIncomingEvent>> {
        let Ok(envelope) = serde_json::from_slice::<GroupWireEnvelopeV1>(payload) else {
            return Ok(None);
        };
        if envelope.version != 1 {
            return Ok(None);
        }

        let event = match envelope.payload {
            GroupPairwisePayloadV1::CreateGroup {
                group_id,
                base_revision,
                new_revision,
                name,
                created_by,
                members,
                admins,
                created_at,
                updated_at,
            } => {
                let record = GroupRecord::from_create_payload(
                    group_id,
                    name,
                    created_by,
                    members,
                    admins,
                    new_revision,
                    created_at,
                    updated_at,
                    sender_owner,
                )?;
                if let Some(existing) = self.groups.get(&record.group_id) {
                    if existing == &record {
                        GroupIncomingEvent::MetadataUpdated(existing.snapshot())
                    } else {
                        return Err(group_error(format!("group `{}` already exists", record.group_id)));
                    }
                } else {
                    if base_revision != 0 {
                        return Err(group_error("create group base revision must be 0"));
                    }
                    let snapshot = record.snapshot();
                    self.groups.insert(record.group_id.clone(), record);
                    GroupIncomingEvent::MetadataUpdated(snapshot)
                }
            }
            GroupPairwisePayloadV1::RenameGroup {
                group_id,
                base_revision,
                new_revision,
                name,
            } => {
                let group = self.group_record_mut(&group_id)?;
                if group.reflects_rename(&name, new_revision) {
                    GroupIncomingEvent::MetadataUpdated(group.snapshot())
                } else {
                    group.apply_rename(sender_owner, name, base_revision, new_revision, group.updated_at)?;
                    GroupIncomingEvent::MetadataUpdated(group.snapshot())
                }
            }
            GroupPairwisePayloadV1::AddMembers {
                group_id,
                base_revision,
                new_revision,
                members,
            } => {
                let additions = validate_unique_owners(&members, "members")?;
                let group = self.group_record_mut(&group_id)?;
                if group.reflects_added_members(&additions, new_revision) {
                    GroupIncomingEvent::MetadataUpdated(group.snapshot())
                } else {
                    group.apply_add_members(sender_owner, &additions, base_revision, new_revision, group.updated_at)?;
                    GroupIncomingEvent::MetadataUpdated(group.snapshot())
                }
            }
            GroupPairwisePayloadV1::RemoveMembers {
                group_id,
                base_revision,
                new_revision,
                members,
            } => {
                let removals = validate_unique_owners(&members, "members")?;
                let group = self.group_record_mut(&group_id)?;
                if group.reflects_removed_members(&removals, new_revision) {
                    GroupIncomingEvent::MetadataUpdated(group.snapshot())
                } else {
                    group.apply_remove_members(sender_owner, &removals, base_revision, new_revision, group.updated_at)?;
                    GroupIncomingEvent::MetadataUpdated(group.snapshot())
                }
            }
            GroupPairwisePayloadV1::AddAdmins {
                group_id,
                base_revision,
                new_revision,
                admins,
            } => {
                let additions = validate_unique_owners(&admins, "admins")?;
                let group = self.group_record_mut(&group_id)?;
                if group.reflects_added_admins(&additions, new_revision) {
                    GroupIncomingEvent::MetadataUpdated(group.snapshot())
                } else {
                    group.apply_add_admins(sender_owner, &additions, base_revision, new_revision, group.updated_at)?;
                    GroupIncomingEvent::MetadataUpdated(group.snapshot())
                }
            }
            GroupPairwisePayloadV1::RemoveAdmins {
                group_id,
                base_revision,
                new_revision,
                admins,
            } => {
                let removals = validate_unique_owners(&admins, "admins")?;
                let group = self.group_record_mut(&group_id)?;
                if group.reflects_removed_admins(&removals, new_revision) {
                    GroupIncomingEvent::MetadataUpdated(group.snapshot())
                } else {
                    group.apply_remove_admins(sender_owner, &removals, base_revision, new_revision, group.updated_at)?;
                    GroupIncomingEvent::MetadataUpdated(group.snapshot())
                }
            }
            GroupPairwisePayloadV1::GroupMessage {
                group_id,
                revision,
                body,
            } => {
                let group = self.group_record(&group_id)?;
                group.ensure_member(sender_owner)?;
                if revision != group.revision {
                    return Err(group_error(format!(
                        "group message revision mismatch for `{group_id}`: expected {}, got {}",
                        group.revision, revision
                    )));
                }
                GroupIncomingEvent::Message(GroupReceivedMessage {
                    group_id,
                    sender_owner,
                    body,
                    revision,
                })
            }
        };

        Ok(Some(event))
    }

    fn fanout_payload<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        group_id: &str,
        recipients: Vec<OwnerPubkey>,
        payload: &GroupPairwisePayloadV1,
    ) -> Result<GroupPreparedSend>
    where
        R: RngCore + CryptoRng,
    {
        let mut prepared = GroupPreparedSend {
            group_id: group_id.to_string(),
            deliveries: Vec::new(),
            invite_responses: Vec::new(),
            relay_gaps: Vec::new(),
        };
        let payload_bytes = serde_json::to_vec(&GroupWireEnvelopeV1 {
            version: 1,
            payload: payload.clone(),
        })?;

        for recipient in recipients {
            let next = session_manager.prepare_send(ctx, recipient, payload_bytes.clone())?;
            prepared.deliveries.extend(next.deliveries);
            prepared.invite_responses.extend(next.invite_responses);
            prepared.relay_gaps.extend(next.relay_gaps);
        }

        prepared.relay_gaps.sort();
        prepared.relay_gaps.dedup();
        Ok(prepared)
    }

    fn group_record(&self, group_id: &str) -> Result<&GroupRecord> {
        self.groups
            .get(group_id)
            .ok_or_else(|| group_error(format!("unknown group `{group_id}`")))
    }

    fn group_record_mut(&mut self, group_id: &str) -> Result<&mut GroupRecord> {
        self.groups
            .get_mut(group_id)
            .ok_or_else(|| group_error(format!("unknown group `{group_id}`")))
    }
}

impl GroupRecord {
    fn from_snapshot(snapshot: GroupSnapshot) -> Result<Self> {
        let members = validate_unique_owners(&snapshot.members, "members")?;
        let admins = validate_unique_owners(&snapshot.admins, "admins")?;
        validate_group_invariants(&members, &admins)?;

        Ok(Self {
            group_id: snapshot.group_id,
            name: snapshot.name,
            created_by: snapshot.created_by,
            members,
            admins,
            revision: snapshot.revision,
            created_at: snapshot.created_at,
            updated_at: snapshot.updated_at,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn from_create_payload(
        group_id: String,
        name: String,
        created_by: OwnerPubkey,
        members: Vec<OwnerPubkey>,
        admins: Vec<OwnerPubkey>,
        new_revision: u64,
        created_at: UnixSeconds,
        updated_at: UnixSeconds,
        sender_owner: OwnerPubkey,
    ) -> Result<Self> {
        let member_set = validate_unique_owners(&members, "members")?;
        let admin_set = validate_unique_owners(&admins, "admins")?;
        if created_by != sender_owner {
            return Err(group_error("create group sender must match created_by"));
        }
        if new_revision == 0 {
            return Err(group_error("create group revision must be at least 1"));
        }
        if !admin_set.contains(&sender_owner) {
            return Err(group_error("create group sender must be an admin"));
        }
        validate_group_invariants(&member_set, &admin_set)?;

        Ok(Self {
            group_id,
            name,
            created_by,
            members: member_set,
            admins: admin_set,
            revision: new_revision,
            created_at,
            updated_at,
        })
    }

    fn snapshot(&self) -> GroupSnapshot {
        GroupSnapshot {
            group_id: self.group_id.clone(),
            name: self.name.clone(),
            created_by: self.created_by,
            members: self.members.iter().copied().collect(),
            admins: self.admins.iter().copied().collect(),
            revision: self.revision,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }

    fn create_payload(&self) -> GroupPairwisePayloadV1 {
        GroupPairwisePayloadV1::CreateGroup {
            group_id: self.group_id.clone(),
            base_revision: 0,
            new_revision: self.revision,
            name: self.name.clone(),
            created_by: self.created_by,
            members: self.members.iter().copied().collect(),
            admins: self.admins.iter().copied().collect(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }

    fn remote_members(&self, local_owner_pubkey: OwnerPubkey) -> Vec<OwnerPubkey> {
        self.members
            .iter()
            .copied()
            .filter(|owner| *owner != local_owner_pubkey)
            .collect()
    }

    fn ensure_admin(&self, owner: OwnerPubkey) -> Result<()> {
        if !self.admins.contains(&owner) {
            return Err(group_error(format!(
                "owner {owner} is not an admin of group `{}`",
                self.group_id
            )));
        }
        Ok(())
    }

    fn ensure_member(&self, owner: OwnerPubkey) -> Result<()> {
        if !self.members.contains(&owner) {
            return Err(group_error(format!(
                "owner {owner} is not a member of group `{}`",
                self.group_id
            )));
        }
        Ok(())
    }

    fn ensure_revision(&self, base_revision: u64, new_revision: u64) -> Result<()> {
        if base_revision != self.revision {
            return Err(group_error(format!(
                "stale group revision for `{}`: expected {}, got {}",
                self.group_id, self.revision, base_revision
            )));
        }
        if new_revision != base_revision + 1 {
            return Err(group_error(format!(
                "invalid next revision for `{}`: expected {}, got {}",
                self.group_id,
                base_revision + 1,
                new_revision
            )));
        }
        Ok(())
    }

    fn retry_delta_revisions(&self, action: &str) -> Result<(u64, u64)> {
        if self.revision < 2 {
            return Err(group_error(format!(
                "{action} retry requires an already-applied revision"
            )));
        }
        Ok((self.revision - 1, self.revision))
    }

    fn reflects_rename(&self, name: &str, new_revision: u64) -> bool {
        self.revision >= new_revision && self.name == name
    }

    fn reflects_added_members(
        &self,
        additions: &BTreeSet<OwnerPubkey>,
        new_revision: u64,
    ) -> bool {
        self.revision >= new_revision && additions.iter().all(|owner| self.members.contains(owner))
    }

    fn reflects_removed_members(
        &self,
        removals: &BTreeSet<OwnerPubkey>,
        new_revision: u64,
    ) -> bool {
        self.revision >= new_revision && removals.iter().all(|owner| !self.members.contains(owner))
    }

    fn reflects_added_admins(
        &self,
        additions: &BTreeSet<OwnerPubkey>,
        new_revision: u64,
    ) -> bool {
        self.revision >= new_revision && additions.iter().all(|owner| self.admins.contains(owner))
    }

    fn reflects_removed_admins(
        &self,
        removals: &BTreeSet<OwnerPubkey>,
        new_revision: u64,
    ) -> bool {
        self.revision >= new_revision && removals.iter().all(|owner| !self.admins.contains(owner))
    }

    fn apply_rename(
        &mut self,
        actor: OwnerPubkey,
        name: String,
        base_revision: u64,
        new_revision: u64,
        updated_at: UnixSeconds,
    ) -> Result<()> {
        self.ensure_admin(actor)?;
        self.ensure_revision(base_revision, new_revision)?;
        self.name = name;
        self.revision = new_revision;
        self.updated_at = updated_at;
        Ok(())
    }

    fn apply_add_members(
        &mut self,
        actor: OwnerPubkey,
        additions: &BTreeSet<OwnerPubkey>,
        base_revision: u64,
        new_revision: u64,
        updated_at: UnixSeconds,
    ) -> Result<()> {
        self.ensure_admin(actor)?;
        self.ensure_revision(base_revision, new_revision)?;
        if additions.is_empty() {
            return Err(group_error("members list must not be empty"));
        }
        for owner in additions {
            if self.members.contains(owner) {
                return Err(group_error(format!("owner {owner} is already a member")));
            }
        }
        self.members.extend(additions.iter().copied());
        self.revision = new_revision;
        self.updated_at = updated_at;
        Ok(())
    }

    fn apply_remove_members(
        &mut self,
        actor: OwnerPubkey,
        removals: &BTreeSet<OwnerPubkey>,
        base_revision: u64,
        new_revision: u64,
        updated_at: UnixSeconds,
    ) -> Result<()> {
        self.ensure_admin(actor)?;
        self.ensure_revision(base_revision, new_revision)?;
        if removals.is_empty() {
            return Err(group_error("members list must not be empty"));
        }
        if removals.contains(&actor) {
            return Err(group_error("self-removal is not allowed"));
        }
        for owner in removals {
            if !self.members.contains(owner) {
                return Err(group_error(format!("owner {owner} is not a member")));
            }
        }
        for owner in removals {
            self.members.remove(owner);
            self.admins.remove(owner);
        }
        validate_group_invariants(&self.members, &self.admins)?;
        self.revision = new_revision;
        self.updated_at = updated_at;
        Ok(())
    }

    fn apply_add_admins(
        &mut self,
        actor: OwnerPubkey,
        additions: &BTreeSet<OwnerPubkey>,
        base_revision: u64,
        new_revision: u64,
        updated_at: UnixSeconds,
    ) -> Result<()> {
        self.ensure_admin(actor)?;
        self.ensure_revision(base_revision, new_revision)?;
        if additions.is_empty() {
            return Err(group_error("admins list must not be empty"));
        }
        for owner in additions {
            if !self.members.contains(owner) {
                return Err(group_error(format!("owner {owner} must be a member before promotion")));
            }
            if self.admins.contains(owner) {
                return Err(group_error(format!("owner {owner} is already an admin")));
            }
        }
        self.admins.extend(additions.iter().copied());
        self.revision = new_revision;
        self.updated_at = updated_at;
        Ok(())
    }

    fn apply_remove_admins(
        &mut self,
        actor: OwnerPubkey,
        removals: &BTreeSet<OwnerPubkey>,
        base_revision: u64,
        new_revision: u64,
        updated_at: UnixSeconds,
    ) -> Result<()> {
        self.ensure_admin(actor)?;
        self.ensure_revision(base_revision, new_revision)?;
        if removals.is_empty() {
            return Err(group_error("admins list must not be empty"));
        }
        for owner in removals {
            if !self.admins.contains(owner) {
                return Err(group_error(format!("owner {owner} is not an admin")));
            }
        }
        if self.admins.len() == removals.len() {
            return Err(group_error("cannot remove the last admin"));
        }
        for owner in removals {
            self.admins.remove(owner);
        }
        validate_group_invariants(&self.members, &self.admins)?;
        self.revision = new_revision;
        self.updated_at = updated_at;
        Ok(())
    }
}

fn random_group_id<R>(ctx: &mut ProtocolContext<'_, R>) -> String
where
    R: RngCore + CryptoRng,
{
    let mut bytes = [0u8; 16];
    ctx.rng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn validate_unique_owners(
    values: &[OwnerPubkey],
    label: &str,
) -> Result<BTreeSet<OwnerPubkey>> {
    let set: BTreeSet<_> = values.iter().copied().collect();
    if set.len() != values.len() {
        return Err(group_error(format!("duplicate {label} are not allowed")));
    }
    Ok(set)
}

fn validate_group_invariants(
    members: &BTreeSet<OwnerPubkey>,
    admins: &BTreeSet<OwnerPubkey>,
) -> Result<()> {
    if members.is_empty() {
        return Err(group_error("group must have at least one member"));
    }
    if admins.is_empty() {
        return Err(group_error("group must have at least one admin"));
    }
    if !admins.is_subset(members) {
        return Err(group_error("all admins must also be members"));
    }
    Ok(())
}

fn merge_group_prepared(into: &mut GroupPreparedSend, next: GroupPreparedSend) {
    into.deliveries.extend(next.deliveries);
    into.invite_responses.extend(next.invite_responses);
    into.relay_gaps.extend(next.relay_gaps);
    into.relay_gaps.sort();
    into.relay_gaps.dedup();
}

fn group_error(message: impl Into<String>) -> crate::Error {
    DomainError::InvalidGroupOperation(message.into()).into()
}
