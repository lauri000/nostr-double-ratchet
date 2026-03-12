use crate::v2::app_keys::{AppKeys, DeviceEntry};
use crate::{utils, Error, Result};
use nostr::{Event, PublicKey, UnsignedEvent};
use std::collections::HashSet;

pub const APP_KEYS_STORAGE_KEY: &str = "v2/app-keys-manager/app-keys";
pub const TRUSTED_DEVICES_STORAGE_KEY: &str = "v2/app-keys-manager/trusted-devices";

#[derive(Debug, Clone)]
pub enum AppKeysInput {
    Start,
    SetAppKeys {
        app_keys: AppKeys,
    },
    AddDeviceKey {
        device: DeviceEntry,
    },
    UpdateDeviceKey {
        device: DeviceEntry,
    },
    RemoveDeviceKey {
        identity_pubkey: PublicKey,
    },
    TrustDeviceKey {
        identity_pubkey: PublicKey,
    },
    RevokeDeviceKey {
        identity_pubkey: PublicKey,
    },
    PublishAppKeys {
        owner_pubkey: PublicKey,
    },
    ApplyAppKeysEvent {
        event: Event,
    },
    StorageReadResult {
        key: String,
        value: Option<String>,
        error: Option<String>,
    },
    StorageWriteResult {
        key: String,
        success: bool,
        error: Option<String>,
    },
    StorageDeleteResult {
        key: String,
        success: bool,
        error: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub enum AppKeysEffect {
    RequestStorageRead { key: String },
    RequestStorageWrite { key: String, value: String },
    RequestStorageDelete { key: String },
    RequestPublishUnsigned { event: UnsignedEvent },
    Notify(AppKeysNotification),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppKeysNotification {
    Started,
    StartIgnoredAlreadyInitialized,
    DeviceAdded {
        identity_pubkey: PublicKey,
    },
    DeviceAddIgnoredAlreadyExists {
        identity_pubkey: PublicKey,
    },
    DeviceUpdated {
        identity_pubkey: PublicKey,
    },
    DeviceRemoved {
        identity_pubkey: PublicKey,
    },
    DeviceRemoveIgnoredMissing {
        identity_pubkey: PublicKey,
    },
    DeviceTrusted {
        identity_pubkey: PublicKey,
    },
    DeviceTrustIgnoredAlreadyTrusted {
        identity_pubkey: PublicKey,
    },
    DeviceRevoked {
        identity_pubkey: PublicKey,
    },
    DeviceRevokeIgnoredNotTrusted {
        identity_pubkey: PublicKey,
    },
    AppKeysSet,
    AppKeysMergedFromEvent,
    AppKeysEventIgnoredNoStateChange,
    StorageReadApplied {
        key: String,
    },
    StorageReadIgnoredUnknownKey {
        key: String,
    },
    StorageReadFailed {
        key: String,
        error: String,
    },
    StorageWriteAcknowledged {
        key: String,
        success: bool,
        error: Option<String>,
    },
    StorageDeleteAcknowledged {
        key: String,
        success: bool,
        error: Option<String>,
    },
    PublishRequested,
}

#[derive(Debug, Clone)]
pub struct AppKeysManager {
    app_keys: AppKeys,
    trusted_devices: HashSet<PublicKey>,
    initialized: bool,
    app_keys_loaded: bool,
}

impl Default for AppKeysManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AppKeysManager {
    pub fn new() -> Self {
        Self {
            app_keys: AppKeys::new(Vec::new()),
            trusted_devices: HashSet::new(),
            initialized: false,
            app_keys_loaded: false,
        }
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    pub fn get_app_keys(&self) -> &AppKeys {
        &self.app_keys
    }

    pub fn list_device_keys(&self) -> Vec<DeviceEntry> {
        self.app_keys.get_all_devices()
    }

    pub fn get_device_key(&self, identity_pubkey: &PublicKey) -> Option<&DeviceEntry> {
        self.app_keys.get_device(identity_pubkey)
    }

    pub fn trusted_device_keys(&self) -> Vec<PublicKey> {
        self.trusted_devices.iter().copied().collect()
    }

    pub fn is_device_key_trusted(&self, identity_pubkey: &PublicKey) -> bool {
        self.trusted_devices.contains(identity_pubkey)
    }

    pub fn apply(&mut self, input: AppKeysInput) -> Result<Vec<AppKeysEffect>> {
        match input {
            AppKeysInput::Start => self.apply_start(),
            AppKeysInput::SetAppKeys { app_keys } => self.apply_set_app_keys(app_keys),
            AppKeysInput::AddDeviceKey { device } => self.apply_add_device_key(device),
            AppKeysInput::UpdateDeviceKey { device } => self.apply_update_device_key(device),
            AppKeysInput::RemoveDeviceKey { identity_pubkey } => {
                self.apply_remove_device_key(identity_pubkey)
            }
            AppKeysInput::TrustDeviceKey { identity_pubkey } => {
                self.apply_trust_device_key(identity_pubkey)
            }
            AppKeysInput::RevokeDeviceKey { identity_pubkey } => {
                self.apply_revoke_device_key(identity_pubkey)
            }
            AppKeysInput::PublishAppKeys { owner_pubkey } => self.apply_publish(owner_pubkey),
            AppKeysInput::ApplyAppKeysEvent { event } => self.apply_app_keys_event(event),
            AppKeysInput::StorageReadResult { key, value, error } => {
                self.apply_storage_read_result(key, value, error)
            }
            AppKeysInput::StorageWriteResult {
                key,
                success,
                error,
            } => Ok(vec![AppKeysEffect::Notify(
                AppKeysNotification::StorageWriteAcknowledged {
                    key,
                    success,
                    error,
                },
            )]),
            AppKeysInput::StorageDeleteResult {
                key,
                success,
                error,
            } => Ok(vec![AppKeysEffect::Notify(
                AppKeysNotification::StorageDeleteAcknowledged {
                    key,
                    success,
                    error,
                },
            )]),
        }
    }

    fn apply_start(&mut self) -> Result<Vec<AppKeysEffect>> {
        if self.initialized {
            return Ok(vec![AppKeysEffect::Notify(
                AppKeysNotification::StartIgnoredAlreadyInitialized,
            )]);
        }

        self.initialized = true;
        self.app_keys_loaded = false;
        Ok(vec![
            AppKeysEffect::RequestStorageRead {
                key: APP_KEYS_STORAGE_KEY.to_string(),
            },
            AppKeysEffect::RequestStorageRead {
                key: TRUSTED_DEVICES_STORAGE_KEY.to_string(),
            },
            AppKeysEffect::Notify(AppKeysNotification::Started),
        ])
    }

    fn apply_set_app_keys(&mut self, app_keys: AppKeys) -> Result<Vec<AppKeysEffect>> {
        self.app_keys = app_keys;
        self.app_keys_loaded = true;
        self.trusted_devices
            .retain(|pk| self.app_keys.get_device(pk).is_some());

        Ok(vec![
            self.request_storage_write_app_keys()?,
            self.request_storage_write_trusted_devices()?,
            AppKeysEffect::Notify(AppKeysNotification::AppKeysSet),
        ])
    }

    fn apply_add_device_key(&mut self, device: DeviceEntry) -> Result<Vec<AppKeysEffect>> {
        if self.app_keys.get_device(&device.identity_pubkey).is_some() {
            return Ok(vec![AppKeysEffect::Notify(
                AppKeysNotification::DeviceAddIgnoredAlreadyExists {
                    identity_pubkey: device.identity_pubkey,
                },
            )]);
        }

        let identity_pubkey = device.identity_pubkey;
        self.app_keys.add_device(device);
        self.app_keys_loaded = true;
        Ok(vec![
            self.request_storage_write_app_keys()?,
            AppKeysEffect::Notify(AppKeysNotification::DeviceAdded { identity_pubkey }),
        ])
    }

    fn apply_update_device_key(&mut self, device: DeviceEntry) -> Result<Vec<AppKeysEffect>> {
        let identity_pubkey = device.identity_pubkey;
        self.app_keys.remove_device(&identity_pubkey);
        self.app_keys.add_device(device);
        self.app_keys_loaded = true;
        Ok(vec![
            self.request_storage_write_app_keys()?,
            AppKeysEffect::Notify(AppKeysNotification::DeviceUpdated { identity_pubkey }),
        ])
    }

    fn apply_remove_device_key(
        &mut self,
        identity_pubkey: PublicKey,
    ) -> Result<Vec<AppKeysEffect>> {
        let existed = self.app_keys.get_device(&identity_pubkey).is_some();
        if existed {
            self.app_keys.remove_device(&identity_pubkey);
            self.app_keys_loaded = true;
        }
        let trusted_removed = self.trusted_devices.remove(&identity_pubkey);

        let mut effects = Vec::new();
        if existed {
            effects.push(self.request_storage_write_app_keys()?);
            effects.push(AppKeysEffect::Notify(AppKeysNotification::DeviceRemoved {
                identity_pubkey,
            }));
        } else {
            effects.push(AppKeysEffect::Notify(
                AppKeysNotification::DeviceRemoveIgnoredMissing { identity_pubkey },
            ));
        }
        if trusted_removed {
            effects.push(self.request_storage_write_trusted_devices()?);
            effects.push(AppKeysEffect::Notify(AppKeysNotification::DeviceRevoked {
                identity_pubkey,
            }));
        }

        Ok(effects)
    }

    fn apply_trust_device_key(&mut self, identity_pubkey: PublicKey) -> Result<Vec<AppKeysEffect>> {
        if self.app_keys.get_device(&identity_pubkey).is_none() {
            return Err(Error::InvalidEvent("Unknown device key".to_string()));
        }

        if !self.trusted_devices.insert(identity_pubkey) {
            return Ok(vec![AppKeysEffect::Notify(
                AppKeysNotification::DeviceTrustIgnoredAlreadyTrusted { identity_pubkey },
            )]);
        }

        Ok(vec![
            self.request_storage_write_trusted_devices()?,
            AppKeysEffect::Notify(AppKeysNotification::DeviceTrusted { identity_pubkey }),
        ])
    }

    fn apply_revoke_device_key(
        &mut self,
        identity_pubkey: PublicKey,
    ) -> Result<Vec<AppKeysEffect>> {
        if !self.trusted_devices.remove(&identity_pubkey) {
            return Ok(vec![AppKeysEffect::Notify(
                AppKeysNotification::DeviceRevokeIgnoredNotTrusted { identity_pubkey },
            )]);
        }

        Ok(vec![
            self.request_storage_write_trusted_devices()?,
            AppKeysEffect::Notify(AppKeysNotification::DeviceRevoked { identity_pubkey }),
        ])
    }

    fn apply_publish(&self, owner_pubkey: PublicKey) -> Result<Vec<AppKeysEffect>> {
        let event = self.app_keys.get_event(owner_pubkey);
        Ok(vec![
            AppKeysEffect::RequestPublishUnsigned { event },
            AppKeysEffect::Notify(AppKeysNotification::PublishRequested),
        ])
    }

    fn apply_app_keys_event(&mut self, event: Event) -> Result<Vec<AppKeysEffect>> {
        let incoming = AppKeys::from_event(&event)?;
        let merged = self.app_keys.merge(&incoming);
        let previous_state = self.app_keys.serialize()?;
        let merged_state = merged.serialize()?;

        if previous_state == merged_state {
            return Ok(vec![AppKeysEffect::Notify(
                AppKeysNotification::AppKeysEventIgnoredNoStateChange,
            )]);
        }

        self.app_keys = merged;
        self.app_keys_loaded = true;
        self.trusted_devices
            .retain(|pk| self.app_keys.get_device(pk).is_some());

        Ok(vec![
            self.request_storage_write_app_keys()?,
            self.request_storage_write_trusted_devices()?,
            AppKeysEffect::Notify(AppKeysNotification::AppKeysMergedFromEvent),
        ])
    }

    fn apply_storage_read_result(
        &mut self,
        key: String,
        value: Option<String>,
        error: Option<String>,
    ) -> Result<Vec<AppKeysEffect>> {
        if let Some(error) = error {
            if key == APP_KEYS_STORAGE_KEY {
                self.app_keys_loaded = true;
                self.trusted_devices
                    .retain(|pk| self.app_keys.get_device(pk).is_some());
            }
            return Ok(vec![AppKeysEffect::Notify(
                AppKeysNotification::StorageReadFailed { key, error },
            )]);
        }

        match key.as_str() {
            APP_KEYS_STORAGE_KEY => {
                self.app_keys = match value {
                    Some(json) => AppKeys::deserialize(&json)?,
                    None => AppKeys::new(Vec::new()),
                };
                self.app_keys_loaded = true;
                self.trusted_devices
                    .retain(|pk| self.app_keys.get_device(pk).is_some());
                Ok(vec![AppKeysEffect::Notify(
                    AppKeysNotification::StorageReadApplied { key },
                )])
            }
            TRUSTED_DEVICES_STORAGE_KEY => {
                let trusted_pubkeys = match value {
                    Some(json) => serde_json::from_str::<Vec<String>>(&json)?
                        .into_iter()
                        .filter_map(|pk_hex| utils::pubkey_from_hex(&pk_hex).ok())
                        .collect::<HashSet<_>>(),
                    None => HashSet::new(),
                };
                self.trusted_devices = if self.app_keys_loaded {
                    trusted_pubkeys
                        .into_iter()
                        .filter(|pk| self.app_keys.get_device(pk).is_some())
                        .collect()
                } else {
                    trusted_pubkeys
                };

                Ok(vec![AppKeysEffect::Notify(
                    AppKeysNotification::StorageReadApplied { key },
                )])
            }
            _ => Ok(vec![AppKeysEffect::Notify(
                AppKeysNotification::StorageReadIgnoredUnknownKey { key },
            )]),
        }
    }

    fn request_storage_write_app_keys(&self) -> Result<AppKeysEffect> {
        Ok(AppKeysEffect::RequestStorageWrite {
            key: APP_KEYS_STORAGE_KEY.to_string(),
            value: self.app_keys.serialize()?,
        })
    }

    fn request_storage_write_trusted_devices(&self) -> Result<AppKeysEffect> {
        let mut trusted = self
            .trusted_devices
            .iter()
            .map(|pk| hex::encode(pk.to_bytes()))
            .collect::<Vec<_>>();
        trusted.sort_unstable();

        Ok(AppKeysEffect::RequestStorageWrite {
            key: TRUSTED_DEVICES_STORAGE_KEY.to_string(),
            value: serde_json::to_string(&trusted)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AppKeysEffect, AppKeysInput, AppKeysManager, AppKeysNotification, DeviceEntry,
        APP_KEYS_STORAGE_KEY, TRUSTED_DEVICES_STORAGE_KEY,
    };
    use crate::{v2::app_keys::AppKeys, Error, APP_KEYS_EVENT_KIND};
    use nostr::{Event, EventBuilder, Kind, PublicKey, Tag};

    fn make_pubkey() -> PublicKey {
        nostr::Keys::generate().public_key()
    }

    fn collect_notifications(effects: &[AppKeysEffect]) -> Vec<&AppKeysNotification> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                AppKeysEffect::Notify(notification) => Some(notification),
                _ => None,
            })
            .collect()
    }

    fn make_signed_app_keys_event(devices: Vec<DeviceEntry>) -> Event {
        let owner_keys = nostr::Keys::generate();
        let app_keys = AppKeys::new(devices);
        app_keys
            .get_event(owner_keys.public_key())
            .sign_with_keys(&owner_keys)
            .expect("signing app keys event should succeed")
    }

    #[test]
    fn start_emits_storage_reads_once() {
        let mut manager = AppKeysManager::new();
        let first = manager
            .apply(AppKeysInput::Start)
            .expect("start should succeed");

        assert!(manager.is_initialized());
        assert!(matches!(
            first.first(),
            Some(AppKeysEffect::RequestStorageRead { key }) if key == APP_KEYS_STORAGE_KEY
        ));
        assert!(matches!(
            first.get(1),
            Some(AppKeysEffect::RequestStorageRead { key }) if key == TRUSTED_DEVICES_STORAGE_KEY
        ));
        assert!(matches!(
            first.get(2),
            Some(AppKeysEffect::Notify(AppKeysNotification::Started))
        ));

        let second = manager
            .apply(AppKeysInput::Start)
            .expect("second start should succeed");
        assert_eq!(second.len(), 1);
        assert!(matches!(
            second.first(),
            Some(AppKeysEffect::Notify(
                AppKeysNotification::StartIgnoredAlreadyInitialized
            ))
        ));
    }

    #[test]
    fn add_device_key_changes_state_and_emits_storage_write() {
        let mut manager = AppKeysManager::new();
        let pk = make_pubkey();
        let effects = manager
            .apply(AppKeysInput::AddDeviceKey {
                device: DeviceEntry {
                    identity_pubkey: pk,
                    created_at: 11,
                },
            })
            .expect("add should succeed");

        let device = manager
            .get_device_key(&pk)
            .expect("device should exist after add");
        assert_eq!(device.created_at, 11);

        assert!(matches!(
            effects.first(),
            Some(AppKeysEffect::RequestStorageWrite { key, .. }) if key == APP_KEYS_STORAGE_KEY
        ));
        assert!(matches!(
            effects.get(1),
            Some(AppKeysEffect::Notify(AppKeysNotification::DeviceAdded { identity_pubkey })) if *identity_pubkey == pk
        ));
    }

    #[test]
    fn add_existing_device_key_is_noop_without_storage_write() {
        let mut manager = AppKeysManager::new();
        let pk = make_pubkey();
        manager
            .apply(AppKeysInput::AddDeviceKey {
                device: DeviceEntry {
                    identity_pubkey: pk,
                    created_at: 10,
                },
            })
            .expect("first add should succeed");

        let effects = manager
            .apply(AppKeysInput::AddDeviceKey {
                device: DeviceEntry {
                    identity_pubkey: pk,
                    created_at: 99,
                },
            })
            .expect("second add should succeed");

        assert_eq!(
            manager
                .get_device_key(&pk)
                .expect("device should still exist")
                .created_at,
            10
        );
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            effects.first(),
            Some(AppKeysEffect::Notify(
                AppKeysNotification::DeviceAddIgnoredAlreadyExists { identity_pubkey }
            )) if *identity_pubkey == pk
        ));
    }

    #[test]
    fn trust_unknown_device_key_returns_error() {
        let mut manager = AppKeysManager::new();
        let unknown = make_pubkey();
        let err = manager
            .apply(AppKeysInput::TrustDeviceKey {
                identity_pubkey: unknown,
            })
            .expect_err("trusting unknown key should fail");

        assert!(matches!(err, Error::InvalidEvent(_)));
    }

    #[test]
    fn remove_device_key_prunes_trust_and_emits_both_storage_writes() {
        let mut manager = AppKeysManager::new();
        let pk = make_pubkey();
        manager
            .apply(AppKeysInput::AddDeviceKey {
                device: DeviceEntry {
                    identity_pubkey: pk,
                    created_at: 2,
                },
            })
            .expect("add should succeed");
        manager
            .apply(AppKeysInput::TrustDeviceKey {
                identity_pubkey: pk,
            })
            .expect("trust should succeed");

        let effects = manager
            .apply(AppKeysInput::RemoveDeviceKey {
                identity_pubkey: pk,
            })
            .expect("remove should succeed");

        assert!(manager.get_device_key(&pk).is_none());
        assert!(!manager.is_device_key_trusted(&pk));

        let storage_writes = effects
            .iter()
            .filter(|effect| matches!(effect, AppKeysEffect::RequestStorageWrite { .. }))
            .count();
        assert_eq!(storage_writes, 2);

        let notifications = collect_notifications(&effects);
        assert!(notifications.iter().any(|notification| {
            matches!(
                notification,
                AppKeysNotification::DeviceRemoved { identity_pubkey } if *identity_pubkey == pk
            )
        }));
        assert!(notifications.iter().any(|notification| {
            matches!(
                notification,
                AppKeysNotification::DeviceRevoked { identity_pubkey } if *identity_pubkey == pk
            )
        }));
    }

    #[test]
    fn set_app_keys_prunes_unknown_trust_and_emits_writes() {
        let mut manager = AppKeysManager::new();
        let pk_a = make_pubkey();
        let pk_b = make_pubkey();

        manager
            .apply(AppKeysInput::AddDeviceKey {
                device: DeviceEntry {
                    identity_pubkey: pk_a,
                    created_at: 1,
                },
            })
            .expect("add a should succeed");
        manager
            .apply(AppKeysInput::AddDeviceKey {
                device: DeviceEntry {
                    identity_pubkey: pk_b,
                    created_at: 2,
                },
            })
            .expect("add b should succeed");
        manager
            .apply(AppKeysInput::TrustDeviceKey {
                identity_pubkey: pk_a,
            })
            .expect("trust a should succeed");
        manager
            .apply(AppKeysInput::TrustDeviceKey {
                identity_pubkey: pk_b,
            })
            .expect("trust b should succeed");

        let effects = manager
            .apply(AppKeysInput::SetAppKeys {
                app_keys: AppKeys::new(vec![DeviceEntry {
                    identity_pubkey: pk_a,
                    created_at: 3,
                }]),
            })
            .expect("set should succeed");

        assert!(manager.get_device_key(&pk_a).is_some());
        assert!(manager.get_device_key(&pk_b).is_none());
        assert!(manager.is_device_key_trusted(&pk_a));
        assert!(!manager.is_device_key_trusted(&pk_b));
        assert!(effects.iter().any(|effect| matches!(
            effect,
            AppKeysEffect::Notify(AppKeysNotification::AppKeysSet)
        )));
    }

    #[test]
    fn apply_app_keys_event_updates_state_and_emits_storage_write() {
        let mut manager = AppKeysManager::new();
        let local_pk = make_pubkey();
        manager
            .apply(AppKeysInput::AddDeviceKey {
                device: DeviceEntry {
                    identity_pubkey: local_pk,
                    created_at: 10,
                },
            })
            .expect("local add should succeed");

        let remote_pk = make_pubkey();
        let event = make_signed_app_keys_event(vec![DeviceEntry {
            identity_pubkey: remote_pk,
            created_at: 20,
        }]);
        let effects = manager
            .apply(AppKeysInput::ApplyAppKeysEvent { event })
            .expect("event apply should succeed");

        assert!(manager.get_device_key(&local_pk).is_some());
        assert!(manager.get_device_key(&remote_pk).is_some());
        assert!(effects.iter().any(|effect| matches!(
            effect,
            AppKeysEffect::RequestStorageWrite { key, .. } if key == APP_KEYS_STORAGE_KEY
        )));
        assert!(effects.iter().any(|effect| matches!(
            effect,
            AppKeysEffect::Notify(AppKeysNotification::AppKeysMergedFromEvent)
        )));
    }

    #[test]
    fn storage_read_results_apply_state_and_filter_unknown_trusted_keys() {
        let mut manager = AppKeysManager::new();
        let known_pk = make_pubkey();
        let unknown_pk = make_pubkey();

        let app_keys_json = AppKeys::new(vec![DeviceEntry {
            identity_pubkey: known_pk,
            created_at: 5,
        }])
        .serialize()
        .expect("app keys should serialize");

        let trusted_json = serde_json::to_string(&vec![
            hex::encode(known_pk.to_bytes()),
            hex::encode(unknown_pk.to_bytes()),
        ])
        .expect("trusted list should serialize");

        manager
            .apply(AppKeysInput::StorageReadResult {
                key: APP_KEYS_STORAGE_KEY.to_string(),
                value: Some(app_keys_json),
                error: None,
            })
            .expect("app keys read callback should succeed");
        manager
            .apply(AppKeysInput::StorageReadResult {
                key: TRUSTED_DEVICES_STORAGE_KEY.to_string(),
                value: Some(trusted_json),
                error: None,
            })
            .expect("trusted read callback should succeed");

        assert!(manager.get_device_key(&known_pk).is_some());
        assert!(manager.is_device_key_trusted(&known_pk));
        assert!(!manager.is_device_key_trusted(&unknown_pk));
    }

    #[test]
    fn trusted_storage_read_before_app_keys_storage_read_keeps_valid_trust() {
        let mut manager = AppKeysManager::new();
        let known_pk = make_pubkey();
        let unknown_pk = make_pubkey();

        let app_keys_json = AppKeys::new(vec![DeviceEntry {
            identity_pubkey: known_pk,
            created_at: 5,
        }])
        .serialize()
        .expect("app keys should serialize");

        let trusted_json = serde_json::to_string(&vec![
            hex::encode(known_pk.to_bytes()),
            hex::encode(unknown_pk.to_bytes()),
        ])
        .expect("trusted list should serialize");

        manager
            .apply(AppKeysInput::StorageReadResult {
                key: TRUSTED_DEVICES_STORAGE_KEY.to_string(),
                value: Some(trusted_json),
                error: None,
            })
            .expect("trusted read callback should succeed");

        assert!(manager.is_device_key_trusted(&known_pk));
        assert!(manager.is_device_key_trusted(&unknown_pk));

        manager
            .apply(AppKeysInput::StorageReadResult {
                key: APP_KEYS_STORAGE_KEY.to_string(),
                value: Some(app_keys_json),
                error: None,
            })
            .expect("app keys read callback should succeed");

        assert!(manager.is_device_key_trusted(&known_pk));
        assert!(!manager.is_device_key_trusted(&unknown_pk));
    }

    #[test]
    fn publish_input_emits_unsigned_publish_effect() {
        let mut manager = AppKeysManager::new();
        manager
            .apply(AppKeysInput::AddDeviceKey {
                device: DeviceEntry {
                    identity_pubkey: make_pubkey(),
                    created_at: 1,
                },
            })
            .expect("add should succeed");

        let owner_pubkey = make_pubkey();
        let effects = manager
            .apply(AppKeysInput::PublishAppKeys { owner_pubkey })
            .expect("publish input should succeed");

        let publish_effect = effects
            .iter()
            .find_map(|effect| match effect {
                AppKeysEffect::RequestPublishUnsigned { event } => Some(event),
                _ => None,
            })
            .expect("publish effect should exist");

        assert_eq!(publish_effect.kind.as_u16(), APP_KEYS_EVENT_KIND as u16);
        let has_d_tag = publish_effect.tags.iter().any(|tag| {
            let vals = tag.clone().to_vec();
            vals.first().map(|value| value.as_str()) == Some("d")
                && vals.get(1).map(|value| value.as_str()) == Some("double-ratchet/app-keys")
        });
        assert!(has_d_tag, "publish effect should include app-keys d tag");
    }

    #[test]
    fn storage_failure_input_emits_notify_effect() {
        let mut manager = AppKeysManager::new();
        let effects = manager
            .apply(AppKeysInput::StorageReadResult {
                key: APP_KEYS_STORAGE_KEY.to_string(),
                value: None,
                error: Some("timeout".to_string()),
            })
            .expect("storage read failure callback should not fail");

        assert_eq!(effects.len(), 1);
        assert!(matches!(
            effects.first(),
            Some(AppKeysEffect::Notify(AppKeysNotification::StorageReadFailed { key, error }))
                if key == APP_KEYS_STORAGE_KEY && error == "timeout"
        ));
    }

    #[test]
    fn app_keys_event_no_state_change_only_notifies() {
        let owner_keys = nostr::Keys::generate();
        let event = EventBuilder::new(Kind::from(APP_KEYS_EVENT_KIND as u16), "")
            .tags(vec![
                Tag::parse(&["d".to_string(), "double-ratchet/app-keys".to_string()])
                    .expect("d tag should parse"),
                Tag::parse(&["version".to_string(), "1".to_string()])
                    .expect("version tag should parse"),
            ])
            .build(owner_keys.public_key())
            .sign_with_keys(&owner_keys)
            .expect("event signing should succeed");

        let mut manager = AppKeysManager::new();
        let effects = manager
            .apply(AppKeysInput::ApplyAppKeysEvent { event })
            .expect("event should apply successfully");

        assert_eq!(effects.len(), 1);
        assert!(matches!(
            effects.first(),
            Some(AppKeysEffect::Notify(
                AppKeysNotification::AppKeysEventIgnoredNoStateChange
            ))
        ));
    }
}
