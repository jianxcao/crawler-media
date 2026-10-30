//! One revisioned app.db configuration for Transfer and Watch directories.
use serde::{Deserialize, Serialize};

use crate::{Store, StoreError};

const KEY: &str = "transfer.settings";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferSettings {
    pub revision: i64,
    pub transfer_mode: String,
    pub scrape: bool,
    pub watch_intake: Option<String>,
    pub watch_inplace: Option<String>,
}

impl Default for TransferSettings {
    fn default() -> Self {
        Self {
            revision: 0,
            transfer_mode: "hardlink".into(),
            scrape: false,
            watch_intake: None,
            watch_inplace: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct TransferSettingsPatch {
    pub transfer_mode: Option<String>,
    pub scrape: Option<bool>,
    pub watch_intake: Option<Option<String>>,
    pub watch_inplace: Option<Option<String>>,
}

impl Store {
    pub fn transfer_settings(&self) -> Result<TransferSettings, StoreError> {
        match self.get_setting(KEY)? {
            Some(raw) => Ok(serde_json::from_str(&raw)?),
            None => Ok(TransferSettings::default()),
        }
    }

    pub fn patch_transfer_settings(
        &self,
        expected_revision: i64,
        patch: TransferSettingsPatch,
    ) -> Result<TransferSettings, StoreError> {
        self.app_write(|store| {
            let mut settings = store.transfer_settings()?;
            if expected_revision < 0 || settings.revision != expected_revision {
                return Err(StoreError::Conflict(
                    "Transfer settings revision changed".into(),
                ));
            }
            if let Some(mode) = patch.transfer_mode {
                if !matches!(mode.as_str(), "hardlink" | "copy" | "move") {
                    return Err(StoreError::Invalid("invalid Transfer mode".into()));
                }
                settings.transfer_mode = mode;
            }
            if let Some(scrape) = patch.scrape {
                settings.scrape = scrape;
            }
            if let Some(path) = patch.watch_intake {
                settings.watch_intake = normalize_path(path);
            }
            if let Some(path) = patch.watch_inplace {
                settings.watch_inplace = normalize_path(path);
            }
            settings.revision = settings
                .revision
                .checked_add(1)
                .ok_or_else(|| StoreError::Invalid("Transfer settings revision overflow".into()))?;
            store.put_setting(KEY, &serde_json::to_string(&settings)?)?;
            tracing::info!(
                revision = settings.revision,
                transfer_mode = settings.transfer_mode,
                scrape = settings.scrape,
                "Transfer settings updated"
            );
            Ok(settings)
        })
    }

    pub fn transfer_mode(&self) -> Result<String, StoreError> {
        Ok(self.transfer_settings()?.transfer_mode)
    }
    pub fn scrape_enabled(&self) -> Result<bool, StoreError> {
        Ok(self.transfer_settings()?.scrape)
    }
    pub fn watch_intake(&self) -> Result<Option<String>, StoreError> {
        Ok(self.transfer_settings()?.watch_intake)
    }
    pub fn watch_inplace(&self) -> Result<Option<String>, StoreError> {
        Ok(self.transfer_settings()?.watch_inplace)
    }

    pub fn set_transfer_mode(&self, mode: &str) -> Result<(), StoreError> {
        self.patch_transfer_settings(
            self.transfer_settings()?.revision,
            TransferSettingsPatch {
                transfer_mode: Some(mode.into()),
                ..Default::default()
            },
        )
        .map(|_| ())
    }
    pub fn set_scrape_enabled(&self, enabled: bool) -> Result<(), StoreError> {
        self.patch_transfer_settings(
            self.transfer_settings()?.revision,
            TransferSettingsPatch {
                scrape: Some(enabled),
                ..Default::default()
            },
        )
        .map(|_| ())
    }
    pub fn set_watch_intake(&self, path: &str) -> Result<(), StoreError> {
        self.patch_transfer_settings(
            self.transfer_settings()?.revision,
            TransferSettingsPatch {
                watch_intake: Some(Some(path.into())),
                ..Default::default()
            },
        )
        .map(|_| ())
    }
    pub fn set_watch_inplace(&self, path: &str) -> Result<(), StoreError> {
        self.patch_transfer_settings(
            self.transfer_settings()?.revision,
            TransferSettingsPatch {
                watch_inplace: Some(Some(path.into())),
                ..Default::default()
            },
        )
        .map(|_| ())
    }
}

fn normalize_path(path: Option<String>) -> Option<String> {
    path.map(|path| path.trim().to_string())
        .filter(|path| !path.is_empty())
}

impl crate::App<'_> {
    pub fn transfer_settings(&self) -> Result<TransferSettings, StoreError> {
        match self.get_setting(KEY)? {
            Some(raw) => Ok(serde_json::from_str(&raw)?),
            None => Ok(TransferSettings::default()),
        }
    }
}
