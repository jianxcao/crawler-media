use super::*;
use crate::{App, Store, StoreError};
use rusqlite::{OptionalExtension, params};

impl App<'_> {
    /// Save validated public config and explicit secret changes in one app transaction.
    pub fn save_plugin_config_with_secrets(
        &self,
        authority: &PluginConfigAuthority,
        revision: i64,
        schema: &str,
        values: &serde_json::Value,
        updates: &[PluginSecretUpdate],
    ) -> Result<PluginRecord, StoreError> {
        let mut keys = std::collections::BTreeSet::new();
        for update in updates {
            super::state::valid_key(&update.key)?;
            if !keys.insert(&update.key) {
                return Err(PluginError::Invalid("duplicate secret key").into());
            }
            if matches!(&update.change,PluginSecretChange::Replace(bytes) if bytes.len()>65536) {
                return Err(PluginError::Invalid("secret exceeds object limit").into());
            }
        }
        let tx = crate::write_tx(self.app)?;
        super::authority::validate_config(self, &authority.0)?;
        let updated = self.save_plugin_config(&authority.0.plugin_id, revision, schema, values)?;
        let scope = authority.0.scope.key()?;
        for update in updates {
            match &update.change {
                PluginSecretChange::Keep => {}
                PluginSecretChange::Replace(bytes) => {
                    tx.execute("INSERT INTO plugin_config_secrets VALUES (?1,?2,?3,?4) ON CONFLICT(plugin_id,security_scope,key) DO UPDATE SET value=excluded.value",params![authority.0.plugin_id,scope,update.key,bytes])?;
                }
                PluginSecretChange::Clear => {
                    tx.execute("DELETE FROM plugin_config_secrets WHERE plugin_id=?1 AND security_scope=?2 AND key=?3",params![authority.0.plugin_id,scope,update.key])?;
                }
            }
        }
        tx.commit()?;
        Ok(updated)
    }
    /// Administrative projection discloses presence in this scope, never secret bytes.
    pub fn plugin_config_secret_configured(
        &self,
        authority: &PluginConfigAuthority,
        key: &str,
    ) -> Result<bool, StoreError> {
        let tx = crate::read_tx(self.app)?;
        super::authority::validate_config(self, &authority.0)?;
        let result = tx.query_row("SELECT EXISTS(SELECT 1 FROM plugin_config_secrets WHERE plugin_id=?1 AND security_scope=?2 AND key=?3)",params![authority.0.plugin_id,authority.0.scope.key()?,key],|r|r.get(0))?;
        tx.commit()?;
        Ok(result)
    }
    /// Host Admin export only, never a WIT import or ordinary config projection.
    pub fn export_plugin_config_secret(
        &self,
        authority: &PluginConfigAuthority,
        key: &str,
    ) -> Result<Option<PluginScopedBytes>, StoreError> {
        let tx = crate::read_tx(self.app)?;
        super::authority::validate_config(self, &authority.0)?;
        let bytes = tx.query_row("SELECT value FROM plugin_config_secrets WHERE plugin_id=?1 AND security_scope=?2 AND key=?3",params![authority.0.plugin_id,authority.0.scope.key()?,key],|r|r.get::<_,Vec<u8>>(0)).optional()?;
        tx.commit()?;
        Ok(bytes.map(|bytes| PluginScopedBytes {
            scope: authority.0.scope.clone(),
            bytes,
        }))
    }
}
impl Store {
    pub fn save_plugin_config_with_secrets(
        &self,
        a: &PluginConfigAuthority,
        revision: i64,
        schema: &str,
        values: &serde_json::Value,
        updates: &[PluginSecretUpdate],
    ) -> Result<PluginRecord, StoreError> {
        self.with_app(|app| {
            app.save_plugin_config_with_secrets(a, revision, schema, values, updates)
        })
    }
    pub fn plugin_config_secret_configured(
        &self,
        a: &PluginConfigAuthority,
        key: &str,
    ) -> Result<bool, StoreError> {
        self.with_app_read(|app| app.plugin_config_secret_configured(a, key))
    }
    pub fn export_plugin_config_secret(
        &self,
        a: &PluginConfigAuthority,
        key: &str,
    ) -> Result<Option<PluginScopedBytes>, StoreError> {
        self.with_app_read(|app| app.export_plugin_config_secret(a, key))
    }
}
