use super::*;
use crate::{App, Store, StoreError};
use rusqlite::params;

fn purge_prior_dev_versions(
    tx: &rusqlite::Connection,
    version: &PluginVersionMetadata,
    active_package_hash: &str,
) -> Result<(), StoreError> {
    let mut stmt = tx.prepare(
        "SELECT package_hash, signature_status FROM plugin_versions \
         WHERE plugin_id=?1 AND (package_hash=?2 OR version=?3 OR package_hash=?4)",
    )?;
    let mut dev_hashes = Vec::new();
    let rows = stmt.query_map(
        params![
            version.plugin_id,
            active_package_hash,
            version.version,
            version.package_hash
        ],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    )?;
    for row in rows {
        let (hash, status) = row?;
        if status != "development" {
            return Err(PluginError::Conflict.into());
        }
        dev_hashes.push(hash);
    }
    drop(stmt);

    for old_hash in dev_hashes {
        for (table, col) in [
            ("plugin_ui_trust", "package_sha256"),
            ("plugin_grants", "package_hash"),
            ("plugin_versions", "package_hash"),
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE plugin_id=?1 AND {col}=?2"),
                params![version.plugin_id, old_hash],
            )?;
        }
    }
    Ok(())
}

impl App<'_> {
    pub fn replace_dev_plugin_version(
        &self,
        version: &PluginVersionMetadata,
        grants: &[PluginGrant],
    ) -> Result<PluginRecord, StoreError> {
        if version.signature_status != "development" || version.plugin_id.is_empty() {
            return Err(
                PluginError::Invalid("dev reload requires development signature_status").into(),
            );
        }
        if version.signing_key_id.is_some() {
            return Err(PluginError::Invalid(
                "development package cannot reference signing_key_id",
            )
            .into());
        }
        super::grants::validate_grants(grants)?;

        let tx = crate::write_tx(self.app)?;
        let plugin = self
            .get_plugin(&version.plugin_id)?
            .ok_or(PluginError::NotFound)?;
        if plugin.revision == i64::MAX || plugin.generation == i64::MAX {
            return Err(PluginError::Conflict.into());
        }
        if !matches!(
            plugin.lifecycle,
            PluginLifecycle::InstalledDisabled | PluginLifecycle::Disabled
        ) || plugin.desired_enabled
            || plugin.active_operation_id.is_some()
            || super::operations::has_unresolved(self, &version.plugin_id)?
        {
            return Err(PluginError::NotDrained.into());
        }

        let active_ver = self
            .get_plugin_version(&version.plugin_id, &plugin.package_hash)?
            .ok_or(PluginError::NotFound)?;
        if active_ver.signature_status != "development" {
            return Err(PluginError::Conflict.into());
        }
        if active_ver.state_schema != version.state_schema
            || active_ver.credential_bound != version.credential_bound
        {
            return Err(PluginError::StateSchemaMismatch.into());
        }
        super::ui_trust::ensure_unique_manifest_remote_name(
            self,
            &version.plugin_id,
            &version.manifest_json,
        )?;
        purge_prior_dev_versions(&tx, version, &plugin.package_hash)?;

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
        for grant in grants {
            tx.execute(
                "INSERT INTO plugin_grants VALUES (?1,?2,?3,?4,'granted',?5)",
                params![
                    version.plugin_id,
                    version.package_hash,
                    grant.capability,
                    serde_json::to_string(&grant.scope)?,
                    grant.sensitive
                ],
            )?;
        }
        super::tasks::disable(self, &version.plugin_id, plugin.generation + 1)?;
        tx.execute(
            "UPDATE plugins SET active_package_hash=?2, revision=revision+1, generation=generation+1, failure_reason=NULL \
             WHERE plugin_id=?1",
            params![version.plugin_id, version.package_hash],
        )?;
        let updated = self
            .get_plugin(&version.plugin_id)?
            .ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }
}

impl Store {
    pub fn replace_dev_plugin_version(
        &self,
        version: &PluginVersionMetadata,
        grants: &[PluginGrant],
    ) -> Result<PluginRecord, StoreError> {
        self.with_app(|app| app.replace_dev_plugin_version(version, grants))
    }
}
