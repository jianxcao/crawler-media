use super::*;
use crate::{App, Store, StoreError};
use rusqlite::{OptionalExtension, params};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginStatePolicy {
    Preserve,
    Reset {
        scopes: Option<Vec<PluginSecurityScope>>,
    },
}

#[derive(Clone, Debug)]
pub struct PluginUpgradeConfig {
    pub schema_revision: String,
    pub values: serde_json::Value,
    pub secret_scope_updates: Vec<(PluginSecurityScope, Vec<PluginSecretUpdate>)>,
}

#[derive(Clone, Debug)]
pub struct ApplyPluginPackageSwitch {
    pub plugin_id: String,
    pub expected_revision: i64,
    pub operation_id: Option<uuid::Uuid>,
    pub target_package_hash: String,
    pub state_policy: PluginStatePolicy,
    pub target_config: Option<PluginUpgradeConfig>,
    pub target_grants: Vec<PluginGrant>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginStateResetResult {
    pub plugin: PluginRecord,
    pub deleted_kv_rows: usize,
    pub deleted_blob_rows: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginStateResetPreview {
    pub kv_entries: i64,
    pub blob_entries: i64,
    pub kv_bytes: i64,
    pub blob_bytes: i64,
    pub state_revision: i64,
}

fn ensure_fenced_drained(
    app: &App<'_>,
    id: &str,
    revision: i64,
    operation_id: Option<uuid::Uuid>,
) -> Result<PluginRecord, StoreError> {
    let p = super::config::current(app, id, revision)?;
    if !matches!(
        p.lifecycle,
        PluginLifecycle::InstalledDisabled | PluginLifecycle::Disabled
    ) || p.desired_enabled
    {
        return Err(PluginError::NotDrained.into());
    }
    if let Some(active) = p.active_operation_id {
        if operation_id != Some(active) {
            return Err(if operation_id.is_some() {
                PluginError::OperationActive(active).into()
            } else {
                PluginError::NotDrained.into()
            });
        }
    } else if operation_id.is_some() {
        return Err(PluginError::Conflict.into());
    }
    if super::operations::has_unresolved(app, id)? {
        return Err(PluginError::NotDrained.into());
    }
    Ok(p)
}

fn validate_upgrade_config(cfg: &PluginUpgradeConfig) -> Result<String, StoreError> {
    if cfg.schema_revision.is_empty() || cfg.schema_revision.len() > 128 {
        return Err(PluginError::Invalid("invalid config schema revision").into());
    }
    let mut seen = std::collections::BTreeSet::new();
    for (scope, updates) in &cfg.secret_scope_updates {
        let scope_key = scope.key()?;
        for update in updates {
            super::state::valid_key(&update.key)?;
            if !seen.insert((scope_key.clone(), update.key.as_str())) {
                return Err(PluginError::Invalid("duplicate secret key").into());
            }
            if matches!(&update.change, PluginSecretChange::Replace(b) if b.len() > 65536) {
                return Err(PluginError::Invalid("secret exceeds object limit").into());
            }
        }
    }
    Ok(serde_json::to_string(&cfg.values)?)
}

fn validate_upgrade_secret_scopes(
    app: &App<'_>,
    cfg: &PluginUpgradeConfig,
    credential_bound: bool,
) -> Result<(), StoreError> {
    for (scope, _) in &cfg.secret_scope_updates {
        if scope.site_id().is_some() != credential_bound {
            return Err(PluginError::Denied.into());
        }
        scope.validate(app, false)?;
    }
    Ok(())
}

fn clear_scopes(
    tx: &rusqlite::Connection,
    plugin_id: &str,
    scopes: Option<&[PluginSecurityScope]>,
) -> Result<(usize, usize), StoreError> {
    let agg_rev: Option<i64> = tx
        .query_row(
            "SELECT revision FROM plugin_quota WHERE plugin_id=?1 AND security_scope='aggregate'",
            [plugin_id],
            |r| r.get(0),
        )
        .optional()?;
    if agg_rev == Some(i64::MAX) {
        return Err(PluginError::Conflict.into());
    }

    let (mut deleted_kv, mut deleted_blobs) = (0usize, 0usize);
    if let Some(scope_list) = scopes {
        for scope in scope_list {
            let key = scope.key()?;
            deleted_kv += tx.execute(
                "DELETE FROM plugin_kv WHERE plugin_id=?1 AND security_scope=?2",
                params![plugin_id, key],
            )?;
            deleted_blobs += tx.execute(
                "DELETE FROM plugin_blobs WHERE plugin_id=?1 AND security_scope=?2",
                params![plugin_id, key],
            )?;
            tx.execute(
                "DELETE FROM plugin_quota WHERE plugin_id=?1 AND security_scope=?2 AND security_scope!='aggregate'",
                params![plugin_id, key],
            )?;
        }
        let (kv_sum, blob_sum): (i64, i64) = tx.query_row(
            "SELECT COALESCE(SUM(kv_bytes),0),COALESCE(SUM(blob_bytes),0) FROM plugin_quota WHERE plugin_id=?1 AND security_scope!='aggregate'",
            [plugin_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        tx.execute(
            "UPDATE plugin_quota SET kv_bytes=?2,blob_bytes=?3,revision=revision+1 WHERE plugin_id=?1 AND security_scope='aggregate'",
            params![plugin_id, kv_sum, blob_sum],
        )?;
    } else {
        deleted_kv = tx.execute("DELETE FROM plugin_kv WHERE plugin_id=?1", [plugin_id])?;
        deleted_blobs = tx.execute("DELETE FROM plugin_blobs WHERE plugin_id=?1", [plugin_id])?;
        tx.execute(
            "DELETE FROM plugin_quota WHERE plugin_id=?1 AND security_scope!='aggregate'",
            [plugin_id],
        )?;
        tx.execute(
            "UPDATE plugin_quota SET kv_bytes=0,blob_bytes=0,revision=revision+1 WHERE plugin_id=?1 AND security_scope='aggregate'",
            [plugin_id],
        )?;
    }
    Ok((deleted_kv, deleted_blobs))
}

fn apply_upgrade_config(
    tx: &rusqlite::Connection,
    plugin_id: &str,
    next_config_revision: i64,
    cfg: &PluginUpgradeConfig,
    json: &str,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO plugin_configs VALUES (?1,?2,?3,?4) ON CONFLICT(plugin_id) DO UPDATE SET schema_revision=excluded.schema_revision,config_revision=excluded.config_revision,public_json=excluded.public_json",
        params![plugin_id, cfg.schema_revision, next_config_revision, json],
    )?;
    for (scope, updates) in &cfg.secret_scope_updates {
        let scope_key = scope.key()?;
        for update in updates {
            match &update.change {
                PluginSecretChange::Keep => {}
                PluginSecretChange::Replace(bytes) => {
                    tx.execute(
                        "INSERT INTO plugin_config_secrets VALUES (?1,?2,?3,?4) ON CONFLICT(plugin_id,security_scope,key) DO UPDATE SET value=excluded.value",
                        params![plugin_id, scope_key, update.key, bytes],
                    )?;
                }
                PluginSecretChange::Clear => {
                    tx.execute(
                        "DELETE FROM plugin_config_secrets WHERE plugin_id=?1 AND security_scope=?2 AND key=?3",
                        params![plugin_id, scope_key, update.key],
                    )?;
                }
            }
        }
    }
    Ok(())
}

impl App<'_> {
    pub fn apply_plugin_package_switch(
        &self,
        req: &ApplyPluginPackageSwitch,
    ) -> Result<PluginRecord, StoreError> {
        super::grants::validate_grants(&req.target_grants)?;
        let config_json = req
            .target_config
            .as_ref()
            .map(validate_upgrade_config)
            .transpose()?;
        let tx = crate::write_tx(self.app)?;
        let p = ensure_fenced_drained(
            self,
            &req.plugin_id,
            req.expected_revision,
            req.operation_id,
        )?;
        let current_version = self
            .get_plugin_version(&req.plugin_id, &p.package_hash)?
            .ok_or(PluginError::NotFound)?;
        let target_version = self
            .get_plugin_version(&req.plugin_id, &req.target_package_hash)?
            .ok_or(PluginError::NotFound)?;
        super::lifecycle::check_activation_policy(self, &target_version)?;

        let domain_changed = current_version.credential_bound != target_version.credential_bound;
        match &req.state_policy {
            PluginStatePolicy::Preserve => {
                if current_version.state_schema != target_version.state_schema || domain_changed {
                    return Err(PluginError::StateSchemaMismatch.into());
                }
            }
            PluginStatePolicy::Reset { scopes } => {
                if domain_changed && scopes.is_some() {
                    return Err(PluginError::StateSchemaMismatch.into());
                }
                clear_scopes(&tx, &req.plugin_id, scopes.as_deref())?;
                if domain_changed {
                    super::quota::purge_incompatible_domain_state(
                        &tx,
                        &req.plugin_id,
                        target_version.credential_bound,
                    )?;
                }
            }
        }

        let next_config_rev = if let (Some(cfg), Some(json)) = (&req.target_config, &config_json) {
            validate_upgrade_secret_scopes(self, cfg, target_version.credential_bound)?;
            let next = p.config_revision + 1;
            apply_upgrade_config(&tx, &req.plugin_id, next, cfg, json)?;
            next
        } else {
            p.config_revision
        };

        tx.execute(
            "UPDATE plugin_grants SET status='revoked' WHERE plugin_id=?1 AND package_hash=?2",
            params![req.plugin_id, req.target_package_hash],
        )?;
        for grant in &req.target_grants {
            tx.execute(
                "INSERT INTO plugin_grants VALUES (?1,?2,?3,?4,'granted',?5) ON CONFLICT(plugin_id,package_hash,capability,object_scope) DO UPDATE SET status='granted',sensitive=excluded.sensitive",
                params![
                    req.plugin_id,
                    req.target_package_hash,
                    grant.capability,
                    serde_json::to_string(&grant.scope)?,
                    grant.sensitive
                ],
            )?;
        }
        super::tasks::disable(self, &req.plugin_id, p.generation + 1)?;
        tx.execute(
            "UPDATE plugins SET active_package_hash=?2,revision=revision+1,config_revision=?3,generation=generation+1,failure_reason=NULL WHERE plugin_id=?1",
            params![req.plugin_id, req.target_package_hash, next_config_rev],
        )?;
        let updated = self
            .get_plugin(&req.plugin_id)?
            .ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }

    pub fn register_and_apply_plugin_package_switch(
        &self,
        register_version: Option<(&mut PluginCommandClaim, &PluginVersionMetadata, i64)>,
        req: &ApplyPluginPackageSwitch,
    ) -> Result<PluginRecord, StoreError> {
        let tx = crate::write_tx(self.app)?;
        let updated = match register_version {
            Some((claim, version, now)) => {
                let saved_claim = claim.clone();
                if let Err(err) =
                    self.register_staged_plugin_version(claim, version, &req.target_grants, now)
                {
                    *claim = saved_claim;
                    return Err(err);
                }
                match self.apply_plugin_package_switch(req) {
                    Ok(rec) => rec,
                    Err(err) => {
                        *claim = saved_claim;
                        return Err(err);
                    }
                }
            }
            None => self.apply_plugin_package_switch(req)?,
        };
        tx.commit()?;
        Ok(updated)
    }

    pub fn preview_plugin_state_reset(
        &self,
        plugin_id: &str,
        scopes: Option<&[PluginSecurityScope]>,
    ) -> Result<PluginStateResetPreview, StoreError> {
        let tx = crate::read_tx(self.app)?;
        let _ = self.get_plugin(plugin_id)?.ok_or(PluginError::NotFound)?;
        let state_revision: i64 = tx
            .query_row(
                "SELECT revision FROM plugin_quota WHERE plugin_id=?1 AND security_scope='aggregate'",
                [plugin_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let (mut kv_entries, mut blob_entries, mut kv_bytes, mut blob_bytes) =
            (0i64, 0i64, 0i64, 0i64);
        if let Some(scope_list) = scopes {
            let mut seen = std::collections::BTreeSet::new();
            for scope in scope_list {
                let key = scope.key()?;
                if !seen.insert(key.clone()) {
                    continue;
                }
                let k: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM plugin_kv WHERE plugin_id=?1 AND security_scope=?2",
                    params![plugin_id, key],
                    |r| r.get(0),
                )?;
                let b: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM plugin_blobs WHERE plugin_id=?1 AND security_scope=?2",
                    params![plugin_id, key],
                    |r| r.get(0),
                )?;
                let (qb_kv, qb_blob): (i64, i64) = tx
                    .query_row(
                        "SELECT kv_bytes,blob_bytes FROM plugin_quota WHERE plugin_id=?1 AND security_scope=?2",
                        params![plugin_id, key],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?
                    .unwrap_or((0, 0));
                kv_entries += k;
                blob_entries += b;
                kv_bytes += qb_kv;
                blob_bytes += qb_blob;
            }
        } else {
            kv_entries = tx.query_row(
                "SELECT COUNT(*) FROM plugin_kv WHERE plugin_id=?1",
                [plugin_id],
                |r| r.get(0),
            )?;
            blob_entries = tx.query_row(
                "SELECT COUNT(*) FROM plugin_blobs WHERE plugin_id=?1",
                [plugin_id],
                |r| r.get(0),
            )?;
            (kv_bytes, blob_bytes) = tx
                .query_row(
                    "SELECT kv_bytes,blob_bytes FROM plugin_quota WHERE plugin_id=?1 AND security_scope='aggregate'",
                    [plugin_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
                .unwrap_or((0, 0));
        }
        tx.commit()?;
        Ok(PluginStateResetPreview {
            kv_entries,
            blob_entries,
            kv_bytes,
            blob_bytes,
            state_revision,
        })
    }

    pub fn reset_plugin_state(
        &self,
        plugin_id: &str,
        expected_revision: i64,
        operation_id: Option<uuid::Uuid>,
        scopes: Option<&[PluginSecurityScope]>,
    ) -> Result<PluginStateResetResult, StoreError> {
        let tx = crate::write_tx(self.app)?;
        let _ = ensure_fenced_drained(self, plugin_id, expected_revision, operation_id)?;
        let (deleted_kv_rows, deleted_blob_rows) = clear_scopes(&tx, plugin_id, scopes)?;
        tx.execute(
            "UPDATE plugins SET revision=revision+1 WHERE plugin_id=?1",
            [plugin_id],
        )?;
        let plugin = self.get_plugin(plugin_id)?.ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(PluginStateResetResult {
            plugin,
            deleted_kv_rows,
            deleted_blob_rows,
        })
    }
}

impl Store {
    pub fn apply_plugin_package_switch(
        &self,
        req: &ApplyPluginPackageSwitch,
    ) -> Result<PluginRecord, StoreError> {
        self.with_app(|app| app.apply_plugin_package_switch(req))
    }

    pub fn register_and_apply_plugin_package_switch(
        &self,
        register_version: Option<(&mut PluginCommandClaim, &PluginVersionMetadata, i64)>,
        req: &ApplyPluginPackageSwitch,
    ) -> Result<PluginRecord, StoreError> {
        self.with_app(|app| app.register_and_apply_plugin_package_switch(register_version, req))
    }

    pub fn preview_plugin_state_reset(
        &self,
        plugin_id: &str,
        scopes: Option<&[PluginSecurityScope]>,
    ) -> Result<PluginStateResetPreview, StoreError> {
        self.with_app_read(|app| app.preview_plugin_state_reset(plugin_id, scopes))
    }

    pub fn reset_plugin_state(
        &self,
        plugin_id: &str,
        expected_revision: i64,
        operation_id: Option<uuid::Uuid>,
        scopes: Option<&[PluginSecurityScope]>,
    ) -> Result<PluginStateResetResult, StoreError> {
        self.with_app(|app| {
            app.reset_plugin_state(plugin_id, expected_revision, operation_id, scopes)
        })
    }
}
