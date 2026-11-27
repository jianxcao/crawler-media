use super::*;
use crate::{App, Store, StoreError};
use rusqlite::params;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginGrantStatus {
    Granted,
    Revoked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginGrantRecord {
    pub grant: PluginGrant,
    pub status: PluginGrantStatus,
}

pub(super) fn validate_grants(grants: &[PluginGrant]) -> Result<(), StoreError> {
    let mut keys = std::collections::BTreeSet::new();
    for grant in grants {
        let Some(canon_scope) = super::authority::canonical_grant_scope_key(&grant.scope) else {
            return Err(PluginError::Invalid("grants require valid exact scopes").into());
        };
        if grant.capability.is_empty()
            || grant.capability.chars().any(char::is_whitespace)
            || grant.capability.contains('*')
            || !keys.insert((grant.capability.clone(), canon_scope))
        {
            return Err(PluginError::Invalid("grants require unique exact scopes").into());
        }
    }
    Ok(())
}

fn revoke_equivalent_grant_rows(
    tx: &rusqlite::Connection,
    id: &str,
    hash: &str,
    capability: &str,
    scope: &PluginGrantScope,
) -> Result<(), StoreError> {
    let mut stmt = tx.prepare(
        "SELECT object_scope FROM plugin_grants WHERE plugin_id=?1 AND package_hash=?2 AND capability=?3 AND status='granted'",
    )?;
    let stored = stmt
        .query_map(params![id, hash, capability], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    for raw in stored {
        if let Ok(parsed) = serde_json::from_str::<PluginGrantScope>(&raw)
            && super::authority::grant_scopes_equivalent(&parsed, scope)
        {
            tx.execute(
                "UPDATE plugin_grants SET status='revoked' WHERE plugin_id=?1 AND package_hash=?2 AND capability=?3 AND object_scope=?4",
                params![id, hash, capability, raw],
            )?;
        }
    }
    Ok(())
}

impl App<'_> {
    pub fn get_plugin_grants(&self, id: &str) -> Result<Vec<PluginGrant>, StoreError> {
        let mut stmt = self.app.prepare("SELECT capability,object_scope,sensitive FROM plugin_grants WHERE plugin_id=?1 AND package_hash=(SELECT active_package_hash FROM plugins WHERE plugin_id=?1) AND status='granted' ORDER BY capability,object_scope")?;
        let raw = stmt
            .query_map([id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, bool>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        raw.into_iter()
            .map(|(capability, scope, sensitive)| {
                Ok(PluginGrant {
                    capability,
                    scope: serde_json::from_str(&scope)?,
                    sensitive,
                })
            })
            .collect()
    }
    pub fn list_plugin_grants(
        &self,
        id: &str,
        package_hash: &str,
    ) -> Result<Vec<PluginGrantRecord>, StoreError> {
        let tx = crate::read_tx(self.app)?;
        let mut stmt = tx.prepare("SELECT capability,object_scope,sensitive,status FROM plugin_grants WHERE plugin_id=?1 AND package_hash=?2 ORDER BY capability,object_scope")?;
        let raw = stmt
            .query_map(params![id, package_hash], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, bool>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        tx.commit()?;
        raw.into_iter()
            .map(|(capability, scope, sensitive, status_raw)| {
                let status = match status_raw.as_str() {
                    "granted" => PluginGrantStatus::Granted,
                    "revoked" => PluginGrantStatus::Revoked,
                    _ => return Err(PluginError::Invalid("invalid stored grant status").into()),
                };
                Ok(PluginGrantRecord {
                    grant: PluginGrant {
                        capability,
                        scope: serde_json::from_str(&scope)?,
                        sensitive,
                    },
                    status,
                })
            })
            .collect()
    }
    pub fn replace_plugin_grants(
        &self,
        id: &str,
        hash: &str,
        revision: i64,
        grants: &[PluginGrant],
    ) -> Result<PluginRecord, StoreError> {
        validate_grants(grants)?;
        let tx = crate::write_tx(self.app)?;
        let p = super::config::current(self, id, revision)?;
        super::config::drained(&p)?;
        if p.package_hash != hash {
            return Err(PluginError::StaleAuthority.into());
        }
        tx.execute(
            "UPDATE plugin_grants SET status='revoked' WHERE plugin_id=?1 AND package_hash=?2",
            params![id, hash],
        )?;
        for grant in grants {
            tx.execute("INSERT INTO plugin_grants VALUES (?1,?2,?3,?4,'granted',?5) ON CONFLICT(plugin_id,package_hash,capability,object_scope) DO UPDATE SET status='granted',sensitive=excluded.sensitive",params![id,hash,grant.capability,serde_json::to_string(&grant.scope)?,grant.sensitive])?;
        }
        tx.execute(
            "UPDATE plugins SET revision=revision+1,generation=generation+1 WHERE plugin_id=?1",
            [id],
        )?;
        let updated = self.get_plugin(id)?.ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }
    pub fn grant_plugin_permissions(
        &self,
        id: &str,
        hash: &str,
        revision: i64,
        grants: &[PluginGrant],
    ) -> Result<PluginRecord, StoreError> {
        validate_grants(grants)?;
        let tx = crate::write_tx(self.app)?;
        let p = super::config::current(self, id, revision)?;
        super::config::drained(&p)?;
        if p.package_hash != hash {
            return Err(PluginError::StaleAuthority.into());
        }
        for grant in grants {
            revoke_equivalent_grant_rows(&tx, id, hash, &grant.capability, &grant.scope)?;
            tx.execute(
                "INSERT INTO plugin_grants VALUES (?1,?2,?3,?4,'granted',?5) ON CONFLICT(plugin_id,package_hash,capability,object_scope) DO UPDATE SET status='granted',sensitive=excluded.sensitive",
                params![
                    id,
                    hash,
                    grant.capability,
                    serde_json::to_string(&grant.scope)?,
                    grant.sensitive
                ],
            )?;
        }
        tx.execute(
            "UPDATE plugins SET revision=revision+1,generation=generation+1 WHERE plugin_id=?1",
            [id],
        )?;
        let updated = self.get_plugin(id)?.ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }
    pub fn revoke_plugin_permissions(
        &self,
        id: &str,
        hash: &str,
        revision: i64,
        grants: &[PluginGrant],
        operation: Option<uuid::Uuid>,
    ) -> Result<PluginRecord, StoreError> {
        validate_grants(grants)?;
        let tx = crate::write_tx(self.app)?;
        let p = super::config::current(self, id, revision)?;
        if p.package_hash != hash {
            return Err(PluginError::StaleAuthority.into());
        }
        if let Some(active) = p.active_operation_id
            && operation != Some(active)
        {
            return Err(PluginError::OperationActive(active).into());
        }
        match p.lifecycle {
            PluginLifecycle::InstalledDisabled | PluginLifecycle::Disabled
                if !p.desired_enabled =>
            {
                for grant in grants {
                    revoke_equivalent_grant_rows(&tx, id, hash, &grant.capability, &grant.scope)?;
                }
                tx.execute(
                    "UPDATE plugins SET revision=revision+1,generation=generation+1 WHERE plugin_id=?1",
                    [id],
                )?;
            }
            PluginLifecycle::Enabled if p.desired_enabled => {
                let op_id = operation.ok_or(PluginError::Invalid(
                    "revoking grants on enabled plugin requires operation",
                ))?;
                super::operations::ensure_lifecycle_op(self, id, op_id)?;
                for grant in grants {
                    revoke_equivalent_grant_rows(&tx, id, hash, &grant.capability, &grant.scope)?;
                }
                tx.execute(
                    "UPDATE plugins SET lifecycle='draining',desired_enabled=0,revision=revision+1,generation=generation+1,active_operation_id=?2,failure_reason=NULL WHERE plugin_id=?1",
                    params![id, op_id.to_string()],
                )?;
                super::operations::cancel_pending(self, id)?;
                super::tasks::disable(self, id, p.generation + 1)?;
            }
            _ => return Err(PluginError::NotDrained.into()),
        }
        let updated = self.get_plugin(id)?.ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }
}
impl Store {
    pub fn replace_plugin_grants(
        &self,
        id: &str,
        hash: &str,
        revision: i64,
        grants: &[PluginGrant],
    ) -> Result<PluginRecord, StoreError> {
        self.with_app(|app| app.replace_plugin_grants(id, hash, revision, grants))
    }
    pub fn get_plugin_grants(&self, id: &str) -> Result<Vec<PluginGrant>, StoreError> {
        self.with_app_read(|app| app.get_plugin_grants(id))
    }
    pub fn list_plugin_grants(
        &self,
        id: &str,
        package_hash: &str,
    ) -> Result<Vec<PluginGrantRecord>, StoreError> {
        self.with_app_read(|app| app.list_plugin_grants(id, package_hash))
    }
    pub fn grant_plugin_permissions(
        &self,
        id: &str,
        hash: &str,
        revision: i64,
        grants: &[PluginGrant],
    ) -> Result<PluginRecord, StoreError> {
        self.with_app(|app| app.grant_plugin_permissions(id, hash, revision, grants))
    }
    pub fn revoke_plugin_permissions(
        &self,
        id: &str,
        hash: &str,
        revision: i64,
        grants: &[PluginGrant],
        operation: Option<uuid::Uuid>,
    ) -> Result<PluginRecord, StoreError> {
        self.with_app(|app| app.revoke_plugin_permissions(id, hash, revision, grants, operation))
    }
}
