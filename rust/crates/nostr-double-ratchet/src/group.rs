use crate::{Delivery, InviteResponseEnvelope, OwnerPubkey, RelayGap, UnixSeconds};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupSnapshot {
    pub group_id: String,
    pub name: String,
    pub created_by: OwnerPubkey,
    pub members: Vec<OwnerPubkey>,
    pub admins: Vec<OwnerPubkey>,
    pub revision: u64,
    pub created_at: UnixSeconds,
    pub updated_at: UnixSeconds,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupManagerSnapshot {
    pub local_owner_pubkey: OwnerPubkey,
    pub groups: Vec<GroupSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupPreparedSend {
    pub group_id: String,
    pub deliveries: Vec<Delivery>,
    pub invite_responses: Vec<InviteResponseEnvelope>,
    pub relay_gaps: Vec<RelayGap>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupCreateResult {
    pub group: GroupSnapshot,
    pub prepared: GroupPreparedSend,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupReceivedMessage {
    pub group_id: String,
    pub sender_owner: OwnerPubkey,
    pub body: Vec<u8>,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupIncomingEvent {
    MetadataUpdated(GroupSnapshot),
    Message(GroupReceivedMessage),
}
