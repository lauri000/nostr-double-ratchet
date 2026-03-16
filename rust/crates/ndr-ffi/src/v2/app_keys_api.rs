use super::RuntimeApi;
use crate::{FfiDeviceEntry, NdrError};
use std::sync::Arc;

#[derive(uniffi::Object)]
pub struct AppKeysApi {
    runtime: Arc<RuntimeApi>,
}

impl AppKeysApi {
    pub(crate) fn new_with_runtime(runtime: Arc<RuntimeApi>) -> Arc<Self> {
        Arc::new(Self { runtime })
    }
}

#[uniffi::export]
impl AppKeysApi {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Self::new_with_runtime(RuntimeApi::new())
    }

    pub fn runtime(&self) -> Arc<RuntimeApi> {
        self.runtime.clone()
    }

    pub fn start(&self) -> Result<(), NdrError> {
        self.runtime.app_keys_start_internal()
    }

    pub fn set_devices(&self, devices: Vec<FfiDeviceEntry>) -> Result<(), NdrError> {
        self.runtime.app_keys_set_devices_internal(devices)
    }

    pub fn add_device(&self, identity_pubkey_hex: String, created_at: u64) -> Result<(), NdrError> {
        self.runtime
            .app_keys_add_device_internal(identity_pubkey_hex, created_at)
    }

    pub fn remove_device(&self, identity_pubkey_hex: String) -> Result<(), NdrError> {
        self.runtime
            .app_keys_remove_device_internal(identity_pubkey_hex)
    }

    pub fn merge_from_event_json(&self, event_json: String) -> Result<(), NdrError> {
        self.runtime
            .app_keys_merge_from_event_json_internal(event_json)
    }

    pub fn publish_unsigned(&self, owner_pubkey_hex: String) -> Result<(), NdrError> {
        self.runtime
            .app_keys_publish_unsigned_internal(owner_pubkey_hex)
    }

    pub fn list_devices(&self) -> Result<Vec<FfiDeviceEntry>, NdrError> {
        self.runtime.app_keys_list_devices_internal()
    }

    pub fn get_device(
        &self,
        identity_pubkey_hex: String,
    ) -> Result<Option<FfiDeviceEntry>, NdrError> {
        self.runtime
            .app_keys_get_device_internal(identity_pubkey_hex)
    }
}

#[cfg(test)]
mod tests {
    use super::AppKeysApi;
    use crate::v2::runtime_api::RuntimeApi;
    use nostr_double_ratchet::v2::{AppKeys, DeviceEntry, APP_KEYS_STORAGE_KEY};

    #[test]
    fn start_requires_runtime_start() {
        let runtime = RuntimeApi::new();
        let app_keys = AppKeysApi::new_with_runtime(runtime);
        let err = app_keys
            .start()
            .expect_err("start should fail before RuntimeApi.start()");
        match err {
            crate::NdrError::StateMismatch(msg) => {
                assert!(msg.contains("start"), "unexpected message: {msg}")
            }
            _ => panic!("unexpected error type"),
        }
    }

    #[test]
    fn add_and_get_device_after_initialization() {
        let runtime = RuntimeApi::new();
        runtime.start();
        let app_keys = AppKeysApi::new_with_runtime(runtime.clone());

        app_keys.start().expect("start should succeed");
        runtime
            .on_storage_read_result(
                "corr-read-0".to_string(),
                APP_KEYS_STORAGE_KEY.to_string(),
                None,
                None,
            )
            .expect("initialization callback should succeed");

        let device_hex = nostr::Keys::generate().public_key().to_hex();
        app_keys
            .add_device(device_hex.clone(), 42)
            .expect("add device should succeed");

        let loaded = app_keys
            .get_device(device_hex.clone())
            .expect("get should succeed")
            .expect("device should exist");
        assert_eq!(loaded.identity_pubkey_hex, device_hex);
        assert_eq!(loaded.created_at, 42);

        let listed = app_keys.list_devices().expect("list should succeed");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].identity_pubkey_hex, device_hex);

        let effects = runtime.drain_effects();
        assert!(
            effects
                .iter()
                .any(|effect| effect.kind == "runtime.storage.write_request"),
            "add should emit storage write request"
        );
        assert!(
            effects
                .iter()
                .any(|effect| effect.kind == "runtime.app_keys.device_added"),
            "add should emit device-added notification"
        );
    }

    #[test]
    fn merge_and_publish_unsigned_emit_expected_effects() {
        let runtime = RuntimeApi::new();
        runtime.start();
        let app_keys = AppKeysApi::new_with_runtime(runtime.clone());

        app_keys.start().expect("start should succeed");
        runtime
            .on_storage_read_result(
                "corr-read-0".to_string(),
                APP_KEYS_STORAGE_KEY.to_string(),
                None,
                None,
            )
            .expect("initialization callback should succeed");

        let owner_keys = nostr::Keys::generate();
        let device_pubkey = nostr::Keys::generate().public_key();
        let signed = AppKeys::new(vec![DeviceEntry {
            identity_pubkey: device_pubkey,
            created_at: 7,
        }])
        .get_event(owner_keys.public_key())
        .sign_with_keys(&owner_keys)
        .expect("app-keys event signing should succeed");
        let event_json =
            serde_json::to_string(&signed).expect("signed app-keys event should serialize");

        app_keys
            .merge_from_event_json(event_json)
            .expect("merge should succeed");
        app_keys
            .publish_unsigned(owner_keys.public_key().to_hex())
            .expect("publish unsigned should succeed");

        let listed = app_keys.list_devices().expect("list should succeed");
        assert!(
            listed
                .iter()
                .any(|entry| entry.identity_pubkey_hex == device_pubkey.to_hex()),
            "merged device should be visible through API"
        );

        let effects = runtime.drain_effects();
        assert!(
            effects
                .iter()
                .any(|effect| effect.kind == "runtime.app_keys.app_keys_merged_from_event"),
            "merge should emit merged notification"
        );
        assert!(
            effects
                .iter()
                .any(|effect| effect.kind == "runtime.nostr.publish_unsigned"),
            "publish should emit runtime publish effect"
        );
    }
}
