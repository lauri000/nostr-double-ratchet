pub mod direct_message_subscriptions;
pub mod file_storage;
#[cfg(feature = "nearby")]
pub mod nearby;
#[cfg(feature = "nearby-mdns")]
pub mod nearby_lan;
pub mod protocol_backfill;
pub mod runtime;
pub mod storage;
pub mod user_record;

pub use direct_message_subscriptions::{
    build_direct_message_backfill_filter, direct_message_subscription_authors,
    DirectMessageSubscriptionTracker,
};
pub use file_storage::{DebouncedFileStorage, FileStorageAdapter};
#[cfg(feature = "nearby")]
pub use nearby::{
    decode_nearby_frame_json, encode_nearby_frame_json, nearby_frame_body_len_from_header,
    read_nearby_frame, NearbyFrameAssembler, NEARBY_FRAME_HEADER_BYTES,
    NEARBY_MAX_FRAME_BODY_BYTES,
};
#[cfg(feature = "nearby-mdns")]
pub use nearby_lan::{
    is_allowed_nearby_peer, NearbyLanConfig, NearbyLanError, NearbyLanIncoming, NearbyLanService,
    IRIS_NEARBY_SERVICE_TYPE,
};
pub use nostr_double_ratchet::*;
pub use nostr_double_ratchet_nostr::{
    self as nostr_adapter, is_app_keys_event, AppKeys, DeviceEntry, DeviceLabels,
    APP_KEYS_EVENT_KIND, CHAT_MESSAGE_KIND, CHAT_SETTINGS_KIND, EXPIRATION_TAG,
    GROUP_SENDER_KEY_MESSAGE_KIND, INVITE_EVENT_KIND, INVITE_RESPONSE_KIND, MESSAGE_EVENT_KIND,
    REACTION_KIND, RECEIPT_KIND, SHARED_CHANNEL_KIND, TYPING_KIND,
};
pub use nostr_double_ratchet_nostr::{
    apply_app_keys_snapshot, apply_app_keys_snapshot_with_required_device,
    evaluate_device_registration_state, resolve_conversation_candidate_pubkeys,
    resolve_invite_owner_routing, resolve_rumor_peer_pubkey, select_latest_app_keys_from_events,
    should_require_relay_registration_confirmation, AppKeysSnapshot, AppKeysSnapshotDecision,
    ChatSettingsPayloadV1, DeviceRegistrationState, InviteOwnerRoutingResolution,
    JsonGroupPayloadCodecV1, NostrGroupManager, SendOptions,
};
pub use nostr_double_ratchet_nostr::{message_builders, message_origin, multi_device, nostr_codec};
pub use nostr_double_ratchet_pairwise_codec as pairwise_codec;
pub use protocol_backfill::{
    NdrProtocolBackfillOptions, DEFAULT_INVITE_BACKFILL_LOOKBACK_SECS,
    DEFAULT_MESSAGE_BACKFILL_LOOKBACK_SECS,
};
pub use runtime::{
    AcceptInviteResult, GroupOuterSubscriptionPlan, MessagePushSessionStateSnapshot, NdrRuntime,
    QueuedMessageDiagnostic, QueuedMessageStage, RuntimeAcceptInviteResult, RuntimeEffect,
    RuntimeGroupCreateResult, RuntimeGroupIncomingResult, RuntimeGroupSnapshotResult,
    RuntimePublishResult, RuntimeTextSendResult,
};
pub use storage::{InMemoryStorage, StorageAdapter};
pub use user_record::{DeviceRecord, StoredDeviceRecord, StoredUserRecord, UserRecord};

pub mod utils {
    pub use nostr_double_ratchet_nostr::utils::*;
}
