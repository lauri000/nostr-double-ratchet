use crate::{DevicePubkey, Result, UnixSeconds};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceEntry {
    pub identity_pubkey: DevicePubkey,
    pub created_at: UnixSeconds,
}

impl DeviceEntry {
    pub fn new(identity_pubkey: DevicePubkey, created_at: u64) -> Self {
        Self {
            identity_pubkey,
            created_at: UnixSeconds(created_at),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceLabels {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_label: Option<String>,
    pub updated_at: UnixSeconds,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppKeys {
    devices: BTreeMap<DevicePubkey, DeviceEntry>,
    device_labels: BTreeMap<DevicePubkey, DeviceLabels>,
}

impl AppKeys {
    pub fn new(devices: Vec<DeviceEntry>) -> Self {
        let mut map = BTreeMap::new();
        for device in devices {
            map.insert(device.identity_pubkey, device);
        }
        Self {
            devices: map,
            device_labels: BTreeMap::new(),
        }
    }

    pub fn add_device(&mut self, device: DeviceEntry) {
        self.devices.entry(device.identity_pubkey).or_insert(device);
    }

    pub fn remove_device(&mut self, identity_pubkey: &DevicePubkey) {
        self.devices.remove(identity_pubkey);
        self.device_labels.remove(identity_pubkey);
    }

    pub fn get_device(&self, identity_pubkey: &DevicePubkey) -> Option<&DeviceEntry> {
        self.devices.get(identity_pubkey)
    }

    pub fn get_all_devices(&self) -> Vec<DeviceEntry> {
        self.devices.values().cloned().collect()
    }

    pub fn set_device_labels(
        &mut self,
        identity_pubkey: DevicePubkey,
        device_label: Option<String>,
        client_label: Option<String>,
        updated_at: UnixSeconds,
    ) {
        self.device_labels.insert(
            identity_pubkey,
            DeviceLabels {
                device_label,
                client_label,
                updated_at,
            },
        );
    }

    pub fn get_device_labels(&self, identity_pubkey: &DevicePubkey) -> Option<&DeviceLabels> {
        self.device_labels.get(identity_pubkey)
    }

    pub fn get_all_device_labels(&self) -> Vec<(DevicePubkey, DeviceLabels)> {
        self.device_labels
            .iter()
            .map(|(identity_pubkey, labels)| (*identity_pubkey, labels.clone()))
            .collect()
    }

    pub fn merge(&self, other: &AppKeys) -> AppKeys {
        let mut merged = BTreeMap::new();
        for device in self
            .get_all_devices()
            .into_iter()
            .chain(other.get_all_devices())
        {
            merged
                .entry(device.identity_pubkey)
                .and_modify(|existing: &mut DeviceEntry| {
                    if device.created_at < existing.created_at {
                        *existing = device.clone();
                    }
                })
                .or_insert(device);
        }

        let mut merged_labels = BTreeMap::new();
        for (identity_pubkey, labels) in self.device_labels.iter().chain(other.device_labels.iter())
        {
            merged_labels
                .entry(*identity_pubkey)
                .and_modify(|existing: &mut DeviceLabels| {
                    if labels.updated_at > existing.updated_at {
                        *existing = labels.clone();
                    }
                })
                .or_insert_with(|| labels.clone());
        }

        merged_labels.retain(|identity_pubkey, _| merged.contains_key(identity_pubkey));
        AppKeys {
            devices: merged,
            device_labels: merged_labels,
        }
    }

    pub fn serialize(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn deserialize(json: &str) -> Result<Self> {
        Ok(serde_json::from_str(json)?)
    }
}
