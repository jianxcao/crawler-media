use super::*;
use crate::{App, Store, StoreError};
use rusqlite::{OptionalExtension, params};

pub(super) const COLUMNS: &str = "binding_id,plugin_id,task_id,definition_revision,generation,enabled,interval_secs,public_config,projection_revision,projection_generation,job_def_id";
pub(super) fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<PluginTaskBinding> {
    let id: String = r.get(0)?;
    let json: String = r.get(7)?;
    let binding_id = uuid::Uuid::parse_str(&id).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let public_config = serde_json::from_str(&json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(PluginTaskBinding {
        binding_id,
        plugin_id: r.get(1)?,
        task_id: r.get(2)?,
        definition_revision: r.get(3)?,
        generation: r.get(4)?,
        definition: PluginTaskDefinition {
            enabled: r.get(5)?,
            interval_secs: r.get(6)?,
            public_config,
        },
        projection_revision: r.get(8)?,
        projection_generation: r.get(9)?,
        job_def_id: r.get(10)?,
    })
}
pub(super) fn disable(app: &App<'_>, id: &str, generation: i64) -> Result<(), StoreError> {
    let exhausted:bool=app.app.query_row("SELECT EXISTS(SELECT 1 FROM plugin_task_bindings WHERE plugin_id=?1 AND definition_revision=9223372036854775807)",[id],|r|r.get(0))?;
    if exhausted {
        return Err(PluginError::Conflict.into());
    }
    app.app.execute("UPDATE plugin_task_bindings SET enabled=0,generation=?2,definition_revision=definition_revision+1 WHERE plugin_id=?1",params![id,generation])?;
    Ok(())
}
impl App<'_> {
    pub fn get_plugin_task_binding_by_id(
        &self,
        binding_id: uuid::Uuid,
    ) -> Result<Option<PluginTaskBinding>, StoreError> {
        self.app
            .query_row(
                &format!("SELECT {COLUMNS} FROM plugin_task_bindings WHERE binding_id=?1"),
                [binding_id.to_string()],
                row,
            )
            .optional()
            .map_err(Into::into)
    }
    pub fn get_plugin_task_binding(
        &self,
        id: &str,
        task: &str,
    ) -> Result<Option<PluginTaskBinding>, StoreError> {
        self.app
            .query_row(
                &format!(
                    "SELECT {COLUMNS} FROM plugin_task_bindings WHERE plugin_id=?1 AND task_id=?2"
                ),
                params![id, task],
                row,
            )
            .optional()
            .map_err(Into::into)
    }
    pub fn save_plugin_task_binding(
        &self,
        plugin: &PluginRecord,
        task: &str,
        revision: i64,
        definition: &PluginTaskDefinition,
    ) -> Result<PluginTaskBinding, StoreError> {
        if task.is_empty() || task.len() > 256 || definition.interval_secs.is_some_and(|n| n <= 0) {
            return Err(PluginError::Invalid("invalid task definition").into());
        }
        let json = serde_json::to_string(&definition.public_config)?;
        let tx = crate::write_tx(self.app)?;
        let p = super::config::current(self, &plugin.plugin_id, plugin.revision)?;
        if p.generation != plugin.generation || p.package_hash != plugin.package_hash {
            return Err(PluginError::StaleAuthority.into());
        }
        if definition.enabled && p.lifecycle != PluginLifecycle::Enabled {
            return Err(PluginError::StaleAuthority.into());
        }
        let old = self.get_plugin_task_binding(&plugin.plugin_id, task)?;
        if old.as_ref().map_or(0, |b| b.definition_revision) != revision || revision == i64::MAX {
            return Err(PluginError::Conflict.into());
        }
        let binding_id = old.map_or_else(uuid::Uuid::new_v4, |b| b.binding_id);
        tx.execute("INSERT INTO plugin_task_bindings VALUES (?1,?2,?3,?4,?5,?6,?7,?8,0,NULL,NULL) ON CONFLICT(plugin_id,task_id) DO UPDATE SET definition_revision=excluded.definition_revision,generation=excluded.generation,enabled=excluded.enabled,interval_secs=excluded.interval_secs,public_config=excluded.public_config",params![binding_id.to_string(),p.plugin_id,task,revision+1,p.generation,definition.enabled,definition.interval_secs,json])?;
        let binding = self
            .get_plugin_task_binding(&p.plugin_id, task)?
            .ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(binding)
    }
    pub fn pending_plugin_task_sync(
        &self,
        after: Option<uuid::Uuid>,
        count: Option<usize>,
    ) -> Result<Vec<PluginTaskBinding>, StoreError> {
        let mut stmt=self.app.prepare(&format!("SELECT {COLUMNS} FROM plugin_task_bindings WHERE (definition_revision!=projection_revision OR generation IS NOT projection_generation) AND binding_id>?1 ORDER BY binding_id LIMIT ?2"))?;
        Ok(stmt
            .query_map(
                params![
                    after.map(|id| id.to_string()).unwrap_or_default(),
                    super::paging::limit(count) as i64
                ],
                row,
            )?
            .collect::<Result<Vec<_>, _>>()?)
    }
    pub fn acknowledge_plugin_task_sync(
        &self,
        id: uuid::Uuid,
        generation: i64,
        revision: i64,
        job_def_id: Option<&str>,
    ) -> Result<(), StoreError> {
        let changed=self.app.execute("UPDATE plugin_task_bindings SET projection_generation=?2,projection_revision=?3,job_def_id=?4 WHERE binding_id=?1 AND generation=?2 AND definition_revision=?3",params![id.to_string(),generation,revision,job_def_id])?;
        if changed == 0 {
            return Err(PluginError::Conflict.into());
        }
        Ok(())
    }
}
impl Store {
    pub fn get_plugin_task_binding_by_id(
        &self,
        binding_id: uuid::Uuid,
    ) -> Result<Option<PluginTaskBinding>, StoreError> {
        self.with_app_read(|app| app.get_plugin_task_binding_by_id(binding_id))
    }
    pub fn get_plugin_task_binding(
        &self,
        id: &str,
        task: &str,
    ) -> Result<Option<PluginTaskBinding>, StoreError> {
        self.with_app_read(|app| app.get_plugin_task_binding(id, task))
    }
    pub fn save_plugin_task_binding(
        &self,
        p: &PluginRecord,
        task: &str,
        revision: i64,
        d: &PluginTaskDefinition,
    ) -> Result<PluginTaskBinding, StoreError> {
        self.with_app(|app| app.save_plugin_task_binding(p, task, revision, d))
    }
    pub fn pending_plugin_task_sync(
        &self,
        after: Option<uuid::Uuid>,
        count: Option<usize>,
    ) -> Result<Vec<PluginTaskBinding>, StoreError> {
        self.with_app_read(|app| app.pending_plugin_task_sync(after, count))
    }
    pub fn acknowledge_plugin_task_sync(
        &self,
        id: uuid::Uuid,
        generation: i64,
        revision: i64,
        job: Option<&str>,
    ) -> Result<(), StoreError> {
        self.with_app(|app| app.acknowledge_plugin_task_sync(id, generation, revision, job))
    }
}
