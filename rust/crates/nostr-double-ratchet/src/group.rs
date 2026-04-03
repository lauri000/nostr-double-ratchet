use crate::{DomainError, GroupId, OwnerPubkey, ProtocolContext, Result, UnixMillis};
use rand::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const GROUP_METADATA_KIND: u32 = 40;
pub const GROUP_INVITE_RUMOR_KIND: u32 = 10445;
pub const GROUP_SENDER_KEY_DISTRIBUTION_KIND: u32 = 10446;
pub const GROUP_SENDER_KEY_MESSAGE_KIND: u32 = 10447;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupData {
    pub id: GroupId,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picture: Option<String>,
    #[serde(with = "serde_owner_set")]
    pub members: BTreeSet<OwnerPubkey>,
    #[serde(with = "serde_owner_set")]
    pub admins: BTreeSet<OwnerPubkey>,
    pub created_at: UnixMillis,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupMetadata {
    pub id: GroupId,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picture: Option<String>,
    #[serde(with = "serde_owner_set")]
    pub members: BTreeSet<OwnerPubkey>,
    #[serde(with = "serde_owner_set")]
    pub admins: BTreeSet<OwnerPubkey>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataValidation {
    Accept,
    Reject,
    Removed,
}

pub struct GroupUpdate {
    pub name: Option<String>,
    pub description: Option<String>,
    pub picture: Option<String>,
}

pub fn is_group_admin(group: &GroupData, pubkey: &OwnerPubkey) -> bool {
    group.admins.contains(pubkey)
}

pub fn generate_group_secret<R>(rng: &mut R) -> String
where
    R: RngCore + CryptoRng,
{
    let mut bytes = [0u8; 32];
    rng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

pub fn create_group_data<R>(
    ctx: &mut ProtocolContext<'_, R>,
    name: &str,
    creator_pubkey: OwnerPubkey,
    member_pubkeys: &[OwnerPubkey],
) -> GroupData
where
    R: RngCore + CryptoRng,
{
    let mut uuid_bytes = [0u8; 16];
    ctx.rng.fill_bytes(&mut uuid_bytes);
    let id = GroupId::new(
        uuid::Builder::from_random_bytes(uuid_bytes)
            .into_uuid()
            .to_string(),
    );

    let mut members = BTreeSet::new();
    members.insert(creator_pubkey);
    members.extend(member_pubkeys.iter().copied());

    let mut admins = BTreeSet::new();
    admins.insert(creator_pubkey);

    GroupData {
        id,
        name: name.to_string(),
        description: None,
        picture: None,
        members,
        admins,
        created_at: ctx.now_millis,
        secret: Some(generate_group_secret(ctx.rng)),
        accepted: Some(true),
    }
}

pub fn build_group_metadata_content(group: &GroupData, exclude_secret: bool) -> Result<String> {
    let metadata = GroupMetadata {
        id: group.id.clone(),
        name: group.name.clone(),
        description: group.description.clone(),
        picture: group.picture.clone(),
        members: group.members.clone(),
        admins: group.admins.clone(),
        secret: if exclude_secret {
            None
        } else {
            group.secret.clone()
        },
    };
    Ok(serde_json::to_string(&metadata)?)
}

pub fn parse_group_metadata(content: &str) -> Result<GroupMetadata> {
    Ok(serde_json::from_str(content)?)
}

pub fn validate_metadata_update(
    existing: &GroupData,
    metadata: &GroupMetadata,
    sender: OwnerPubkey,
    my_pubkey: OwnerPubkey,
) -> MetadataValidation {
    if !is_group_admin(existing, &sender) {
        return MetadataValidation::Reject;
    }
    if !metadata.members.contains(&my_pubkey) {
        return MetadataValidation::Removed;
    }
    MetadataValidation::Accept
}

pub fn validate_metadata_creation(
    metadata: &GroupMetadata,
    sender: OwnerPubkey,
    my_pubkey: OwnerPubkey,
) -> bool {
    metadata.admins.contains(&sender) && metadata.members.contains(&my_pubkey)
}

pub fn apply_metadata_update(existing: &GroupData, metadata: &GroupMetadata) -> GroupData {
    GroupData {
        id: existing.id.clone(),
        name: metadata.name.clone(),
        description: metadata.description.clone(),
        picture: metadata.picture.clone(),
        members: metadata.members.clone(),
        admins: metadata.admins.clone(),
        created_at: existing.created_at,
        secret: metadata.secret.clone().or_else(|| existing.secret.clone()),
        accepted: existing.accepted,
    }
}

pub fn add_group_member<R>(
    group: &GroupData,
    pubkey: OwnerPubkey,
    actor: OwnerPubkey,
    rng: &mut R,
) -> Result<GroupData>
where
    R: RngCore + CryptoRng,
{
    if !is_group_admin(group, &actor) {
        return Err(DomainError::InvalidState("actor is not group admin".to_string()).into());
    }
    if group.members.contains(&pubkey) {
        return Err(DomainError::InvalidState("member already exists".to_string()).into());
    }
    let mut new_members = group.members.clone();
    new_members.insert(pubkey);
    Ok(GroupData {
        members: new_members,
        secret: Some(generate_group_secret(rng)),
        ..group.clone()
    })
}

pub fn remove_group_member<R>(
    group: &GroupData,
    pubkey: OwnerPubkey,
    actor: OwnerPubkey,
    rng: &mut R,
) -> Result<GroupData>
where
    R: RngCore + CryptoRng,
{
    if !is_group_admin(group, &actor) {
        return Err(DomainError::InvalidState("actor is not group admin".to_string()).into());
    }
    if !group.members.contains(&pubkey) {
        return Err(DomainError::InvalidState("member not found".to_string()).into());
    }
    if pubkey == actor {
        return Err(DomainError::InvalidState("admin cannot remove self".to_string()).into());
    }
    let mut members = group.members.clone();
    let mut admins = group.admins.clone();
    members.remove(&pubkey);
    admins.remove(&pubkey);
    Ok(GroupData {
        members,
        admins,
        secret: Some(generate_group_secret(rng)),
        ..group.clone()
    })
}

pub fn update_group_data(
    group: &GroupData,
    updates: &GroupUpdate,
    actor: OwnerPubkey,
) -> Result<GroupData> {
    if !is_group_admin(group, &actor) {
        return Err(DomainError::InvalidState("actor is not group admin".to_string()).into());
    }

    let mut updated = group.clone();
    if let Some(name) = &updates.name {
        updated.name = name.clone();
    }
    if let Some(description) = &updates.description {
        updated.description = Some(description.clone());
    }
    if let Some(picture) = &updates.picture {
        updated.picture = Some(picture.clone());
    }
    Ok(updated)
}

pub fn add_group_admin(
    group: &GroupData,
    pubkey: OwnerPubkey,
    actor: OwnerPubkey,
) -> Result<GroupData> {
    if !is_group_admin(group, &actor) {
        return Err(DomainError::InvalidState("actor is not group admin".to_string()).into());
    }
    if !group.members.contains(&pubkey) {
        return Err(DomainError::InvalidState("new admin must be a member".to_string()).into());
    }
    if group.admins.contains(&pubkey) {
        return Err(DomainError::InvalidState("admin already exists".to_string()).into());
    }
    let mut admins = group.admins.clone();
    admins.insert(pubkey);
    Ok(GroupData {
        admins,
        ..group.clone()
    })
}

pub fn remove_group_admin(
    group: &GroupData,
    pubkey: OwnerPubkey,
    actor: OwnerPubkey,
) -> Result<GroupData> {
    if !is_group_admin(group, &actor) {
        return Err(DomainError::InvalidState("actor is not group admin".to_string()).into());
    }
    if !group.admins.contains(&pubkey) {
        return Err(DomainError::InvalidState("admin not found".to_string()).into());
    }
    if group.admins.len() <= 1 {
        return Err(DomainError::InvalidState("cannot remove last admin".to_string()).into());
    }
    let mut admins = group.admins.clone();
    admins.remove(&pubkey);
    Ok(GroupData {
        admins,
        ..group.clone()
    })
}

mod serde_owner_set {
    use crate::OwnerPubkey;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeSet;

    pub fn serialize<S>(set: &BTreeSet<OwnerPubkey>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let ordered: Vec<OwnerPubkey> = set.iter().copied().collect();
        ordered.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<BTreeSet<OwnerPubkey>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let ordered: Vec<OwnerPubkey> = Vec::deserialize(deserializer)?;
        Ok(ordered.into_iter().collect())
    }
}
