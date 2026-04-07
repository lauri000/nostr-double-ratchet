pub mod error;
pub mod ids;
pub mod invite;
pub mod roster;
pub mod session;
pub mod session_manager;
pub mod types;

mod utils;

pub use error::{DomainError, Error, Result};
pub use ids::{DevicePubkey, OwnerPubkey, UnixSeconds};
pub use invite::{Invite, InviteResponse, InviteResponseEnvelope};
pub use roster::{AuthorizedDevice, DeviceRoster, RosterSnapshotDecision};
pub use session::{
    Header, MessageEnvelope, ReceiveOutcome, ReceivePlan, SendOutcome, SendPlan,
    SerializableKeyPair, Session, SessionState, SkippedKeysEntry,
};
pub use session_manager::{
    Delivery, DeviceRecordSnapshot, PreparedSend, ProcessedInviteResponse, PruneReport,
    ReceivedMessage, RelayGap, SessionManager, SessionManagerSnapshot, UserRecordSnapshot,
};
pub use types::{ProtocolContext, MAX_SKIP};

pub(crate) use utils::{
    device_pubkey_from_secret_bytes, kdf, random_secret_key_bytes, secret_key_from_bytes,
};

#[cfg(test)]
mod architecture_tests {
    #[test]
    fn domain_modules_do_not_pull_in_background_runtime_primitives() {
        const FILES: &[&str] = &[
            include_str!("ids.rs"),
            include_str!("invite.rs"),
            include_str!("roster.rs"),
            include_str!("session.rs"),
            include_str!("session_manager.rs"),
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
