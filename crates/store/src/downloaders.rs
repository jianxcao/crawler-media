use std::collections::HashMap;

use domain::DownloaderId;
use downloader::PathMap;
use rusqlite::{OptionalExtension, params};

use super::StoreError;

#[derive(Clone, PartialEq, Eq)]
pub struct DownloaderRow {
    pub id: DownloaderId,
    pub name: String,
    pub kind: String,
    pub url: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub api_key: Option<String>,
    pub category: Option<String>,
    pub path_maps: Vec<PathMap>,
    pub is_default: bool,
    /// Disabled downloaders are skipped for automatic delivery.
    pub enabled: bool,
}

/// 手写 `Debug`：`password` 与 `api_key` 是下载器凭证，绝不能进日志。
///
/// 之前这里是 `#[derive(Debug)]`，任何一句 `debug!(?row)` 都会把口令打进日志与
/// 前端"系统日志"。`None`/`Some("<redacted>")` 仍能看出是否配置了凭证，
/// 其余字段保留原样，便于诊断。`DownloaderRow` 没有 serde 派生，这里只影响
/// `Debug`。
impl std::fmt::Debug for DownloaderRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloaderRow")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("url", &self.url)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("category", &self.category)
            .field("path_maps", &self.path_maps)
            .field("is_default", &self.is_default)
            .field("enabled", &self.enabled)
            .finish()
    }
}

fn api_key_setting(id: DownloaderId) -> String {
    format!("downloader.{id}.api_key")
}

fn normalized_api_key(api_key: &Option<String>) -> Option<&str> {
    api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
}

const COLS: &str =
    "id, name, kind, url, username, password, category, path_maps, is_default, enabled";

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DownloaderRow> {
    let path_maps: Option<String> = row.get(7)?;
    Ok(DownloaderRow {
        id: super::maps::parse_id(row.get(0)?, 0)?,
        name: row.get(1)?,
        kind: row.get(2)?,
        url: row.get(3)?,
        username: row.get(4)?,
        password: row.get(5)?,
        api_key: None,
        category: row.get(6)?,
        path_maps: path_maps
            .and_then(|raw| serde_json::from_str::<Vec<serde_json::Value>>(&raw).ok())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| {
                let from = v["from"].as_str()?.to_string();
                let to = v["to"].as_str()?.to_string();
                Some(PathMap::new(from, to))
            })
            .collect(),
        is_default: row.get::<_, i64>(8)? != 0,
        enabled: row.get::<_, Option<i64>>(9)?.unwrap_or(1) != 0,
    })
}

fn maps_json(maps: &[PathMap]) -> String {
    serde_json::json!(
        maps.iter()
            .map(|m| serde_json::json!({
                "from": m.from,
                "to": m.to.display().to_string(),
            }))
            .collect::<Vec<_>>()
    )
    .to_string()
}

#[cfg(test)]
mod debug_tests {
    use super::DownloaderRow;
    use domain::DownloaderId;

    #[test]
    fn debug_never_prints_the_password() {
        let row = DownloaderRow {
            id: DownloaderId::new(),
            name: "qbit".into(),
            kind: "qbittorrent".into(),
            url: "http://127.0.0.1:8080".into(),
            username: Some("admin".into()),
            password: Some("downloader-password-secret".into()),
            api_key: Some("qbt_secret_token".into()),
            category: Some("movies".into()),
            path_maps: Vec::new(),
            is_default: true,
            enabled: true,
        };
        let rendered = format!("{row:?}");
        assert!(
            !rendered.contains("downloader-password-secret"),
            "Debug 不得输出 password: {rendered}"
        );
        assert!(
            !rendered.contains("qbt_secret_token"),
            "Debug 不得输出 api_key: {rendered}"
        );
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(rendered.contains("127.0.0.1:8080"), "{rendered}");
        assert!(rendered.contains("qbit"), "{rendered}");
    }
}

impl crate::writers::App<'_> {
    fn sync_api_key(&self, row: &DownloaderRow) -> Result<(), StoreError> {
        let key = api_key_setting(row.id);
        match normalized_api_key(&row.api_key) {
            Some(value) => self.put_setting(&key, value)?,
            None => {
                self.delete_setting(&key)?;
            }
        }
        Ok(())
    }

    fn hydrate_api_key(&self, mut row: DownloaderRow) -> Result<DownloaderRow, StoreError> {
        row.api_key = self
            .get_setting(&api_key_setting(row.id))?
            .filter(|s| !s.trim().is_empty());
        Ok(row)
    }

    fn load_api_keys(&self) -> Result<HashMap<String, String>, StoreError> {
        let mut stmt = self
            .app
            .prepare("SELECT key, value FROM settings WHERE key LIKE 'downloader.%.api_key'")?;
        let pairs = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut map = HashMap::new();
        for pair in pairs {
            let (key, value) = pair?;
            if value.trim().is_empty() {
                continue;
            }
            if let Some(id) = key
                .strip_prefix("downloader.")
                .and_then(|rest| rest.strip_suffix(".api_key"))
            {
                map.insert(id.to_string(), value);
            }
        }
        Ok(map)
    }

    /// Verify status and error are one result of one exact observed configuration.
    /// An IMMEDIATE transaction prevents another connection changing config between
    /// the comparison and status publication; any second-statement failure rolls back.
    pub fn record_downloader_verification(
        &self,
        expected: &DownloaderRow,
        outcome: Result<(), &str>,
    ) -> Result<bool, StoreError> {
        let tx = crate::write_tx(self.app)?;
        if self.get_downloader(expected.id)?.as_ref() != Some(expected) {
            return Ok(false);
        }
        self.put_setting(
            &format!("downloader.{}.verified", expected.id),
            if outcome.is_ok() { "true" } else { "false" },
        )?;
        let error_key = format!("downloader.{}.last_error", expected.id);
        match outcome {
            Ok(()) => {
                self.delete_setting(&error_key)?;
            }
            Err(error) => self.put_setting(&error_key, error)?,
        }
        tx.commit()?;
        Ok(true)
    }

    pub fn insert_downloader(&self, row: &DownloaderRow) -> Result<(), StoreError> {
        let tx = crate::write_tx(self.app)?;
        if row.is_default {
            self.app
                .execute("UPDATE downloaders SET is_default = 0", [])?;
        }
        self.app.execute(
            "INSERT INTO downloaders (id, name, kind, url, username, password, category, path_maps, is_default, enabled)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                row.id.to_string(),
                row.name,
                row.kind,
                row.url,
                row.username,
                row.password,
                row.category,
                maps_json(&row.path_maps),
                row.is_default as i64,
                row.enabled as i64,
            ],
        )?;
        self.sync_api_key(row)?;
        tx.commit()?;
        Ok(())
    }

    pub fn save_downloader(&self, row: &DownloaderRow) -> Result<(), StoreError> {
        let tx = crate::write_tx(self.app)?;
        self.app.execute(
            "UPDATE downloaders SET name = ?1, kind = ?2, url = ?3, username = ?4,
                    password = ?5, category = ?6, path_maps = ?7, enabled = ?9
             WHERE id = ?8",
            params![
                row.name,
                row.kind,
                row.url,
                row.username,
                row.password,
                row.category,
                maps_json(&row.path_maps),
                row.id.to_string(),
                row.enabled as i64,
            ],
        )?;
        self.sync_api_key(row)?;
        tx.commit()?;
        Ok(())
    }

    /// Flip a downloader's enabled flag. Returns false when the row is absent.
    pub fn set_downloader_enabled(
        &self,
        id: DownloaderId,
        enabled: bool,
    ) -> Result<bool, StoreError> {
        let changed = self.app.execute(
            "UPDATE downloaders SET enabled = ?2 WHERE id = ?1",
            params![id.to_string(), enabled as i64],
        )?;
        Ok(changed > 0)
    }

    pub fn list_downloaders(&self) -> Result<Vec<DownloaderRow>, StoreError> {
        let mut api_keys = self.load_api_keys()?;
        let mut stmt = self
            .app
            .prepare(&format!("SELECT {COLS} FROM downloaders ORDER BY name"))?;
        let rows = stmt.query_map([], map_row)?;
        let mut list = rows.collect::<Result<Vec<_>, _>>()?;
        for row in &mut list {
            row.api_key = api_keys.remove(&row.id.to_string());
        }
        Ok(list)
    }

    pub fn get_downloader(&self, id: DownloaderId) -> Result<Option<DownloaderRow>, StoreError> {
        let row = self
            .app
            .query_row(
                &format!("SELECT {COLS} FROM downloaders WHERE id = ?1"),
                params![id.to_string()],
                map_row,
            )
            .optional()?;
        row.map(|r| self.hydrate_api_key(r)).transpose()
    }

    pub fn delete_downloader(&self, id: DownloaderId) -> Result<bool, StoreError> {
        let tx = crate::write_tx(self.app)?;
        let n = self.app.execute(
            "DELETE FROM downloaders WHERE id = ?1",
            params![id.to_string()],
        )?;
        self.delete_setting(&api_key_setting(id))?;
        tx.commit()?;
        Ok(n > 0)
    }

    pub fn default_downloader(&self) -> Result<Option<DownloaderRow>, StoreError> {
        let row = self
            .app
            .query_row(
                &format!(
                    "SELECT {COLS} FROM downloaders WHERE is_default = 1 AND enabled = 1 LIMIT 1"
                ),
                [],
                map_row,
            )
            .optional()?;
        row.map(|r| self.hydrate_api_key(r)).transpose()
    }

    pub fn set_default_downloader(&self, id: DownloaderId) -> Result<bool, StoreError> {
        let tx = crate::write_tx(self.app)?;
        let exists = self
            .app
            .query_row(
                "SELECT 1 FROM downloaders WHERE id = ?1",
                params![id.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !exists {
            return Ok(false);
        }
        self.app
            .execute("UPDATE downloaders SET is_default = 0", [])?;
        self.app.execute(
            "UPDATE downloaders SET is_default = 1 WHERE id = ?1",
            params![id.to_string()],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Atomically read-modify-write a downloader row, its `api_key` setting,
    /// verification invalidation (when connection/auth fields change), and
    /// optional default promotion inside one `IMMEDIATE` transaction.
    pub fn mutate_downloader<F>(
        &self,
        id: DownloaderId,
        make_default: bool,
        mutate: F,
    ) -> Result<Option<(DownloaderRow, Option<String>, Option<String>)>, StoreError>
    where
        F: FnOnce(&mut DownloaderRow) -> Result<(), StoreError>,
    {
        let tx = crate::write_tx(self.app)?;
        let Some(before) = self.get_downloader(id)? else {
            return Ok(None);
        };
        let mut row = before.clone();
        mutate(&mut row)?;
        if make_default {
            self.app
                .execute("UPDATE downloaders SET is_default = 0", [])?;
            row.is_default = true;
        }
        self.app.execute(
            "UPDATE downloaders SET name = ?1, kind = ?2, url = ?3, username = ?4,
                    password = ?5, category = ?6, path_maps = ?7, enabled = ?9, is_default = ?10
             WHERE id = ?8",
            params![
                row.name,
                row.kind,
                row.url,
                row.username,
                row.password,
                row.category,
                maps_json(&row.path_maps),
                row.id.to_string(),
                row.enabled as i64,
                row.is_default as i64,
            ],
        )?;
        self.sync_api_key(&row)?;
        let connection_or_auth_changed = row.kind != before.kind
            || row.url != before.url
            || row.username != before.username
            || row.password != before.password
            || normalized_api_key(&row.api_key) != normalized_api_key(&before.api_key)
            || row.category != before.category
            || row.path_maps != before.path_maps;
        let verified_key = format!("downloader.{}.verified", row.id);
        let error_key = format!("downloader.{}.last_error", row.id);
        if connection_or_auth_changed {
            self.delete_setting(&verified_key)?;
            self.delete_setting(&error_key)?;
        }
        let verified = self.get_setting(&verified_key)?;
        let last_error = self.get_setting(&error_key)?;
        tx.commit()?;
        Ok(Some((row, verified, last_error)))
    }
}

impl crate::Store {
    pub fn mutate_downloader<F>(
        &self,
        id: DownloaderId,
        make_default: bool,
        mutate: F,
    ) -> Result<Option<(DownloaderRow, Option<String>, Option<String>)>, StoreError>
    where
        F: FnOnce(&mut DownloaderRow) -> Result<(), StoreError>,
    {
        self.with_app(|app| app.mutate_downloader(id, make_default, mutate))
    }
}

mod app_access;
