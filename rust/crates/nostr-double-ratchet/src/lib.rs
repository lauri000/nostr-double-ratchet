pub mod app_keys;
pub mod codec;
pub mod error;
pub mod group;
pub mod ids;
pub mod invite;
pub mod peer_book;
pub mod session;
pub mod session_manager;
pub mod state;
pub mod types;

mod utils;

pub use app_keys::{AppKeys, DeviceEntry, DeviceLabels};
pub use error::{CodecError, DomainError, Error, Result};
pub use group::{
    add_group_admin, add_group_member, apply_metadata_update, build_group_metadata_content,
    create_group_data, generate_group_secret, is_group_admin, parse_group_metadata,
    remove_group_admin, remove_group_member, update_group_data, validate_metadata_creation,
    validate_metadata_update, GroupData, GroupMetadata, GroupUpdate, MetadataValidation,
    GROUP_INVITE_RUMOR_KIND, GROUP_METADATA_KIND, GROUP_SENDER_KEY_DISTRIBUTION_KIND,
    GROUP_SENDER_KEY_MESSAGE_KIND,
};
pub use ids::{DeviceId, DevicePubkey, GroupId, OwnerPubkey, UnixMillis, UnixSeconds};
pub use invite::{
    IncomingInviteResponseEnvelope, Invite, InviteResponse, OutgoingInviteResponseEnvelope,
};
pub use peer_book::{
    PeerBook, PeerBookReceivePlan, PeerBookSendPlan, StoredPeerBook, StoredPeerDevice,
};
pub use session::{
    DirectMessageContent, Header, IncomingDirectMessageEnvelope, OutgoingDirectMessageEnvelope,
    ReceiveOutcome, ReceivePlan, Rumor, SendOutcome, SendPlan, SerializableKeyPair, Session,
    SessionState, SkippedKeysEntry,
};
pub use session_manager::{
    DeviceDelivery, DeviceRecordSnapshot, PreparedFanout, ProcessedInviteResponse, PruneReport,
    ReceivedDirectMessage, RelayGap, SessionManager, SessionManagerPolicy, SessionManagerSnapshot,
    UserRecordSnapshot,
};
pub use state::{
    AppKeysSnapshotDecision, DeviceSnapshot, GroupMutation, InviteAcceptance, LocalSnapshot,
    NdrSnapshot, NdrState, PeerSnapshot, PreparedDirectMessage,
    ProcessedInviteResponse as NdrProcessedInviteResponse,
    ReceivedDirectMessage as NdrReceivedDirectMessage,
};
pub use types::{
    ProtocolContext, APP_KEYS_EVENT_KIND, CHAT_MESSAGE_KIND, CHAT_SETTINGS_KIND, EXPIRATION_TAG,
    INVITE_EVENT_KIND, INVITE_RESPONSE_KIND, MAX_SKIP, MESSAGE_EVENT_KIND, REACTION_KIND,
    RECEIPT_KIND, SHARED_CHANNEL_KIND, TYPING_KIND,
};

pub(crate) use utils::{
    device_pubkey_from_secret_bytes, kdf, random_secret_key_bytes, secret_key_from_bytes,
};

#[cfg(test)]
mod architecture_tests {
    #[test]
    fn domain_modules_do_not_pull_in_background_runtime_primitives() {
        const FILES: &[&str] = &[
            include_str!("app_keys.rs"),
            include_str!("group.rs"),
            include_str!("ids.rs"),
            include_str!("invite.rs"),
            include_str!("peer_book.rs"),
            include_str!("session.rs"),
            include_str!("session_manager.rs"),
            include_str!("state.rs"),
            include_str!("types.rs"),
        ];

        for source in FILES {
            for banned in [
                "tokio",
                "crossbeam",
                "Arc",
                "Mutex",
                "mpsc",
                "spawn",
                "async ",
            ] {
                assert!(
                    !source.contains(banned),
                    "found banned runtime primitive `{banned}` in domain source"
                );
            }
        }
    }
}
