use super::*;
use crate::{App, StoreError};
use rusqlite::{OptionalExtension, params};

pub(super) fn charge(
    app: &App<'_>,
    binding: &super::authority::Binding,
    kind: PluginStateKind,
    delta: i64,
) -> Result<i64, StoreError> {
    let total: (i64,i64) = app.app.query_row(&format!("SELECT {},revision FROM plugin_quota WHERE plugin_id=?1 AND security_scope='aggregate'",kind.counter()),[&binding.plugin_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?.unwrap_or((0,0));
    if total.0 + delta > kind.total_limit() {
        return Err(PluginError::QuotaExceeded.into());
    }
    if total.1 == i64::MAX {
        return Err(PluginError::Conflict.into());
    }
    let scope = binding.scope.key()?;
    for key in ["aggregate", scope.as_str()] {
        app.app.execute("INSERT INTO plugin_quota VALUES (?1,?2,0,0,0) ON CONFLICT(plugin_id,security_scope) DO NOTHING",params![binding.plugin_id,key])?;
        app.app.execute(
            &format!(
                "UPDATE plugin_quota SET {0}={0}+?3 WHERE plugin_id=?1 AND security_scope=?2",
                kind.counter()
            ),
            params![binding.plugin_id, key, delta],
        )?;
    }
    app.app.execute("UPDATE plugin_quota SET revision=revision+1 WHERE plugin_id=?1 AND security_scope='aggregate'",[&binding.plugin_id])?;
    Ok(total.1 + 1)
}

pub(super) fn quota(
    app: &App<'_>,
    binding: &super::authority::Binding,
) -> Result<PluginQuota, StoreError> {
    let read = |key: &str| -> Result<(i64, i64), StoreError> {
        Ok(app.app.query_row("SELECT kv_bytes,blob_bytes FROM plugin_quota WHERE plugin_id=?1 AND security_scope=?2",params![binding.plugin_id,key],|r|Ok((r.get(0)?,r.get(1)?))).optional()?.unwrap_or((0,0)))
    };
    let (scope_kv_bytes, scope_blob_bytes) = read(&binding.scope.key()?)?;
    let (plugin_kv_bytes, plugin_blob_bytes) = read("aggregate")?;
    Ok(PluginQuota {
        scope_kv_bytes,
        scope_blob_bytes,
        plugin_kv_bytes,
        plugin_blob_bytes,
    })
}

pub(crate) fn purge_retired_site_scopes(
    tx: &rusqlite::Connection,
    site_id: domain::SiteId,
    retired_epoch: Option<&str>,
) -> Result<(), StoreError> {
    let (clause, param) = match retired_epoch {
        Some(epoch) => (
            "security_scope = ?1",
            super::authority::PluginSecurityScope::site_scope_key(site_id, epoch)?,
        ),
        None => (
            "security_scope LIKE ?1",
            format!("{{\"Site\":{{\"id\":\"{site_id}\",\"epoch\":\"%"),
        ),
    };
    let affected_plugins: Vec<String> = {
        let mut stmt = tx.prepare(&format!(
            "SELECT DISTINCT plugin_id FROM plugin_quota WHERE security_scope != 'aggregate' AND {clause}"
        ))?;
        stmt.query_map([&param], |r| r.get(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    for table in [
        "plugin_kv",
        "plugin_blobs",
        "plugin_config_secrets",
        "plugin_command_payloads",
        "plugin_operation_results",
    ] {
        tx.execute(&format!("DELETE FROM {table} WHERE {clause}"), [&param])?;
    }
    tx.execute(
        &format!("DELETE FROM plugin_quota WHERE security_scope != 'aggregate' AND {clause}"),
        [&param],
    )?;
    for plugin_id in affected_plugins {
        let (kv_sum, blob_sum): (i64, i64) = tx.query_row(
            "SELECT COALESCE(SUM(kv_bytes),0), COALESCE(SUM(blob_bytes),0) FROM plugin_quota WHERE plugin_id=?1 AND security_scope!='aggregate'",
            [&plugin_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        tx.execute(
            "UPDATE plugin_quota SET kv_bytes=?2, blob_bytes=?3, revision=CASE WHEN revision<9223372036854775807 THEN revision+1 ELSE revision END WHERE plugin_id=?1 AND security_scope='aggregate'",
            params![plugin_id, kv_sum, blob_sum],
        )?;
    }
    Ok(())
}

pub(super) fn purge_incompatible_domain_state(
    tx: &rusqlite::Connection,
    plugin_id: &str,
    credential_bound: bool,
) -> Result<(), StoreError> {
    let global_key = super::authority::PluginSecurityScope::global().key()?;
    let mut deleted_any_state = 0usize;
    for table in ["plugin_kv", "plugin_blobs", "plugin_config_secrets"] {
        let sql = if credential_bound {
            format!("DELETE FROM {table} WHERE plugin_id=?1 AND security_scope=?2")
        } else {
            format!("DELETE FROM {table} WHERE plugin_id=?1 AND security_scope!=?2")
        };
        let n = tx.execute(&sql, params![plugin_id, global_key])?;
        if table != "plugin_config_secrets" {
            deleted_any_state += n;
        }
    }
    let deleted_quota = if credential_bound {
        tx.execute(
            "DELETE FROM plugin_quota WHERE plugin_id=?1 AND security_scope=?2",
            params![plugin_id, global_key],
        )?
    } else {
        tx.execute(
            "DELETE FROM plugin_quota WHERE plugin_id=?1 AND security_scope!=?2 AND security_scope!='aggregate'",
            params![plugin_id, global_key],
        )?
    };
    if deleted_any_state > 0 || deleted_quota > 0 {
        let (kv_sum, blob_sum): (i64, i64) = tx.query_row(
            "SELECT COALESCE(SUM(kv_bytes),0), COALESCE(SUM(blob_bytes),0) FROM plugin_quota WHERE plugin_id=?1 AND security_scope!='aggregate'",
            [plugin_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        tx.execute(
            "UPDATE plugin_quota SET kv_bytes=?2, blob_bytes=?3, revision=CASE WHEN revision<9223372036854775807 THEN revision+1 ELSE revision END WHERE plugin_id=?1 AND security_scope='aggregate'",
            params![plugin_id, kv_sum, blob_sum],
        )?;
    }
    Ok(())
}
