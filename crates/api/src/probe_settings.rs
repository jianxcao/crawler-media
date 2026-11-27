//! Instance-wide media probe admission settings.
use serde::{Deserialize, Serialize};

use crate::{Store, settings_keys};

pub(crate) const MAX_METADATA_WORKERS: usize = 8;
pub(crate) const MAX_FINGERPRINT_WORKERS: usize = 8;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProbeSettings {
    pub metadata_workers: usize,
    pub remote_concurrency: usize,
    pub remote_cooldown_ms: u64,
}

impl Default for ProbeSettings {
    fn default() -> Self {
        Self {
            metadata_workers: 1,
            remote_concurrency: marker::media_read::DEFAULT_REMOTE_READ_LIMIT,
            remote_cooldown_ms: 500,
        }
    }
}

impl ProbeSettings {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_METADATA_WORKERS).contains(&self.metadata_workers) {
            return Err("媒体信息并发需要在 1~8 之间".into());
        }
        if !(1..=8).contains(&self.remote_concurrency) {
            return Err("远程读取并发需要在 1~8 之间".into());
        }
        if self.remote_cooldown_ms > 60_000 {
            return Err("远程读取间隔需要在 0~60000 毫秒之间".into());
        }
        Ok(())
    }

    pub fn load(store: &Store) -> Result<Self, store::StoreError> {
        let settings = match store.get_setting(settings_keys::PROBE_SETTINGS)? {
            Some(raw) => serde_json::from_str::<Self>(&raw)?,
            None => Self::default(),
        };
        settings.validate().map_err(store::StoreError::Invalid)?;
        Ok(settings)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RemoteReadsSettings {
    pub paused: bool,
}

impl RemoteReadsSettings {
    pub fn load(store: &Store) -> Result<Self, store::StoreError> {
        match store.get_setting(settings_keys::PROBE_REMOTE_READS)? {
            Some(raw) => Ok(serde_json::from_str(&raw)?),
            None => Ok(Self::default()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FingerprintProbeSettings {
    pub workers: usize,
    pub paused: bool,
}

impl Default for FingerprintProbeSettings {
    fn default() -> Self {
        Self {
            workers: 1,
            paused: false,
        }
    }
}

impl FingerprintProbeSettings {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_FINGERPRINT_WORKERS).contains(&self.workers) {
            return Err("片头片尾声纹并发需要在 1~8 之间".into());
        }
        Ok(())
    }

    pub fn load(store: &Store) -> Result<Self, store::StoreError> {
        let settings = match store.get_setting(settings_keys::PROBE_FINGERPRINT)? {
            Some(raw) => serde_json::from_str::<Self>(&raw)?,
            None => Self::default(),
        };
        settings.validate().map_err(store::StoreError::Invalid)?;
        Ok(settings)
    }
}
