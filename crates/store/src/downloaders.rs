use domain::DownloaderId;
use downloader::PathMap;
use rusqlite::{OptionalExtension, params};

use super::{Store, StoreError};

#[derive(Clone, Debug)]
pub struct DownloaderRow {
    pub id: DownloaderId,
    pub name: String,
    pub kind: String,
    pub url: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub category: Option<String>,
    pub path_maps: Vec<PathMap>,
    pub is_default: bool,
    /// Disabled downloaders are skipped for automatic delivery.
    pub enabled: bool,
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

impl Store {
    pub fn insert_downloader(&self, row: &DownloaderRow) -> Result<(), StoreError> {
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
        Ok(())
    }

    pub fn save_downloader(&self, row: &DownloaderRow) -> Result<(), StoreError> {
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
        let mut stmt = self
            .app
            .prepare(&format!("SELECT {COLS} FROM downloaders ORDER BY name"))?;
        let rows = stmt.query_map([], map_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_downloader(&self, id: DownloaderId) -> Result<Option<DownloaderRow>, StoreError> {
        self.app
            .query_row(
                &format!("SELECT {COLS} FROM downloaders WHERE id = ?1"),
                params![id.to_string()],
                map_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn delete_downloader(&self, id: DownloaderId) -> Result<bool, StoreError> {
        let n = self.app.execute(
            "DELETE FROM downloaders WHERE id = ?1",
            params![id.to_string()],
        )?;
        Ok(n > 0)
    }

    pub fn default_downloader(&self) -> Result<Option<DownloaderRow>, StoreError> {
        self.app
            .query_row(
                &format!(
                    "SELECT {COLS} FROM downloaders WHERE is_default = 1 AND enabled = 1 LIMIT 1"
                ),
                [],
                map_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn set_default_downloader(&self, id: DownloaderId) -> Result<bool, StoreError> {
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
        Ok(true)
    }
}
