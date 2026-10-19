use domain::{LedgerId, LibraryId, MediaId, MediaKind};
use rusqlite::params;

use super::types::{BrowseOrder, BrowsePage, ItemPage, LibraryItem, LibraryQuery};
use super::super::{Store, StoreError};

impl Store {
    pub fn library_item_page(&self, query: &LibraryQuery) -> Result<ItemPage, StoreError> {
        let lib_id_str = query.library_id.to_string();
        let mut where_clauses = vec!["library_id = ?1".to_string()];
        let mut params_vec: Vec<rusqlite::types::Value> = vec![lib_id_str.into()];

        if let Some(identity) = &query.filter.identity {
            params_vec.push(identity.clone().into());
            where_clauses.push(format!("identity_state = ?{}", params_vec.len()));
        }

        let where_sql = where_clauses.join(" AND ");

        // 1. Calculate total if requested
        let total = if query.include_total {
            let count_sql = format!("SELECT COUNT(*) FROM library_items WHERE {where_sql}");
            let c: i64 = self.library.query_row(&count_sql, rusqlite::params_from_iter(params_vec.iter().cloned()), |r| r.get(0))?;
            Some(c as u64)
        } else {
            None
        };

        // 2. Build ORDER BY
        let (order_col, _default_desc) = match query.order {
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
                    preferred_file_id, has_still, rating_sort, aliases_json
             FROM library_items
             WHERE {where_sql}
             ORDER BY {order_col} {dir}, media_id ASC
             LIMIT ?{} OFFSET ?{}",
            params_vec.len() + 1,
            params_vec.len() + 2
        );

        let mut query_params = params_vec;
        let limit_val = i64::try_from(limit).unwrap_or(i64::MAX);
        let offset_val = i64::try_from(offset).unwrap_or(0);
        query_params.push(limit_val.into());
        query_params.push(offset_val.into());

        let mut stmt = self.library.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(query_params), |r| {
            let lib_id: LibraryId = r.get::<_, String>(0)?.parse().unwrap();
            let media_id: MediaId = r.get::<_, String>(1)?.parse().unwrap();
            let kind: MediaKind = r.get::<_, String>(2)?.parse().unwrap_or(MediaKind::Movie);
            let year_i: Option<i64> = r.get(5)?;
            let size_i: Option<i64> = r.get(12)?;
            let pref_str: Option<String> = r.get(13)?;
            let pref_id = pref_str.and_then(|s| s.parse::<LedgerId>().ok());
            let still_i: i64 = r.get(14)?;
            let rating_f: f64 = r.get(15)?;
            let rating_opt = if rating_f >= 0.0 { Some(rating_f) } else { None };
            let aliases_raw: Option<String> = r.get(16)?;
            let studio = aliases_raw
                .as_deref()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                .and_then(|val| val.get("studio").and_then(|s| s.as_str()).map(str::to_string));

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
                rating: rating_opt,
                studio,
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
