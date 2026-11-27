use super::*;
use crate::{App, Store, StoreError};
use rusqlite::{OptionalExtension, params};

pub(super) fn valid_key(key: &str) -> Result<(), StoreError> {
    if key.is_empty() || key.len() > 256 {
        return Err(PluginError::Invalid("key must be 1..256 UTF-8 bytes").into());
    }
    Ok(())
}

impl App<'_> {
    pub fn plugin_state_get(
        &self,
        authority: &PluginStateAuthority,
        kind: PluginStateKind,
        key: &str,
    ) -> Result<Option<PluginStateEntry>, StoreError> {
        valid_key(key)?;
        let tx = crate::read_tx(self.app)?;
        super::authority::validate_runtime(self, &authority.0, kind.capability())?;
        let raw:Option<(i64,Vec<u8>)> = tx.query_row(&format!("SELECT revision,value FROM {} WHERE plugin_id=?1 AND security_scope=?2 AND key=?3",kind.table()),params![authority.0.plugin_id,authority.0.scope.key()?,key],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let value = raw.map(|(revision, bytes)| PluginStateEntry {
            key: key.into(),
            revision,
            value: PluginScopedBytes {
                scope: authority.0.scope.clone(),
                bytes,
            },
        });
        tx.commit()?;
        Ok(value)
    }
    pub fn plugin_state_list(
        &self,
        authority: &PluginStateAuthority,
        kind: PluginStateKind,
        after: Option<&str>,
        count: Option<usize>,
    ) -> Result<Vec<PluginStateKey>, StoreError> {
        let tx = crate::read_tx(self.app)?;
        super::authority::validate_runtime(self, &authority.0, kind.capability())?;
        let mut stmt = tx.prepare(&format!("SELECT key,revision,length(value) FROM {} WHERE plugin_id=?1 AND security_scope=?2 AND key>?3 ORDER BY key LIMIT ?4",kind.table()))?;
        let result = stmt
            .query_map(
                params![
                    authority.0.plugin_id,
                    authority.0.scope.key()?,
                    after.unwrap_or(""),
                    super::paging::limit(count) as i64
                ],
                |r| {
                    Ok(PluginStateKey {
                        key: r.get(0)?,
                        revision: r.get(1)?,
                        bytes: r.get(2)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        tx.commit()?;
        Ok(result)
    }
    pub fn plugin_state_put(
        &self,
        authority: &PluginStateAuthority,
        kind: PluginStateKind,
        key: &str,
        bytes: &[u8],
        expected_revision: i64,
    ) -> Result<PluginStateEntry, StoreError> {
        valid_key(key)?;
        if bytes.len() > kind.object_limit() {
            return Err(PluginError::Invalid("value exceeds object limit").into());
        }
        let tx = crate::write_tx(self.app)?;
        super::authority::validate_runtime(self, &authority.0, kind.capability())?;
        let prior:Option<(i64,i64)> = tx.query_row(&format!("SELECT revision,length(CAST(key AS BLOB))+length(value) FROM {} WHERE plugin_id=?1 AND security_scope=?2 AND key=?3",kind.table()),params![authority.0.plugin_id,authority.0.scope.key()?,key],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if prior.map_or(0, |v| v.0) != expected_revision {
            return Err(PluginError::Conflict.into());
        }
        let revision = super::quota::charge(
            self,
            &authority.0,
            kind,
            (key.len() + bytes.len()) as i64 - prior.map_or(0, |v| v.1),
        )?;
        tx.execute(&format!("INSERT INTO {} VALUES (?1,?2,?3,?4,?5) ON CONFLICT(plugin_id,security_scope,key) DO UPDATE SET revision=excluded.revision,value=excluded.value",kind.table()),params![authority.0.plugin_id,authority.0.scope.key()?,key,revision,bytes])?;
        tx.commit()?;
        Ok(PluginStateEntry {
            key: key.into(),
            revision,
            value: PluginScopedBytes {
                scope: authority.0.scope.clone(),
                bytes: bytes.into(),
            },
        })
    }
    pub fn plugin_state_delete(
        &self,
        authority: &PluginStateAuthority,
        kind: PluginStateKind,
        key: &str,
        expected_revision: i64,
    ) -> Result<(), StoreError> {
        valid_key(key)?;
        let tx = crate::write_tx(self.app)?;
        super::authority::validate_runtime(self, &authority.0, kind.capability())?;
        let prior:Option<(i64,i64)> = tx.query_row(&format!("SELECT revision,length(CAST(key AS BLOB))+length(value) FROM {} WHERE plugin_id=?1 AND security_scope=?2 AND key=?3",kind.table()),params![authority.0.plugin_id,authority.0.scope.key()?,key],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let prior = prior.ok_or(PluginError::NotFound)?;
        if prior.0 != expected_revision {
            return Err(PluginError::Conflict.into());
        }
        super::quota::charge(self, &authority.0, kind, -prior.1)?;
        tx.execute(
            &format!(
                "DELETE FROM {} WHERE plugin_id=?1 AND security_scope=?2 AND key=?3",
                kind.table()
            ),
            params![authority.0.plugin_id, authority.0.scope.key()?, key],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn plugin_state_quota(
        &self,
        authority: &PluginStateAuthority,
        kind: PluginStateKind,
    ) -> Result<PluginQuota, StoreError> {
        let tx = crate::read_tx(self.app)?;
        super::authority::validate_runtime(self, &authority.0, kind.capability())?;
        let result = super::quota::quota(self, &authority.0)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn plugin_state_list_entries(
        &self,
        authority: &PluginStateAuthority,
        kind: PluginStateKind,
        prefix: &str,
        after: Option<&str>,
        count: Option<usize>,
    ) -> Result<Vec<PluginStateEntry>, StoreError> {
        if prefix.len() > 256 || after.is_some_and(|c| c.len() > 256) {
            return Err(
                PluginError::Invalid("prefix and cursor must be <= 256 UTF-8 bytes").into(),
            );
        }
        let limit = super::paging::limit(count);
        let limit = if kind == PluginStateKind::Blob {
            limit.min(32)
        } else {
            limit
        };
        let tx = crate::read_tx(self.app)?;
        super::authority::validate_runtime(self, &authority.0, kind.capability())?;
        let mut stmt = tx.prepare(&format!(
            "SELECT key,revision,value FROM {} WHERE plugin_id=?1 AND security_scope=?2 AND key>=?3 AND substr(key,1,length(?3))=?3 AND (?4 IS NULL OR key>?4) ORDER BY key LIMIT ?5",
            kind.table()
        ))?;
        let scope = authority.0.scope.clone();
        let result = stmt
            .query_map(
                params![
                    authority.0.plugin_id,
                    scope.key()?,
                    prefix,
                    after,
                    limit as i64
                ],
                |r| {
                    Ok(PluginStateEntry {
                        key: r.get(0)?,
                        revision: r.get(1)?,
                        value: PluginScopedBytes {
                            scope: scope.clone(),
                            bytes: r.get(2)?,
                        },
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        tx.commit()?;
        Ok(result)
    }
}
impl Store {
    pub fn plugin_state_get(
        &self,
        a: &PluginStateAuthority,
        k: PluginStateKind,
        key: &str,
    ) -> Result<Option<PluginStateEntry>, StoreError> {
        self.with_app_read(|app| app.plugin_state_get(a, k, key))
    }
    pub fn plugin_state_list(
        &self,
        a: &PluginStateAuthority,
        k: PluginStateKind,
        after: Option<&str>,
        count: Option<usize>,
    ) -> Result<Vec<PluginStateKey>, StoreError> {
        self.with_app_read(|app| app.plugin_state_list(a, k, after, count))
    }
    pub fn plugin_state_list_entries(
        &self,
        a: &PluginStateAuthority,
        k: PluginStateKind,
        prefix: &str,
        after: Option<&str>,
        count: Option<usize>,
    ) -> Result<Vec<PluginStateEntry>, StoreError> {
        self.with_app_read(|app| app.plugin_state_list_entries(a, k, prefix, after, count))
    }
    pub fn plugin_state_put(
        &self,
        a: &PluginStateAuthority,
        k: PluginStateKind,
        key: &str,
        value: &[u8],
        revision: i64,
    ) -> Result<PluginStateEntry, StoreError> {
        self.with_app(|app| app.plugin_state_put(a, k, key, value, revision))
    }
    pub fn plugin_state_delete(
        &self,
        a: &PluginStateAuthority,
        k: PluginStateKind,
        key: &str,
        revision: i64,
    ) -> Result<(), StoreError> {
        self.with_app(|app| app.plugin_state_delete(a, k, key, revision))
    }
    pub fn plugin_state_quota(
        &self,
        a: &PluginStateAuthority,
        k: PluginStateKind,
    ) -> Result<PluginQuota, StoreError> {
        self.with_app_read(|app| app.plugin_state_quota(a, k))
    }
}
