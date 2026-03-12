use crate::v2::app_keys::{AppKeys, DeviceEntry};
use crate::{Error, InMemoryStorage, NostrPubSub, Result, StorageAdapter};
use nostr::PublicKey;
use std::collections::HashSet;
use std::sync::Arc;

pub struct AppKeysManager {
    pubsub: Arc<dyn NostrPubSub>,
    storage: Arc<dyn StorageAdapter>,
    app_keys: AppKeys,
    trusted_devices: HashSet<PublicKey>,
    initialized: bool,
    storage_version: String,
}

impl AppKeysManager {
    pub fn new(pubsub: Arc<dyn NostrPubSub>, storage: Option<Arc<dyn StorageAdapter>>) -> Self {
        Self {
            pubsub,
            storage: storage.unwrap_or_else(|| Arc::new(InMemoryStorage::new())),
            app_keys: AppKeys::new(Vec::new()),
            trusted_devices: HashSet::new(),
            initialized: false,
            storage_version: "2".to_string(),
        }
    }

    pub fn start(&mut self) -> Result<()> {
        self.init()
    }

    pub fn init(&mut self) -> Result<()> {
        if self.initialized {
            return Ok(());
        }
        self.initialized = true;

        if let Some(data) = self.storage.get(&self.app_keys_key())? {
            if let Ok(keys) = AppKeys::deserialize(&data) {
                self.app_keys = keys;
            }
        }

        self.trusted_devices = self.load_trusted_devices()?;

        Ok(())
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

    pub fn add_device_key(&mut self, device: DeviceEntry) -> Result<()> {
        self.app_keys.add_device(device);
        self.save_app_keys()
    }

    pub fn update_device_key(&mut self, device: DeviceEntry) -> Result<()> {
        self.app_keys.remove_device(&device.identity_pubkey);
        self.app_keys.add_device(device);
        self.save_app_keys()
    }

    pub fn remove_device_key(&mut self, identity_pubkey: &PublicKey) -> Result<()> {
        self.app_keys.remove_device(identity_pubkey);
        self.trusted_devices.remove(identity_pubkey);
        self.save_app_keys()?;
        self.save_trusted_devices()
    }

    pub fn trust_device_key(&mut self, identity_pubkey: &PublicKey) -> Result<()> {
        if self.app_keys.get_device(identity_pubkey).is_none() {
            return Err(Error::InvalidEvent("Unknown device key".to_string()));
        }
        self.trusted_devices.insert(*identity_pubkey);
        self.save_trusted_devices()
    }

    pub fn revoke_device_key(&mut self, identity_pubkey: &PublicKey) -> Result<()> {
        self.trusted_devices.remove(identity_pubkey);
        self.save_trusted_devices()
    }

    pub fn is_device_key_trusted(&self, identity_pubkey: &PublicKey) -> bool {
        self.trusted_devices.contains(identity_pubkey)
    }

    pub fn set_app_keys(&mut self, app_keys: AppKeys) -> Result<()> {
        self.app_keys = app_keys;
        self.trusted_devices
            .retain(|pk| self.app_keys.get_device(pk).is_some());
        self.save_app_keys()?;
        self.save_trusted_devices()
    }

    pub fn publish(&self, owner_pubkey: PublicKey) -> Result<()> {
        let event = self.app_keys.get_event(owner_pubkey);
        self.pubsub.publish(event)?;
        Ok(())
    }

    pub fn close(&mut self) {}

    fn app_keys_key(&self) -> String {
        format!("v{}/app-keys-manager/app-keys", self.storage_version)
    }

    fn trusted_devices_key(&self) -> String {
        format!("v{}/app-keys-manager/trusted-devices", self.storage_version)
    }

    fn save_app_keys(&self) -> Result<()> {
        self.storage.put(&self.app_keys_key(), self.app_keys.serialize()?)?;
        Ok(())
    }

    fn save_trusted_devices(&self) -> Result<()> {
        let mut trusted = self
            .trusted_devices
            .iter()
            .map(|pk| hex::encode(pk.to_bytes()))
            .collect::<Vec<_>>();
        trusted.sort_unstable();
        self.storage
            .put(&self.trusted_devices_key(), serde_json::to_string(&trusted)?)?;
        Ok(())
    }

    fn load_trusted_devices(&self) -> Result<HashSet<PublicKey>> {
        let Some(data) = self.storage.get(&self.trusted_devices_key())? else {
            return Ok(HashSet::new());
        };

        let trusted_hex = serde_json::from_str::<Vec<String>>(&data).unwrap_or_default();
        Ok(trusted_hex
            .into_iter()
            .filter_map(|pk_hex| crate::utils::pubkey_from_hex(&pk_hex).ok())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{AppKeysManager, DeviceEntry};
    use crate::{InMemoryStorage, NostrPubSub, Result, StorageAdapter};
    use nostr::{Event, PublicKey, UnsignedEvent};
    use std::sync::Arc;

    struct NoopPubSub;

    impl NostrPubSub for NoopPubSub {
        fn publish(&self, _event: UnsignedEvent) -> Result<()> {
            Ok(())
        }

        fn publish_signed(&self, _event: Event) -> Result<()> {
            Ok(())
        }

        fn subscribe(&self, _subid: String, _filter_json: String) -> Result<()> {
            Ok(())
        }

        fn unsubscribe(&self, _subid: String) -> Result<()> {
            Ok(())
        }

        fn decrypted_message(
            &self,
            _sender: PublicKey,
            _sender_device: Option<PublicKey>,
            _content: String,
            _event_id: Option<String>,
        ) -> Result<()> {
            Ok(())
        }

        fn received_event(&self, _event: Event) -> Result<()> {
            Ok(())
        }
    }

    fn make_pubkey() -> PublicKey {
        nostr::Keys::generate().public_key()
    }

    #[test]
    fn update_device_key_replaces_existing_entry() {
        let pubsub: Arc<dyn NostrPubSub> = Arc::new(NoopPubSub);
        let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
        let mut manager = AppKeysManager::new(pubsub, Some(storage));
        manager.start().expect("manager should initialize");

        let pk = make_pubkey();
        manager
            .add_device_key(DeviceEntry {
                identity_pubkey: pk,
                created_at: 1,
            })
            .expect("initial add should succeed");
        manager
            .update_device_key(DeviceEntry {
                identity_pubkey: pk,
                created_at: 2,
            })
            .expect("update should succeed");

        let entry = manager
            .get_device_key(&pk)
            .expect("device should exist after update");
        assert_eq!(entry.created_at, 2);
    }

    #[test]
    fn trusted_state_persists_and_is_pruned_on_removal() {
        let pubsub: Arc<dyn NostrPubSub> = Arc::new(NoopPubSub);
        let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());

        let pk = make_pubkey();

        let mut manager = AppKeysManager::new(pubsub.clone(), Some(storage.clone()));
        manager.start().expect("manager should initialize");
        manager
            .add_device_key(DeviceEntry {
                identity_pubkey: pk,
                created_at: 42,
            })
            .expect("add should succeed");
        manager
            .trust_device_key(&pk)
            .expect("trust should succeed for existing key");

        let mut reloaded = AppKeysManager::new(pubsub, Some(storage));
        reloaded.start().expect("reloaded manager should initialize");
        assert!(reloaded.is_device_key_trusted(&pk));

        reloaded
            .remove_device_key(&pk)
            .expect("remove should succeed");
        assert!(!reloaded.is_device_key_trusted(&pk));
        assert!(reloaded.get_device_key(&pk).is_none());
    }
}
