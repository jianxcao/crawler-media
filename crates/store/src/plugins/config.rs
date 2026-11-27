use super::*;
use crate::{App, Store, StoreError};
use rusqlite::{OptionalExtension, params};

pub(super) fn current(app: &App<'_>, id: &str, revision: i64) -> Result<PluginRecord, StoreError> {
    let record = app.get_plugin(id)?.ok_or(PluginError::NotFound)?;
    if record.revision != revision
        || record.revision == i64::MAX
        || record.generation == i64::MAX
        || record.config_revision == i64::MAX
    {
        return Err(PluginError::Conflict.into());
    }
    Ok(record)
}

pub(super) fn drained(record: &PluginRecord) -> Result<(), StoreError> {
    if !matches!(
        record.lifecycle,
        PluginLifecycle::InstalledDisabled | PluginLifecycle::Disabled
    ) || record.desired_enabled
        || record.active_operation_id.is_some()
    {
        return Err(PluginError::NotDrained.into());
    }
    Ok(())
}

impl App<'_> {
    pub fn get_plugin_config(&self, id: &str) -> Result<Option<PluginConfig>, StoreError> {
        let row: Option<(String,i64,String)> = self.app.query_row("SELECT schema_revision,config_revision,public_json FROM plugin_configs WHERE plugin_id=?1", [id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        row.map(|(schema_revision, revision, json)| {
            Ok(PluginConfig {
                schema_revision,
                revision,
                values: serde_json::from_str(&json)?,
            })
        })
        .transpose()
    }
    pub fn save_plugin_config(
        &self,
        id: &str,
        revision: i64,
        schema: &str,
        values: &serde_json::Value,
    ) -> Result<PluginRecord, StoreError> {
        let json = serde_json::to_string(values)?;
        let tx = crate::write_tx(self.app)?;
        let p = current(self, id, revision)?;
        drained(&p)?;
        tx.execute("INSERT INTO plugin_configs VALUES (?1,?2,?3,?4) ON CONFLICT(plugin_id) DO UPDATE SET schema_revision=excluded.schema_revision,config_revision=excluded.config_revision,public_json=excluded.public_json", params![id,schema,p.config_revision+1,json])?;
        tx.execute("UPDATE plugins SET revision=revision+1,config_revision=config_revision+1 WHERE plugin_id=?1", [id])?;
        let record = self.get_plugin(id)?.ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(record)
    }
}

impl Store {
    pub fn get_plugin_config(&self, id: &str) -> Result<Option<PluginConfig>, StoreError> {
        self.with_app_read(|app| app.get_plugin_config(id))
    }
    pub fn save_plugin_config(
        &self,
        id: &str,
        revision: i64,
        schema: &str,
        values: &serde_json::Value,
    ) -> Result<PluginRecord, StoreError> {
        self.with_app(|app| app.save_plugin_config(id, revision, schema, values))
    }
}
