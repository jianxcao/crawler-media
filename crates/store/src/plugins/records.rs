use super::*;
use crate::{App, Store, StoreError};
use rusqlite::{OptionalExtension, params};

pub(super) const COLUMNS: &str = "plugin_id,active_package_hash,lifecycle,desired_enabled,revision,config_revision,generation,active_operation_id,failure_reason";
pub(super) const VERSION_COLUMNS: &str = "plugin_id,package_hash,version,manifest_json,config_schema_json,state_schema,artifact_path,signing_key_id,signature_status,ui_metadata_ref,credential_bound";

pub(super) fn generation_floor_key(plugin_id: &str) -> String {
    format!("plugin.generation_floor.{plugin_id}")
}

pub(super) fn load_generation_floor(app: &App<'_>, plugin_id: &str) -> Result<i64, StoreError> {
    Ok(app
        .get_setting(&generation_floor_key(plugin_id))?
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|&g| g >= 0)
        .unwrap_or(0))
}

pub(super) fn plugin_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PluginRecord> {
    let raw: String = row.get(2)?;
    let lifecycle = match raw.as_str() {
        "installed-disabled" => PluginLifecycle::InstalledDisabled,
        "enabling" => PluginLifecycle::Enabling,
        "enabled" => PluginLifecycle::Enabled,
        "draining" => PluginLifecycle::Draining,
        "disabled" => PluginLifecycle::Disabled,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    let operation: Option<String> = row.get(7)?;
    let active_operation_id = operation
        .map(|s| uuid::Uuid::parse_str(&s))
        .transpose()
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(e))
        })?;
    Ok(PluginRecord {
        plugin_id: row.get(0)?,
        package_hash: row.get(1)?,
        lifecycle,
        desired_enabled: row.get(3)?,
        revision: row.get(4)?,
        config_revision: row.get(5)?,
        generation: row.get(6)?,
        active_operation_id,
        failure_reason: row.get(8)?,
    })
}

pub(super) fn version_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<PluginVersionMetadata> {
    Ok(PluginVersionMetadata {
        plugin_id: r.get(0)?,
        package_hash: r.get(1)?,
        version: r.get(2)?,
        manifest_json: r.get(3)?,
        config_schema_json: r.get(4)?,
        state_schema: r.get(5)?,
        artifact_path: r.get(6)?,
        signing_key_id: r.get(7)?,
        signature_status: r.get(8)?,
        ui_metadata_ref: r.get(9)?,
        credential_bound: r.get(10)?,
    })
}

impl App<'_> {
    pub fn get_plugin(&self, id: &str) -> Result<Option<PluginRecord>, StoreError> {
        self.app
            .query_row(
                &format!("SELECT {COLUMNS} FROM plugins WHERE plugin_id=?1"),
                [id],
                plugin_row,
            )
            .optional()
            .map_err(Into::into)
    }
    pub fn get_plugin_version(
        &self,
        id: &str,
        hash: &str,
    ) -> Result<Option<PluginVersionMetadata>, StoreError> {
        self.app
            .query_row(
                &format!("SELECT {VERSION_COLUMNS} FROM plugin_versions WHERE plugin_id=?1 AND package_hash=?2"),
                params![id, hash],
                version_row,
            )
            .optional()
            .map_err(Into::into)
    }
    pub fn get_plugin_version_by_semver(
        &self,
        id: &str,
        version: &str,
    ) -> Result<Option<PluginVersionMetadata>, StoreError> {
        self.app
            .query_row(
                &format!("SELECT {VERSION_COLUMNS} FROM plugin_versions WHERE plugin_id=?1 AND version=?2"),
                params![id, version],
                version_row,
            )
            .optional()
            .map_err(Into::into)
    }
    pub fn list_plugin_versions(
        &self,
        plugin_id: &str,
        after_hash: Option<&str>,
        limit: Option<usize>,
    ) -> Result<PluginPage<PluginVersionMetadata>, StoreError> {
        let cap = super::paging::api_limit(limit);
        let tx = crate::read_tx(self.app)?;
        let mut stmt = tx.prepare(&format!(
            "SELECT {VERSION_COLUMNS} FROM plugin_versions WHERE plugin_id=?1 AND package_hash>?2 ORDER BY package_hash LIMIT ?3"
        ))?;
        let items = stmt
            .query_map(
                params![plugin_id, after_hash.unwrap_or(""), (cap + 1) as i64],
                version_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        tx.commit()?;
        Ok(super::paging::paginate(items, cap, |v| {
            v.package_hash.clone()
        }))
    }
    pub fn referenced_plugin_package_hashes(
        &self,
    ) -> Result<std::collections::BTreeSet<String>, StoreError> {
        let tx = crate::read_tx(self.app)?;
        let mut stmt = tx.prepare(
            "SELECT active_package_hash FROM plugins
             UNION
             SELECT package_hash FROM plugin_versions
             UNION
             SELECT c.package_hash
             FROM plugin_operations o
             JOIN plugin_commands c ON c.operation_id=o.operation_id
             WHERE o.state NOT IN ('succeeded','rejected','failed','cancelled')
               AND c.package_hash IS NOT NULL",
        )?;
        let hashes = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
        drop(stmt);
        tx.commit()?;
        Ok(hashes)
    }
    pub fn register_plugin_version(
        &self,
        version: &PluginVersionMetadata,
        expected_revision: i64,
    ) -> Result<PluginRecord, StoreError> {
        let tx = crate::write_tx(self.app)?;
        let current = self.get_plugin(&version.plugin_id)?;
        if let Some(p) = &current {
            if let Some(active) = p.active_operation_id {
                return Err(PluginError::OperationActive(active).into());
            }
        } else if let Some(lock_op) =
            super::staged_operations::active_install_lock_op(self, &version.plugin_id)?
        {
            return Err(PluginError::OperationActive(lock_op.operation_id).into());
        }
        let prior_hash: Option<String> = tx
            .query_row(
                "SELECT package_hash FROM plugin_versions WHERE plugin_id=?1 AND version=?2",
                params![version.plugin_id, version.version],
                |r| r.get(0),
            )
            .optional()?;
        if prior_hash
            .as_deref()
            .is_some_and(|hash| hash != version.package_hash)
        {
            return Err(PluginError::PackageConflict.into());
        }
        if let Some(prior) = self.get_plugin_version(&version.plugin_id, &version.package_hash)? {
            if prior != *version {
                return Err(PluginError::PackageConflict.into());
            }
            return Err(PluginError::Conflict.into());
        }
        if current.as_ref().map_or(0, |p| p.revision) != expected_revision {
            return Err(PluginError::Conflict.into());
        }
        super::ui_trust::ensure_unique_manifest_remote_name(
            self,
            &version.plugin_id,
            &version.manifest_json,
        )?;
        if current.is_none() {
            super::quota::purge_incompatible_domain_state(
                &tx,
                &version.plugin_id,
                version.credential_bound,
            )?;
            let init_gen = load_generation_floor(self, &version.plugin_id)?;
            tx.execute(
                "INSERT INTO plugins VALUES (?1,?2,'installed-disabled',0,1,0,?3,NULL,NULL)",
                params![version.plugin_id, version.package_hash, init_gen],
            )?;
        }
        tx.execute(
            "INSERT INTO plugin_versions VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                version.plugin_id,
                version.package_hash,
                version.version,
                version.manifest_json,
                version.config_schema_json,
                version.state_schema,
                version.artifact_path,
                version.signing_key_id,
                version.signature_status,
                version.ui_metadata_ref,
                version.credential_bound
            ],
        )?;
        let record = self
            .get_plugin(&version.plugin_id)?
            .ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(record)
    }
}

impl Store {
    pub fn register_plugin_version(
        &self,
        value: &PluginVersionMetadata,
        revision: i64,
    ) -> Result<PluginRecord, StoreError> {
        self.with_app(|app| app.register_plugin_version(value, revision))
    }
    pub fn get_plugin(&self, id: &str) -> Result<Option<PluginRecord>, StoreError> {
        self.with_app_read(|app| app.get_plugin(id))
    }
    pub fn get_plugin_version(
        &self,
        id: &str,
        hash: &str,
    ) -> Result<Option<PluginVersionMetadata>, StoreError> {
        self.with_app_read(|app| app.get_plugin_version(id, hash))
    }
    pub fn get_plugin_version_by_semver(
        &self,
        id: &str,
        version: &str,
    ) -> Result<Option<PluginVersionMetadata>, StoreError> {
        self.with_app_read(|app| app.get_plugin_version_by_semver(id, version))
    }
    pub fn list_plugin_versions(
        &self,
        plugin_id: &str,
        after_hash: Option<&str>,
        limit: Option<usize>,
    ) -> Result<PluginPage<PluginVersionMetadata>, StoreError> {
        self.with_app_read(|app| app.list_plugin_versions(plugin_id, after_hash, limit))
    }
    pub fn referenced_plugin_package_hashes(
        &self,
    ) -> Result<std::collections::BTreeSet<String>, StoreError> {
        self.with_app_read(|app| app.referenced_plugin_package_hashes())
    }
}
