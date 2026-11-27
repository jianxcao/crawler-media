use rusqlite::{OptionalExtension, params};

use super::*;
use crate::{App, Store, StoreError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginUiTrustRecord {
    pub plugin_id: String,
    pub package_sha256: String,
    pub bundle_sha256: String,
    pub granted_by_user_id: String,
    pub granted_at: String,
    pub revoked_at: Option<String>,
    pub revision: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantPluginUiTrust {
    pub plugin_id: String,
    pub package_sha256: String,
    pub bundle_sha256: String,
    pub granted_by_user_id: String,
    pub granted_at: String,
    pub expected_revision: i64,
}

const COLUMNS: &str =
    "plugin_id,package_sha256,bundle_sha256,granted_by_user_id,granted_at,revoked_at,revision";

fn trust_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PluginUiTrustRecord> {
    Ok(PluginUiTrustRecord {
        plugin_id: row.get(0)?,
        package_sha256: row.get(1)?,
        bundle_sha256: row.get(2)?,
        granted_by_user_id: row.get(3)?,
        granted_at: row.get(4)?,
        revoked_at: row.get(5)?,
        revision: row.get(6)?,
    })
}

pub(crate) fn extract_manifest_remote_name(manifest_json: &str) -> Option<String> {
    let val: serde_json::Value = serde_json::from_str(manifest_json).ok()?;
    val.get("ui")?
        .get("remote_name")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

pub(crate) fn ensure_unique_manifest_remote_name(
    app: &App<'_>,
    plugin_id: &str,
    manifest_json: &str,
) -> Result<(), StoreError> {
    let Some(remote_name) = extract_manifest_remote_name(manifest_json) else {
        return Ok(());
    };
    if app
        .find_conflicting_remote_name_plugin(plugin_id, &remote_name)?
        .is_some()
    {
        return Err(PluginError::Conflict.into());
    }
    Ok(())
}

impl App<'_> {
    pub fn get_ui_trust(
        &self,
        plugin_id: &str,
        package_sha256: &str,
    ) -> Result<Option<PluginUiTrustRecord>, StoreError> {
        self.app
            .query_row(
                &format!(
                    "SELECT {COLUMNS} FROM plugin_ui_trust WHERE plugin_id=?1 AND package_sha256=?2"
                ),
                params![plugin_id, package_sha256],
                trust_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn get_active_ui_trust(
        &self,
        plugin_id: &str,
        package_sha256: &str,
    ) -> Result<Option<PluginUiTrustRecord>, StoreError> {
        self.app
            .query_row(
                &format!(
                    "SELECT {COLUMNS} FROM plugin_ui_trust WHERE plugin_id=?1 AND package_sha256=?2 AND revoked_at IS NULL"
                ),
                params![plugin_id, package_sha256],
                trust_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn grant_ui_trust(
        &self,
        input: &GrantPluginUiTrust,
    ) -> Result<PluginUiTrustRecord, StoreError> {
        if input.plugin_id.trim().is_empty()
            || input.package_sha256.trim().is_empty()
            || input.bundle_sha256.trim().is_empty()
            || input.granted_by_user_id.trim().is_empty()
            || input.granted_at.trim().is_empty()
        {
            return Err(PluginError::Invalid("invalid ui trust payload").into());
        }
        let tx = crate::write_tx(self.app)?;
        let existing = self.get_ui_trust(&input.plugin_id, &input.package_sha256)?;
        match existing {
            None => {
                if input.expected_revision != 0 {
                    return Err(PluginError::Conflict.into());
                }
                tx.execute(
                    "INSERT INTO plugin_ui_trust(plugin_id,package_sha256,bundle_sha256,granted_by_user_id,granted_at,revoked_at,revision) VALUES (?1,?2,?3,?4,?5,NULL,1)",
                    params![
                        input.plugin_id,
                        input.package_sha256,
                        input.bundle_sha256,
                        input.granted_by_user_id,
                        input.granted_at,
                    ],
                )?;
            }
            Some(prev) => {
                if input.expected_revision != prev.revision || prev.revision == i64::MAX {
                    return Err(PluginError::Conflict.into());
                }
                tx.execute(
                    "UPDATE plugin_ui_trust SET bundle_sha256=?3,granted_by_user_id=?4,granted_at=?5,revoked_at=NULL,revision=revision+1 WHERE plugin_id=?1 AND package_sha256=?2",
                    params![
                        input.plugin_id,
                        input.package_sha256,
                        input.bundle_sha256,
                        input.granted_by_user_id,
                        input.granted_at,
                    ],
                )?;
            }
        }
        let saved = self
            .get_ui_trust(&input.plugin_id, &input.package_sha256)?
            .ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(saved)
    }

    pub fn revoke_ui_trust(
        &self,
        plugin_id: &str,
        package_sha256: &str,
        expected_revision: i64,
        revoked_at: &str,
    ) -> Result<PluginUiTrustRecord, StoreError> {
        if revoked_at.trim().is_empty() {
            return Err(PluginError::Invalid("revoked_at must not be empty").into());
        }
        let tx = crate::write_tx(self.app)?;
        let existing = self
            .get_ui_trust(plugin_id, package_sha256)?
            .ok_or(PluginError::NotFound)?;
        if expected_revision != existing.revision {
            return Err(PluginError::Conflict.into());
        }
        if existing.revoked_at.is_none() {
            if existing.revision == i64::MAX {
                return Err(PluginError::Conflict.into());
            }
            tx.execute(
                "UPDATE plugin_ui_trust SET revoked_at=?3,revision=revision+1 WHERE plugin_id=?1 AND package_sha256=?2",
                params![plugin_id, package_sha256, revoked_at],
            )?;
        }
        let updated = self
            .get_ui_trust(plugin_id, package_sha256)?
            .ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }

    pub fn find_conflicting_remote_name_plugin(
        &self,
        plugin_id: &str,
        remote_name: &str,
    ) -> Result<Option<String>, StoreError> {
        let target = remote_name.trim();
        if target.is_empty() {
            return Ok(None);
        }
        let mut stmt = self.app.prepare(
            "SELECT p.plugin_id, v.manifest_json
             FROM plugins p
             JOIN plugin_versions v
               ON v.plugin_id = p.plugin_id AND v.package_hash = p.active_package_hash
             WHERE p.plugin_id != ?1",
        )?;
        let rows = stmt.query_map([plugin_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (other_id, manifest_json) = row?;
            if extract_manifest_remote_name(&manifest_json).as_deref() == Some(target) {
                return Ok(Some(other_id));
            }
        }
        Ok(None)
    }
}

impl Store {
    pub fn get_ui_trust(
        &self,
        plugin_id: &str,
        package_sha256: &str,
    ) -> Result<Option<PluginUiTrustRecord>, StoreError> {
        self.with_app_read(|app| app.get_ui_trust(plugin_id, package_sha256))
    }

    pub fn get_active_ui_trust(
        &self,
        plugin_id: &str,
        package_sha256: &str,
    ) -> Result<Option<PluginUiTrustRecord>, StoreError> {
        self.with_app_read(|app| app.get_active_ui_trust(plugin_id, package_sha256))
    }

    pub fn grant_ui_trust(
        &self,
        input: &GrantPluginUiTrust,
    ) -> Result<PluginUiTrustRecord, StoreError> {
        self.with_app(|app| app.grant_ui_trust(input))
    }

    pub fn revoke_ui_trust(
        &self,
        plugin_id: &str,
        package_sha256: &str,
        expected_revision: i64,
        revoked_at: &str,
    ) -> Result<PluginUiTrustRecord, StoreError> {
        self.with_app(|app| {
            app.revoke_ui_trust(plugin_id, package_sha256, expected_revision, revoked_at)
        })
    }

    pub fn find_conflicting_remote_name_plugin(
        &self,
        plugin_id: &str,
        remote_name: &str,
    ) -> Result<Option<String>, StoreError> {
        self.with_app_read(|app| app.find_conflicting_remote_name_plugin(plugin_id, remote_name))
    }
}
