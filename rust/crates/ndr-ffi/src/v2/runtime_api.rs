use crate::{FfiDeviceEntry, NdrError};
use nostr_double_ratchet::v2::{
    AppKeys, AppKeysEffect, AppKeysInput, AppKeysManager, AppKeysNotification, DeviceEntry,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, uniffi::Record, PartialEq, Eq)]
pub struct RuntimeEffect {
    pub kind: String,
    pub request_id: Option<String>,
    pub sub_id: Option<String>,
    pub correlation_id: Option<String>,
    pub event_json: Option<String>,
    pub key: Option<String>,
    pub value: Option<String>,
    pub success: Option<bool>,
    pub error: Option<String>,
}

#[derive(Default)]
struct RuntimeState {
    started: bool,
    effects: VecDeque<RuntimeEffect>,
    request_to_sub: HashMap<String, String>,
    active_subscriptions: HashSet<String>,
    router: SubscriptionRouter,
    app_keys_manager: AppKeysManager,
    sessions: SessionsNode,
    next_correlation_id: u64,
}

#[derive(uniffi::Object)]
pub struct RuntimeApi {
    inner: Mutex<RuntimeState>,
}

#[uniffi::export]
impl RuntimeApi {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(RuntimeState::default()),
        })
    }

    pub fn start(&self) {
        let mut state = self.inner.lock().expect("runtime mutex poisoned");
        if state.started {
            return;
        }
        state.started = true;
        state.effects.push_back(RuntimeEffect {
            kind: "runtime.started".to_string(),
            request_id: None,
            sub_id: None,
            correlation_id: None,
            event_json: None,
            key: None,
            value: None,
            success: Some(true),
            error: None,
        });
    }

    pub fn on_subscription_opened(
        &self,
        request_id: String,
        sub_id: String,
    ) -> Result<(), NdrError> {
        let mut state = self.inner.lock().expect("runtime mutex poisoned");
        ensure_started(&state)?;

        state
            .request_to_sub
            .insert(request_id.clone(), sub_id.clone());
        state.active_subscriptions.insert(sub_id.clone());
        if let Some(route) = Route::from_request_id(&request_id) {
            state.router.bind(sub_id.clone(), route);
        }
        state.effects.push_back(RuntimeEffect {
            kind: "runtime.nostr.subscription_opened".to_string(),
            request_id: Some(request_id),
            sub_id: Some(sub_id),
            correlation_id: None,
            event_json: None,
            key: None,
            value: None,
            success: Some(true),
            error: None,
        });
        Ok(())
    }

    pub fn on_subscription_closed(&self, sub_id: String) -> Result<(), NdrError> {
        let mut state = self.inner.lock().expect("runtime mutex poisoned");
        ensure_started(&state)?;

        state.active_subscriptions.remove(&sub_id);
        state
            .request_to_sub
            .retain(|_, mapped_sub_id| mapped_sub_id != &sub_id);
        state.router.unbind(&sub_id);
        state.effects.push_back(RuntimeEffect {
            kind: "runtime.nostr.subscription_closed".to_string(),
            request_id: None,
            sub_id: Some(sub_id),
            correlation_id: None,
            event_json: None,
            key: None,
            value: None,
            success: Some(true),
            error: None,
        });
        Ok(())
    }

    pub fn on_nostr_event(&self, sub_id: String, event_json: String) -> Result<(), NdrError> {
        let mut state = self.inner.lock().expect("runtime mutex poisoned");
        ensure_started(&state)?;

        let parsed_event: nostr::Event =
            serde_json::from_str(&event_json).map_err(|e| NdrError::InvalidEvent(e.to_string()))?;
        let normalized_json = serde_json::to_string(&parsed_event)
            .map_err(|e| NdrError::Serialization(e.to_string()))?;
        let Some(route) = state.router.resolve(&sub_id) else {
            state.effects.push_back(RuntimeEffect {
                kind: "runtime.nostr.unrouted_event".to_string(),
                request_id: None,
                sub_id: Some(sub_id),
                correlation_id: None,
                event_json: Some(normalized_json),
                key: None,
                value: None,
                success: Some(false),
                error: Some("unknown subscription route".to_string()),
            });
            return Ok(());
        };

        match route {
            Route::AppKeys(_app_route) => apply_app_keys_nostr_event(&mut state, parsed_event)?,
            Route::Sessions(session_route) => {
                state.sessions.on_nostr_event(session_route, &parsed_event)
            }
        }

        state.effects.push_back(RuntimeEffect {
            kind: "runtime.nostr.event_routed".to_string(),
            request_id: None,
            sub_id: Some(sub_id),
            correlation_id: None,
            event_json: Some(normalized_json),
            key: None,
            value: None,
            success: Some(true),
            error: None,
        });
        Ok(())
    }

    pub fn on_publish_result(
        &self,
        correlation_id: String,
        success: bool,
        error: Option<String>,
    ) -> Result<(), NdrError> {
        let mut state = self.inner.lock().expect("runtime mutex poisoned");
        ensure_started(&state)?;

        state.effects.push_back(RuntimeEffect {
            kind: "runtime.nostr.publish_result".to_string(),
            request_id: None,
            sub_id: None,
            correlation_id: Some(correlation_id),
            event_json: None,
            key: None,
            value: None,
            success: Some(success),
            error,
        });
        Ok(())
    }

    pub fn on_storage_read_result(
        &self,
        correlation_id: String,
        key: String,
        value: Option<String>,
        error: Option<String>,
    ) -> Result<(), NdrError> {
        let mut state = self.inner.lock().expect("runtime mutex poisoned");
        ensure_started(&state)?;

        apply_app_keys_input(
            &mut state,
            AppKeysInput::StorageReadResult {
                key: key.clone(),
                value: value.clone(),
                error: error.clone(),
            },
        )?;

        state.effects.push_back(RuntimeEffect {
            kind: "runtime.storage.read_result".to_string(),
            request_id: None,
            sub_id: None,
            correlation_id: Some(correlation_id),
            event_json: None,
            key: Some(key),
            value,
            success: Some(error.is_none()),
            error,
        });
        Ok(())
    }

    pub fn on_storage_write_result(
        &self,
        correlation_id: String,
        key: String,
        success: bool,
        error: Option<String>,
    ) -> Result<(), NdrError> {
        let mut state = self.inner.lock().expect("runtime mutex poisoned");
        ensure_started(&state)?;

        apply_app_keys_input(
            &mut state,
            AppKeysInput::StorageWriteResult {
                key: key.clone(),
                success,
                error: error.clone(),
            },
        )?;

        state.effects.push_back(RuntimeEffect {
            kind: "runtime.storage.write_result".to_string(),
            request_id: None,
            sub_id: None,
            correlation_id: Some(correlation_id),
            event_json: None,
            key: Some(key),
            value: None,
            success: Some(success),
            error,
        });
        Ok(())
    }

    pub fn on_storage_delete_result(
        &self,
        correlation_id: String,
        key: String,
        success: bool,
        error: Option<String>,
    ) -> Result<(), NdrError> {
        let mut state = self.inner.lock().expect("runtime mutex poisoned");
        ensure_started(&state)?;

        apply_app_keys_input(
            &mut state,
            AppKeysInput::StorageDeleteResult {
                key: key.clone(),
                success,
                error: error.clone(),
            },
        )?;

        state.effects.push_back(RuntimeEffect {
            kind: "runtime.storage.delete_result".to_string(),
            request_id: None,
            sub_id: None,
            correlation_id: Some(correlation_id),
            event_json: None,
            key: Some(key),
            value: None,
            success: Some(success),
            error,
        });
        Ok(())
    }

    pub fn drain_effects(&self) -> Vec<RuntimeEffect> {
        let mut state = self.inner.lock().expect("runtime mutex poisoned");
        state.effects.drain(..).collect()
    }
}

impl RuntimeApi {
    pub(crate) fn app_keys_start_internal(&self) -> Result<(), NdrError> {
        let mut state = self.inner.lock().expect("runtime mutex poisoned");
        ensure_started(&state)?;
        apply_app_keys_input(&mut state, AppKeysInput::Start)
    }

    pub(crate) fn app_keys_set_devices_internal(
        &self,
        devices: Vec<FfiDeviceEntry>,
    ) -> Result<(), NdrError> {
        let mut state = self.inner.lock().expect("runtime mutex poisoned");
        ensure_started(&state)?;

        let mut mapped = Vec::with_capacity(devices.len());
        for device in devices {
            mapped.push(device_entry_from_ffi(device)?);
        }

        apply_app_keys_input(
            &mut state,
            AppKeysInput::SetAppKeys {
                app_keys: AppKeys::new(mapped),
            },
        )
    }

    pub(crate) fn app_keys_add_device_internal(
        &self,
        identity_pubkey_hex: String,
        created_at: u64,
    ) -> Result<(), NdrError> {
        let mut state = self.inner.lock().expect("runtime mutex poisoned");
        ensure_started(&state)?;

        let identity_pubkey = nostr_double_ratchet::utils::pubkey_from_hex(&identity_pubkey_hex)?;
        apply_app_keys_input(
            &mut state,
            AppKeysInput::AddDeviceKey {
                device: DeviceEntry {
                    identity_pubkey,
                    created_at,
                },
            },
        )
    }

    pub(crate) fn app_keys_remove_device_internal(
        &self,
        identity_pubkey_hex: String,
    ) -> Result<(), NdrError> {
        let mut state = self.inner.lock().expect("runtime mutex poisoned");
        ensure_started(&state)?;

        let identity_pubkey = nostr_double_ratchet::utils::pubkey_from_hex(&identity_pubkey_hex)?;
        apply_app_keys_input(
            &mut state,
            AppKeysInput::RemoveDeviceKey { identity_pubkey },
        )
    }

    pub(crate) fn app_keys_merge_from_event_json_internal(
        &self,
        event_json: String,
    ) -> Result<(), NdrError> {
        let mut state = self.inner.lock().expect("runtime mutex poisoned");
        ensure_started(&state)?;

        let event: nostr::Event =
            serde_json::from_str(&event_json).map_err(|e| NdrError::InvalidEvent(e.to_string()))?;
        apply_app_keys_input(&mut state, AppKeysInput::ApplyAppKeysEvent { event })
    }

    pub(crate) fn app_keys_publish_unsigned_internal(
        &self,
        owner_pubkey_hex: String,
    ) -> Result<(), NdrError> {
        let mut state = self.inner.lock().expect("runtime mutex poisoned");
        ensure_started(&state)?;

        let owner_pubkey = nostr_double_ratchet::utils::pubkey_from_hex(&owner_pubkey_hex)?;
        apply_app_keys_input(&mut state, AppKeysInput::PublishAppKeys { owner_pubkey })
    }

    pub(crate) fn app_keys_list_devices_internal(&self) -> Result<Vec<FfiDeviceEntry>, NdrError> {
        let state = self.inner.lock().expect("runtime mutex poisoned");
        ensure_started(&state)?;
        Ok(state
            .app_keys_manager
            .list_device_keys()
            .into_iter()
            .map(device_entry_to_ffi)
            .collect())
    }

    pub(crate) fn app_keys_get_device_internal(
        &self,
        identity_pubkey_hex: String,
    ) -> Result<Option<FfiDeviceEntry>, NdrError> {
        let state = self.inner.lock().expect("runtime mutex poisoned");
        ensure_started(&state)?;
        let identity_pubkey = nostr_double_ratchet::utils::pubkey_from_hex(&identity_pubkey_hex)?;
        Ok(state
            .app_keys_manager
            .get_device_key(&identity_pubkey)
            .cloned()
            .map(device_entry_to_ffi))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    AppKeys(AppKeysRoute),
    Sessions(SessionsRoute),
}

impl Route {
    fn from_request_id(request_id: &str) -> Option<Self> {
        if request_id.starts_with("app_keys:") || request_id.starts_with("app_keys/") {
            Some(Route::AppKeys(AppKeysRoute::Default))
        } else if request_id.starts_with("sessions:") || request_id.starts_with("sessions/") {
            Some(Route::Sessions(SessionsRoute::Default))
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppKeysRoute {
    Default,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionsRoute {
    Default,
}

#[derive(Default)]
struct SubscriptionRouter {
    by_sub_id: HashMap<String, Route>,
}

impl SubscriptionRouter {
    fn bind(&mut self, sub_id: String, route: Route) {
        self.by_sub_id.insert(sub_id, route);
    }

    fn unbind(&mut self, sub_id: &str) {
        self.by_sub_id.remove(sub_id);
    }

    fn resolve(&self, sub_id: &str) -> Option<Route> {
        self.by_sub_id.get(sub_id).copied()
    }
}

#[derive(Default)]
struct SessionsNode;

impl SessionsNode {
    fn on_nostr_event(&mut self, _route: SessionsRoute, _event: &nostr::Event) {
        // Intentionally no-op in this slice: runtime routing only.
    }
}

fn apply_app_keys_nostr_event(
    state: &mut RuntimeState,
    event: nostr::Event,
) -> Result<(), NdrError> {
    if nostr_double_ratchet::v2::is_app_keys_event(&event) {
        apply_app_keys_input(state, AppKeysInput::ApplyAppKeysEvent { event })?;
    }
    Ok(())
}

fn apply_app_keys_input(state: &mut RuntimeState, input: AppKeysInput) -> Result<(), NdrError> {
    let effects = state.app_keys_manager.apply(input)?;
    apply_app_keys_effects(state, effects)
}

fn apply_app_keys_effects(
    state: &mut RuntimeState,
    effects: Vec<AppKeysEffect>,
) -> Result<(), NdrError> {
    for effect in effects {
        match effect {
            AppKeysEffect::RequestStorageRead { key } => {
                let correlation_id = next_correlation_id(state, "app_keys/storage/read");
                state.effects.push_back(RuntimeEffect {
                    kind: "runtime.storage.read_request".to_string(),
                    request_id: None,
                    sub_id: None,
                    correlation_id: Some(correlation_id),
                    event_json: None,
                    key: Some(key),
                    value: None,
                    success: None,
                    error: None,
                });
            }
            AppKeysEffect::RequestStorageWrite { key, value } => {
                let correlation_id = next_correlation_id(state, "app_keys/storage/write");
                state.effects.push_back(RuntimeEffect {
                    kind: "runtime.storage.write_request".to_string(),
                    request_id: None,
                    sub_id: None,
                    correlation_id: Some(correlation_id),
                    event_json: None,
                    key: Some(key),
                    value: Some(value),
                    success: None,
                    error: None,
                });
            }
            AppKeysEffect::RequestStorageDelete { key } => {
                let correlation_id = next_correlation_id(state, "app_keys/storage/delete");
                state.effects.push_back(RuntimeEffect {
                    kind: "runtime.storage.delete_request".to_string(),
                    request_id: None,
                    sub_id: None,
                    correlation_id: Some(correlation_id),
                    event_json: None,
                    key: Some(key),
                    value: None,
                    success: None,
                    error: None,
                });
            }
            AppKeysEffect::RequestPublishUnsigned { event } => {
                let event_json = serde_json::to_string(&event)
                    .map_err(|e| NdrError::Serialization(e.to_string()))?;
                state.effects.push_back(RuntimeEffect {
                    kind: "runtime.nostr.publish_unsigned".to_string(),
                    request_id: None,
                    sub_id: None,
                    correlation_id: None,
                    event_json: Some(event_json),
                    key: None,
                    value: None,
                    success: Some(true),
                    error: None,
                });
            }
            AppKeysEffect::Notify(notification) => {
                state.effects.push_back(RuntimeEffect {
                    kind: notification_kind(&notification).to_string(),
                    request_id: None,
                    sub_id: None,
                    correlation_id: None,
                    event_json: None,
                    key: None,
                    value: Some(format!("{notification:?}")),
                    success: Some(true),
                    error: None,
                });
            }
        }
    }

    Ok(())
}

fn next_correlation_id(state: &mut RuntimeState, prefix: &str) -> String {
    let id = state.next_correlation_id;
    state.next_correlation_id = state.next_correlation_id.saturating_add(1);
    format!("{prefix}:{id}")
}

fn notification_kind(notification: &AppKeysNotification) -> &'static str {
    match notification {
        AppKeysNotification::Started => "runtime.app_keys.started",
        AppKeysNotification::Initialized => "runtime.app_keys.initialized",
        AppKeysNotification::StartIgnoredAlreadyInitializing => {
            "runtime.app_keys.start_ignored_already_initializing"
        }
        AppKeysNotification::StartIgnoredAlreadyInitialized => {
            "runtime.app_keys.start_ignored_already_initialized"
        }
        AppKeysNotification::InputIgnoredNotStarted => "runtime.app_keys.input_ignored_not_started",
        AppKeysNotification::InputIgnoredStillInitializing => {
            "runtime.app_keys.input_ignored_still_initializing"
        }
        AppKeysNotification::DeviceAdded { .. } => "runtime.app_keys.device_added",
        AppKeysNotification::DeviceAddIgnoredAlreadyExists { .. } => {
            "runtime.app_keys.device_add_ignored_already_exists"
        }
        AppKeysNotification::DeviceRemoved { .. } => "runtime.app_keys.device_removed",
        AppKeysNotification::DeviceRemoveIgnoredMissing { .. } => {
            "runtime.app_keys.device_remove_ignored_missing"
        }
        AppKeysNotification::AppKeysSet => "runtime.app_keys.app_keys_set",
        AppKeysNotification::AppKeysMergedFromEvent => {
            "runtime.app_keys.app_keys_merged_from_event"
        }
        AppKeysNotification::AppKeysEventIgnoredNoStateChange => {
            "runtime.app_keys.app_keys_event_ignored_no_state_change"
        }
        AppKeysNotification::StorageReadApplied { .. } => "runtime.app_keys.storage_read_applied",
        AppKeysNotification::StorageReadIgnoredUnknownKey { .. } => {
            "runtime.app_keys.storage_read_ignored_unknown_key"
        }
        AppKeysNotification::StorageReadFailed { .. } => "runtime.app_keys.storage_read_failed",
        AppKeysNotification::StorageWriteAcknowledged { .. } => {
            "runtime.app_keys.storage_write_acknowledged"
        }
        AppKeysNotification::StorageDeleteAcknowledged { .. } => {
            "runtime.app_keys.storage_delete_acknowledged"
        }
        AppKeysNotification::PublishRequested => "runtime.app_keys.publish_requested",
    }
}

fn device_entry_from_ffi(device: FfiDeviceEntry) -> Result<DeviceEntry, NdrError> {
    Ok(DeviceEntry {
        identity_pubkey: nostr_double_ratchet::utils::pubkey_from_hex(&device.identity_pubkey_hex)?,
        created_at: device.created_at,
    })
}

fn device_entry_to_ffi(device: DeviceEntry) -> FfiDeviceEntry {
    FfiDeviceEntry {
        identity_pubkey_hex: device.identity_pubkey.to_hex(),
        created_at: device.created_at,
    }
}

fn ensure_started(state: &RuntimeState) -> Result<(), NdrError> {
    if state.started {
        Ok(())
    } else {
        Err(NdrError::StateMismatch(
            "RuntimeApi.start() must be called first".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::RuntimeApi;
    use nostr_double_ratchet::v2::{AppKeys, DeviceEntry, APP_KEYS_STORAGE_KEY};

    #[test]
    fn callbacks_require_start() {
        let api = RuntimeApi::new();
        let err = api
            .on_subscription_opened("req-1".to_string(), "sub-1".to_string())
            .expect_err("callbacks should reject calls before start");
        match err {
            crate::NdrError::StateMismatch(msg) => {
                assert!(msg.contains("start"), "unexpected message: {msg}")
            }
            _ => panic!("unexpected error type"),
        }
    }

    #[test]
    fn drains_effects_in_fifo_order() {
        let api = RuntimeApi::new();
        api.start();
        api.on_subscription_opened("app_keys:req-1".to_string(), "sub-1".to_string())
            .expect("open callback should succeed");

        let keys = nostr::Keys::generate();
        let unsigned = nostr::EventBuilder::text_note("hello").build(keys.public_key());
        let signed = unsigned
            .sign_with_keys(&keys)
            .expect("event signing should succeed");
        let event_json = serde_json::to_string(&signed).expect("signed event should serialize");

        api.on_nostr_event("sub-1".to_string(), event_json)
            .expect("event callback should succeed");

        let effects = api.drain_effects();
        let kinds = effects
            .iter()
            .map(|effect| effect.kind.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                "runtime.started",
                "runtime.nostr.subscription_opened",
                "runtime.nostr.event_routed",
            ]
        );

        assert!(
            api.drain_effects().is_empty(),
            "drain should clear the queue"
        );
    }

    #[test]
    fn unrouted_sub_id_emits_unrouted_effect() {
        let api = RuntimeApi::new();
        api.start();

        let keys = nostr::Keys::generate();
        let unsigned = nostr::EventBuilder::text_note("hello").build(keys.public_key());
        let signed = unsigned
            .sign_with_keys(&keys)
            .expect("event signing should succeed");
        let event_json = serde_json::to_string(&signed).expect("signed event should serialize");

        api.on_nostr_event("sub-missing".to_string(), event_json)
            .expect("callback should not error for unrouted sub_id");

        let effects = api.drain_effects();
        let last = effects
            .last()
            .expect("expected at least one effect after start + event");
        assert_eq!(last.kind, "runtime.nostr.unrouted_event");
        assert_eq!(last.success, Some(false));
    }

    #[test]
    fn app_keys_start_emits_storage_read_request() {
        let api = RuntimeApi::new();
        api.start();

        api.app_keys_start_internal()
            .expect("app keys start should succeed");

        let effects = api.drain_effects();
        assert!(
            effects
                .iter()
                .any(|effect| effect.kind == "runtime.storage.read_request"),
            "expected a storage read request effect"
        );
    }

    #[test]
    fn routed_app_keys_event_updates_state_and_emits_storage_write_request() {
        let api = RuntimeApi::new();
        api.start();

        api.app_keys_start_internal()
            .expect("app keys start should succeed");
        api.on_storage_read_result(
            "corr-read-0".to_string(),
            APP_KEYS_STORAGE_KEY.to_string(),
            None,
            None,
        )
        .expect("app keys initialization read callback should succeed");

        api.on_subscription_opened("app_keys:req-1".to_string(), "sub-1".to_string())
            .expect("open callback should succeed");

        let owner_keys = nostr::Keys::generate();
        let device_pk = nostr::Keys::generate().public_key();
        let signed = AppKeys::new(vec![DeviceEntry {
            identity_pubkey: device_pk,
            created_at: 11,
        }])
        .get_event(owner_keys.public_key())
        .sign_with_keys(&owner_keys)
        .expect("app-keys event signing should succeed");
        let event_json =
            serde_json::to_string(&signed).expect("signed app-keys event should serialize");

        api.on_nostr_event("sub-1".to_string(), event_json)
            .expect("event callback should succeed");

        let loaded = api
            .app_keys_get_device_internal(device_pk.to_hex())
            .expect("lookup should succeed");
        assert!(
            loaded.is_some(),
            "routed app-keys event should update state"
        );

        let effects = api.drain_effects();
        assert!(
            effects
                .iter()
                .any(|effect| effect.kind == "runtime.storage.write_request"),
            "expected storage write request from merged app-keys state"
        );
        assert!(
            effects
                .iter()
                .any(|effect| effect.kind == "runtime.app_keys.app_keys_merged_from_event"),
            "expected app-keys merge notification"
        );
    }
}
