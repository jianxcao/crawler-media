use domain::{LedgerId, LibraryId, MediaId, MediaKind};
use rusqlite::params;

use super::types::{BrowseOrder, BrowsePage, ItemPage, LibraryItem, LibraryQuery};
use super::super::{Store, StoreError};

impl Store {
    pub fn library_item_page(&self, query: &LibraryQuery) -> Result<ItemPage, StoreError> {
        let lib_id_str = query.library_id.to_string();

        // 1. Calculate total if requested
        let total = if query.include_total {
            let count_sql = "SELECT COUNT(*) FROM library_items WHERE library_id = ?1";
            let c: i64 = self.library.query_row(count_sql, params![lib_id_str], |r| r.get(0))?;
            Some(c as u64)
        } else {
            None
        };

        // 2. Build ORDER BY
        let (order_col, default_desc) = match query.order {
            BrowseOrder::Title => ("title_sort", false),
            BrowseOrder::AddedAt => ("added_at_sort", true),
            BrowseOrder::ReleaseDate => ("release_date_sort", true),
            BrowseOrder::Rating => ("rating_sort", true),
            BrowseOrder::Runtime => ("runtime_sort", true),
            BrowseOrder::Size => ("size_sort", true),
            BrowseOrder::Resolution => ("resolution_rank", true),
            BrowseOrder::Random => ("stable_shuffle_key", false),
            BrowseOrder::LastPlayed => ("media_id", true),
        };

        let dir = if query.descending { "DESC" } else { "ASC" };

        // 3. Pagination offset & limit
        let (offset, limit) = match query.page {
            BrowsePage::Offset { start, limit } => (start, limit),
            BrowsePage::Cursor { .. } => (0, 50),
        };

        let sql = format!(
            "SELECT library_id, media_id, kind, title, title_sort, year, file_count,
                    available_file_count, missing_file_count, unknown_file_count,
                    distinct_episode_count, distinct_season_count, size_bytes,
                    preferred_file_id, has_still
             FROM library_items
             WHERE library_id = ?1
             ORDER BY {order_col} {dir}, media_id ASC
             LIMIT ?2 OFFSET ?3"
        );

        let mut stmt = self.library.prepare(&sql)?;
        let rows = stmt.query_map(params![lib_id_str, limit as i64, offset as i64], |r| {
            let lib_id: LibraryId = r.get::<_, String>(0)?.parse().unwrap();
            let media_id: MediaId = r.get::<_, String>(1)?.parse().unwrap();
            let kind: MediaKind = r.get::<_, String>(2)?.parse().unwrap_or(MediaKind::Movie);
            let year_i: Option<i64> = r.get(5)?;
            let size_i: Option<i64> = r.get(12)?;
            let pref_str: Option<String> = r.get(13)?;
            let pref_id = pref_str.and_then(|s| s.parse::<LedgerId>().ok());
            let still_i: i64 = r.get(14)?;

            Ok(LibraryItem {
                library_id: lib_id,
                media_id,
                kind,
                title: r.get(3)?,
                title_sort: r.get(4)?,
                year: year_i.map(|y| y as u16),
                file_count: r.get::<_, i64>(6)? as usize,
                available_file_count: r.get::<_, i64>(7)? as usize,
                missing_file_count: r.get::<_, i64>(8)? as usize,
                unknown_file_count: r.get::<_, i64>(9)? as usize,
                distinct_episode_count: r.get::<_, i64>(10)? as usize,
                distinct_season_count: r.get::<_, i64>(11)? as usize,
                size_bytes: size_i.map(|s| s as u64),
                preferred_file_id: pref_id,
                has_still: still_i != 0,
            })
        })?;

        let items = rows.collect::<Result<Vec<_>, _>>()?;

        Ok(ItemPage {
            items,
            total,
            next_cursor: None,
        })
    }
}
