//! Authorized playback view pages selected in SQL from sparse playback facts.

use domain::{LibraryId, MediaId, UnitKey, UserId};
use rusqlite::params_from_iter;
use rusqlite::types::Value;

use crate::{Store, StoreError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FavoriteOrder {
    FavoritedAt,
    Title,
    Year,
}

#[derive(Clone, Debug)]
pub struct PlaybackViewQuery {
    pub user: UserId,
    pub libraries: Vec<LibraryId>,
    pub limit: usize,
    pub offset: usize,
    pub order: FavoriteOrder,
    pub descending: bool,
    pub unwatched_first: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackViewRow {
    pub media_id: MediaId,
    pub library_id: LibraryId,
    /// Most recently favorited unit of the Media.
    pub unit: UnitKey,
    pub favorited_at: i64,
}

#[derive(Clone, Debug, Default)]
pub struct PlaybackViewPage {
    pub items: Vec<PlaybackViewRow>,
    pub total: usize,
}

impl Store {
    /// One page of distinct favorited Media in the authorized libraries; `total`
    /// uses the same predicate. Cross-library duplicates collapse before LIMIT.
    pub fn playback_favorite_page(
        &self,
        q: &PlaybackViewQuery,
    ) -> Result<PlaybackViewPage, StoreError> {
        if q.libraries.is_empty() || q.limit == 0 {
            return Ok(PlaybackViewPage::default());
        }
        let libs = vec!["?"; q.libraries.len()].join(",");
        let base = format!(
            "WITH fav AS (
               SELECT media_id, season, episode, favorite_at,
                      ROW_NUMBER() OVER (PARTITION BY media_id
                        ORDER BY favorite_at DESC, season DESC, episode DESC) AS rn
               FROM playback_units WHERE user_id = ?1 AND favorite != 0),
             item AS (
               SELECT media_id, MIN(library_id) AS library_id, MIN(title_sort) AS title_sort,
                      MAX(year) AS year
               FROM library_items WHERE library_id IN ({libs}) GROUP BY media_id),
             tier AS (
               SELECT media_id, MAX(seen) AS seen, MAX(played) AS played
               FROM user_library_items WHERE user_id = ?1 AND library_id IN ({libs})
               GROUP BY media_id)
             SELECT f.media_id, i.library_id, f.season, f.episode, f.favorite_at,
                    CASE WHEN COALESCE(t.seen,0)=0 THEN 0 WHEN t.played!=0 THEN 2 ELSE 1 END AS tier,
                    i.title_sort, i.year
             FROM fav f JOIN item i ON i.media_id = f.media_id
             LEFT JOIN tier t ON t.media_id = f.media_id
             WHERE f.rn = 1"
        );
        let mut args: Vec<Value> = vec![Value::Text(q.user.to_string())];
        for _ in 0..2 {
            args.extend(q.libraries.iter().map(|l| Value::Text(l.to_string())));
        }
        // Positional `?` after `?1` continue numbering from 2, so both IN lists bind in order.
        let total: i64 = self.library.query_row(
            &format!("SELECT COUNT(*) FROM ({base})"),
            params_from_iter(args.iter()),
            |row| row.get(0),
        )?;
        let dir = if q.descending { "DESC" } else { "ASC" };
        let primary = match q.order {
            FavoriteOrder::FavoritedAt => format!("favorite_at {dir}"),
            FavoriteOrder::Title => format!("title_sort {dir}"),
            FavoriteOrder::Year => format!("year {dir}"),
        };
        let tier = if q.unwatched_first { "tier ASC, " } else { "" };
        let sql = format!(
            "SELECT * FROM ({base}) ORDER BY {tier}{primary}, media_id ASC LIMIT ? OFFSET ?"
        );
        args.push(Value::Integer(q.limit as i64));
        args.push(Value::Integer(q.offset as i64));
        let mut stmt = self.library.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i32>(2)?,
                row.get::<_, i32>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;
        let mut items = Vec::new();
        for row in rows {
            let (media, library, season, episode, favorited_at) = row?;
            let invalid = |what: &str| StoreError::Invalid(format!("favorite page {what}"));
            items.push(PlaybackViewRow {
                media_id: media.parse().map_err(|_| invalid("media_id"))?,
                library_id: library.parse().map_err(|_| invalid("library_id"))?,
                unit: UnitKey { season, episode },
                favorited_at,
            });
        }
        Ok(PlaybackViewPage {
            items,
            total: total as usize,
        })
    }
}
