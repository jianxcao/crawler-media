use super::*;
use crate::{App, Store, StoreError};

pub(super) fn limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(100).clamp(1, 200)
}

pub(crate) fn api_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(50).clamp(1, 200)
}

pub(crate) fn paginate<T>(
    mut items: Vec<T>,
    limit: usize,
    cursor_of: impl FnOnce(&T) -> String,
) -> PluginPage<T> {
    let has_more = items.len() > limit;
    if has_more {
        items.truncate(limit);
    }
    let next_cursor = if has_more {
        items.last().map(cursor_of)
    } else {
        None
    };
    PluginPage { items, next_cursor }
}

impl App<'_> {
    pub fn list_plugins(
        &self,
        after: Option<&str>,
        count: Option<usize>,
    ) -> Result<Vec<PluginRecord>, StoreError> {
        let tx = crate::read_tx(self.app)?;
        let mut stmt = self.app.prepare(
            "SELECT plugin_id FROM plugins WHERE plugin_id>?1 ORDER BY plugin_id LIMIT ?2",
        )?;
        let ids = stmt
            .query_map(
                rusqlite::params![after.unwrap_or(""), limit(count) as i64],
                |r| r.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        let records = ids
            .into_iter()
            .map(|id| {
                self.get_plugin(&id)?
                    .ok_or_else(|| PluginError::NotFound.into())
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        tx.commit()?;
        Ok(records)
    }
    pub fn plugin_authority_snapshot(
        &self,
        id: &str,
    ) -> Result<PluginAuthoritySnapshot, StoreError> {
        let tx = crate::read_tx(self.app)?;
        let plugin = self.get_plugin(id)?.ok_or(PluginError::NotFound)?;
        let version = self
            .get_plugin_version(id, &plugin.package_hash)?
            .ok_or(PluginError::NotFound)?;
        let config = self.get_plugin_config(id)?;
        let grants = self.get_plugin_grants(id)?;
        tx.commit()?;
        Ok(PluginAuthoritySnapshot {
            plugin,
            version,
            config,
            grants,
        })
    }
}
impl Store {
    pub fn list_plugins(
        &self,
        after: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<PluginRecord>, StoreError> {
        self.with_app_read(|app| app.list_plugins(after, limit))
    }
    pub fn plugin_authority_snapshot(
        &self,
        id: &str,
    ) -> Result<PluginAuthoritySnapshot, StoreError> {
        self.with_app_read(|app| app.plugin_authority_snapshot(id))
    }
}
