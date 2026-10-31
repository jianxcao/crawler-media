//! Favorite candidates and navigation on one explicitly borrowed read snapshot.
use rusqlite::{Row, params_from_iter};

use super::cursor::{Cursor, Primary, invalid};
use super::query::Query;
use super::*;

fn row(row: &Row<'_>, q: &PlaybackViewQuery) -> Result<(PlaybackViewRow, Cursor), StoreError> {
    let media: String = row.get(0)?;
    let library: String = row.get(1)?;
    let primary = match q.order {
        FavoriteOrder::FavoritedAt => Primary::Time(row.get(4)?),
        FavoriteOrder::Title => Primary::Title(row.get(6)?),
        FavoriteOrder::Year => Primary::Year(row.get(7)?),
    };
    let cursor = Cursor::new(q, row.get(5)?, primary, media.clone())?;
    Ok((
        PlaybackViewRow {
            media_id: media.parse().map_err(|_| invalid())?,
            library_id: library.parse().map_err(|_| invalid())?,
            unit: UnitKey {
                season: row.get(2)?,
                episode: row.get(3)?,
            },
            favorited_at: row.get(4)?,
        },
        cursor,
    ))
}
impl crate::LibraryConnection<'_> {
    pub fn playback_favorite_page(
        &self,
        q: &PlaybackViewQuery,
    ) -> Result<PlaybackViewPage, StoreError> {
        let after = q
            .after
            .as_deref()
            .map(|raw| Cursor::decode(q, raw))
            .transpose()?;
        let mut query = Query::new(q);
        let total = if after.is_none() {
            let count: i64 = self.library.query_row(
                &format!("SELECT COUNT(*) FROM ({})", query.base),
                params_from_iter(query.values.iter()),
                |row| row.get(0),
            )?;
            Some(count.max(0) as usize)
        } else {
            None
        };
        let limit = q.limit.clamp(1, 200);
        let predicate = after
            .as_ref()
            .map(|cursor| query.seek(q, cursor))
            .map(|seek| format!(" WHERE {seek}"))
            .unwrap_or_default();
        let sql = format!(
            "SELECT * FROM ({}){predicate} ORDER BY {} LIMIT {}",
            query.base,
            Query::order(q, false),
            limit + 1
        );
        let mut statement = self.library.prepare(&sql)?;
        let mut rows = statement.query(params_from_iter(query.values.iter()))?;
        let mut items = Vec::new();
        let mut boundary = None;
        while let Some(raw) = rows.next()? {
            if items.len() == limit {
                return Ok(PlaybackViewPage {
                    items,
                    total,
                    next_cursor: boundary,
                });
            }
            let (item, cursor) = row(raw, q)?;
            boundary = Some(cursor.encode()?);
            items.push(item);
        }
        Ok(PlaybackViewPage {
            items,
            total,
            next_cursor: None,
        })
    }

    pub fn playback_favorite_position(
        &self,
        q: &PlaybackViewQuery,
        start: usize,
    ) -> Result<Option<String>, StoreError> {
        if q.after.is_some() {
            return Err(StoreError::Invalid(
                "favorite position does not accept after".into(),
            ));
        }
        if start == 0 {
            return Ok(None);
        }
        let query = Query::new(q);
        let offset = i64::try_from(start - 1).unwrap_or(i64::MAX);
        for (reverse, offset) in [(false, offset), (true, 0)] {
            let sql = format!(
                "SELECT * FROM ({}) ORDER BY {} LIMIT 1 OFFSET {offset}",
                query.base,
                Query::order(q, reverse)
            );
            let mut statement = self.library.prepare(&sql)?;
            let mut rows = statement.query(params_from_iter(query.values.iter()))?;
            if let Some(raw) = rows.next()? {
                return Ok(Some(row(raw, q)?.1.encode()?));
            }
        }
        Ok(None)
    }
}
