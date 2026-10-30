use crate::library_query::types::{LibraryEpisode, LibraryUnit, UnitPage};
use crate::{Store, StoreError};
use domain::{LedgerId, LibraryId, MediaId, UnitKey};
use rusqlite::{params, params_from_iter};
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct UnitCursor {
    library_id: LibraryId,
    media_id: MediaId,
    season: Option<i64>,
    order: String,
    last_season: i64,
    last_episode: i64,
}

const UNIT_CURSOR_ORDER: &str = "season_episode_asc";

impl Store {
    pub fn library_unit_page(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        season: Option<u32>,
        page: &crate::library_query::types::BrowsePage,
    ) -> Result<UnitPage, StoreError> {
        self.with_library_read(|db| db.library_unit_page(library_id, media_id, season, page))
    }

    pub fn library_episodes(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        season: Option<u32>,
        locale: &str,
    ) -> Result<Vec<LibraryEpisode>, StoreError> {
        self.with_library_read(|db| db.library_episodes(library_id, media_id, season, locale))
    }

    fn map_episode_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LibraryEpisode> {
        let season: i64 = row.get(0)?;
        let episode: i64 = row.get(1)?;
        let runtime: Option<i64> = row.get(5)?;
        Ok(LibraryEpisode {
            season: season as u32,
            episode: episode as u32,
            title: row.get(2)?,
            overview: row.get(3)?,
            air_date: row.get(4)?,
            runtime_minutes: runtime.map(|minutes| minutes as u32),
            rating: row.get(6)?,
            still_url: row.get(7)?,
            owned: false,
            file_ids: Vec::new(),
        })
    }
}

#[path = "episodes/connection.rs"]
mod connection;
