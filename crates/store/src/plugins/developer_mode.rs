use super::*;
use crate::{App, Store, StoreError};
use serde::{Deserialize, Serialize};

pub const PLUGIN_DEVELOPER_MODE_SETTING_KEY: &str = "plugins.developer_mode";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginDeveloperMode {
    pub enabled: bool,
    pub revision: i64,
    pub updated_at: Option<i64>,
    pub updated_by: Option<String>,
}

pub(super) fn is_enabled(app: &App<'_>) -> Result<bool, StoreError> {
    Ok(app.plugin_developer_mode()?.enabled)
}

impl App<'_> {
    pub fn plugin_developer_mode(&self) -> Result<PluginDeveloperMode, StoreError> {
        match self.get_setting(PLUGIN_DEVELOPER_MODE_SETTING_KEY)? {
            Some(raw) => Ok(serde_json::from_str(&raw)?),
            None => Ok(PluginDeveloperMode::default()),
        }
    }

    pub fn set_plugin_developer_mode(
        &self,
        enabled: bool,
        expected_revision: i64,
        actor: &str,
    ) -> Result<PluginDeveloperMode, StoreError> {
        if actor.trim().is_empty() || actor.len() > 256 {
            return Err(PluginError::Invalid("invalid developer mode actor").into());
        }
        let tx = crate::write_tx(self.app)?;
        let current = self.plugin_developer_mode()?;
        if expected_revision < 0 || current.revision != expected_revision {
            return Err(PluginError::Conflict.into());
        }
        let next_revision = current
            .revision
            .checked_add(1)
            .ok_or(PluginError::Conflict)?;
        let next = PluginDeveloperMode {
            enabled,
            revision: next_revision,
            updated_at: Some(crate::unix_now()),
            updated_by: Some(actor.to_string()),
        };
        self.put_setting(
            PLUGIN_DEVELOPER_MODE_SETTING_KEY,
            &serde_json::to_string(&next)?,
        )?;
        tx.commit()?;
        Ok(next)
    }
}

impl Store {
    pub fn plugin_developer_mode(&self) -> Result<PluginDeveloperMode, StoreError> {
        self.with_app_read(|app| app.plugin_developer_mode())
    }

    pub fn set_plugin_developer_mode(
        &self,
        enabled: bool,
        expected_revision: i64,
        actor: &str,
    ) -> Result<PluginDeveloperMode, StoreError> {
        self.with_app(|app| app.set_plugin_developer_mode(enabled, expected_revision, actor))
    }
}
