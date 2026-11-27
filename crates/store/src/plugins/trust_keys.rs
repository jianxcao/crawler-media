use super::*;
use crate::{App, Store, StoreError};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginTrustKeyStatus {
    Trusted,
    Revoked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginTrustKey {
    pub key_id: String,
    pub label: String,
    pub public_key: String,
    pub fingerprint: String,
    pub status: PluginTrustKeyStatus,
    pub revision: i64,
    pub created_at: i64,
    pub created_by: String,
    pub revoked_at: Option<i64>,
    pub revoked_by: Option<String>,
    pub revoked_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginTrustKeySummary {
    pub key: PluginTrustKey,
    pub associated_package_count: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinPluginTrustKey {
    pub key_id: String,
    pub label: String,
    pub public_key: String,
    pub fingerprint: String,
    pub actor: String,
}

const COLUMNS: &str = "key_id,label,public_key,fingerprint,status,revision,created_at,created_by,revoked_at,revoked_by,revoked_reason";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<PluginTrustKey> {
    let raw: String = r.get(4)?;
    let status = match raw.as_str() {
        "trusted" => PluginTrustKeyStatus::Trusted,
        "revoked" => PluginTrustKeyStatus::Revoked,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    Ok(PluginTrustKey {
        key_id: r.get(0)?,
        label: r.get(1)?,
        public_key: r.get(2)?,
        fingerprint: r.get(3)?,
        status,
        revision: r.get(5)?,
        created_at: r.get(6)?,
        created_by: r.get(7)?,
        revoked_at: r.get(8)?,
        revoked_by: r.get(9)?,
        revoked_reason: r.get(10)?,
    })
}

fn valid_text(value: &str, max: usize, msg: &'static str) -> Result<(), StoreError> {
    if value.trim().is_empty() || value.len() > max {
        return Err(PluginError::Invalid(msg).into());
    }
    Ok(())
}

pub(super) fn is_key_active(app: &App<'_>, key_id: &str) -> Result<bool, StoreError> {
    Ok(app.app.query_row(
        "SELECT EXISTS(SELECT 1 FROM plugin_trust_keys WHERE key_id=?1 AND status='trusted')",
        [key_id],
        |r| r.get(0),
    )?)
}

impl App<'_> {
    pub fn pin_plugin_trust_key(
        &self,
        input: &PinPluginTrustKey,
    ) -> Result<PluginTrustKey, StoreError> {
        valid_text(&input.key_id, 128, "invalid trust key_id")?;
        valid_text(&input.label, 256, "invalid trust key label")?;
        valid_text(&input.public_key, 4096, "invalid trust public_key")?;
        valid_text(&input.fingerprint, 256, "invalid trust fingerprint")?;
        valid_text(&input.actor, 256, "invalid trust key actor")?;

        let tx = crate::write_tx(self.app)?;
        let duplicate: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM plugin_trust_keys WHERE key_id=?1 OR fingerprint=?2)",
            params![input.key_id, input.fingerprint],
            |r| r.get(0),
        )?;
        if duplicate {
            return Err(PluginError::Conflict.into());
        }
        let now = crate::unix_now();
        tx.execute(
            "INSERT INTO plugin_trust_keys VALUES (?1,?2,?3,?4,'trusted',1,?5,?6,NULL,NULL,NULL)",
            params![
                input.key_id,
                input.label,
                input.public_key,
                input.fingerprint,
                now,
                input.actor,
            ],
        )?;
        let created = self
            .get_plugin_trust_key(&input.key_id)?
            .ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(created)
    }

    pub fn get_plugin_trust_key(&self, key_id: &str) -> Result<Option<PluginTrustKey>, StoreError> {
        self.app
            .query_row(
                &format!("SELECT {COLUMNS} FROM plugin_trust_keys WHERE key_id=?1"),
                [key_id],
                row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn count_plugin_versions_for_key(&self, key_id: &str) -> Result<i64, StoreError> {
        self.app
            .query_row(
                "SELECT COUNT(*) FROM plugin_versions WHERE signing_key_id=?1",
                [key_id],
                |r| r.get(0),
            )
            .map_err(Into::into)
    }

    pub fn list_plugin_trust_keys(
        &self,
        after_key_id: Option<&str>,
        limit: Option<usize>,
    ) -> Result<PluginPage<PluginTrustKeySummary>, StoreError> {
        let cap = super::paging::api_limit(limit);
        let tx = crate::read_tx(self.app)?;
        let mut stmt = tx.prepare(&format!(
            "SELECT {COLUMNS},(SELECT COUNT(*) FROM plugin_versions v WHERE v.signing_key_id=plugin_trust_keys.key_id) FROM plugin_trust_keys WHERE key_id>?1 ORDER BY key_id LIMIT ?2"
        ))?;
        let items = stmt
            .query_map(params![after_key_id.unwrap_or(""), (cap + 1) as i64], |r| {
                Ok(PluginTrustKeySummary {
                    key: row(r)?,
                    associated_package_count: r.get(11)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        tx.commit()?;
        Ok(super::paging::paginate(items, cap, |item| {
            item.key.key_id.clone()
        }))
    }

    pub fn list_active_plugin_trust_keys(
        &self,
        after_key_id: Option<&str>,
        limit: Option<usize>,
    ) -> Result<PluginPage<PluginTrustKey>, StoreError> {
        let cap = super::paging::api_limit(limit);
        let tx = crate::read_tx(self.app)?;
        let mut stmt = tx.prepare(&format!(
            "SELECT {COLUMNS} FROM plugin_trust_keys WHERE status='trusted' AND key_id>?1 ORDER BY key_id LIMIT ?2"
        ))?;
        let items = stmt
            .query_map(params![after_key_id.unwrap_or(""), (cap + 1) as i64], row)?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        tx.commit()?;
        Ok(super::paging::paginate(items, cap, |k| k.key_id.clone()))
    }

    pub fn revoke_plugin_trust_key(
        &self,
        key_id: &str,
        expected_revision: Option<i64>,
        reason: &str,
        actor: &str,
    ) -> Result<PluginTrustKey, StoreError> {
        valid_text(actor, 256, "invalid revoke actor")?;
        if reason.len() > 1024 {
            return Err(PluginError::Invalid("revoked reason exceeds limit").into());
        }
        let tx = crate::write_tx(self.app)?;
        let current = self
            .get_plugin_trust_key(key_id)?
            .ok_or(PluginError::NotFound)?;
        if expected_revision.is_some_and(|rev| rev != current.revision) {
            return Err(PluginError::Conflict.into());
        }
        if current.status == PluginTrustKeyStatus::Revoked {
            tx.commit()?;
            return Ok(current);
        }
        if current.revision == i64::MAX {
            return Err(PluginError::Conflict.into());
        }
        let now = crate::unix_now();
        tx.execute(
            "UPDATE plugin_trust_keys SET status='revoked',revision=revision+1,revoked_at=?2,revoked_by=?3,revoked_reason=?4 WHERE key_id=?1",
            params![key_id, now, actor, reason],
        )?;
        let updated = self
            .get_plugin_trust_key(key_id)?
            .ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }
}

impl Store {
    pub fn pin_plugin_trust_key(
        &self,
        input: &PinPluginTrustKey,
    ) -> Result<PluginTrustKey, StoreError> {
        self.with_app(|app| app.pin_plugin_trust_key(input))
    }

    pub fn get_plugin_trust_key(&self, key_id: &str) -> Result<Option<PluginTrustKey>, StoreError> {
        self.with_app_read(|app| app.get_plugin_trust_key(key_id))
    }

    pub fn count_plugin_versions_for_key(&self, key_id: &str) -> Result<i64, StoreError> {
        self.with_app_read(|app| app.count_plugin_versions_for_key(key_id))
    }

    pub fn list_plugin_trust_keys(
        &self,
        after_key_id: Option<&str>,
        limit: Option<usize>,
    ) -> Result<PluginPage<PluginTrustKeySummary>, StoreError> {
        self.with_app_read(|app| app.list_plugin_trust_keys(after_key_id, limit))
    }

    pub fn list_active_plugin_trust_keys(
        &self,
        after_key_id: Option<&str>,
        limit: Option<usize>,
    ) -> Result<PluginPage<PluginTrustKey>, StoreError> {
        self.with_app_read(|app| app.list_active_plugin_trust_keys(after_key_id, limit))
    }

    pub fn revoke_plugin_trust_key(
        &self,
        key_id: &str,
        expected_revision: Option<i64>,
        reason: &str,
        actor: &str,
    ) -> Result<PluginTrustKey, StoreError> {
        self.with_app(|app| app.revoke_plugin_trust_key(key_id, expected_revision, reason, actor))
    }
}
