use rusqlite::types::Value;

use super::cursor::{Cursor, Primary};
use super::{FavoriteOrder, PlaybackViewQuery};

pub(super) struct Query {
    pub base: String,
    pub values: Vec<Value>,
}
impl Query {
    pub(super) fn new(q: &PlaybackViewQuery) -> Self {
        let mut values = vec![Value::Text(q.user.to_string())];
        let libraries = if q.libraries.is_empty() {
            "NULL".into()
        } else {
            q.libraries
                .iter()
                .map(|library| {
                    values.push(Value::Text(library.to_string()));
                    format!("?{}", values.len())
                })
                .collect::<Vec<_>>()
                .join(",")
        };
        let owned = format!("i.media_id=f.media_id AND i.library_id IN ({libraries})");
        let watched =
            format!("u.user_id=?1 AND u.media_id=f.media_id AND u.library_id IN ({libraries})");
        let tier = if q.unwatched_first {
            format!(
                "CASE WHEN COALESCE((SELECT MAX(seen) FROM user_library_items u WHERE {watched}),0)=0 THEN 0 WHEN COALESCE((SELECT MAX(played) FROM user_library_items u WHERE {watched}),0)!=0 THEN 2 ELSE 1 END"
            )
        } else {
            "0".into()
        };
        let base = format!(
            "SELECT f.media_id, (SELECT MIN(library_id) FROM library_items i WHERE {owned}) AS library_id,
             f.season, f.episode, f.favorite_at, {tier} AS tier,
             (SELECT MIN(title_sort) FROM library_items i WHERE {owned}) AS title_sort,
             (SELECT MAX(year) FROM library_items i WHERE {owned}) AS year
             FROM playback_units f INDEXED BY playback_favorite_order
             WHERE f.user_id=?1 AND f.favorite=1
             AND NOT EXISTS (SELECT 1 FROM playback_units n INDEXED BY playback_favorite_winner
               WHERE n.user_id=f.user_id AND n.media_id=f.media_id AND n.favorite=1
               AND (n.favorite_at,n.season,n.episode)>(f.favorite_at,f.season,f.episode))
             AND EXISTS (SELECT 1 FROM library_items i WHERE {owned})"
        );
        Self { base, values }
    }
    pub(super) fn bind(&mut self, value: Value) -> String {
        self.values.push(value);
        format!("?{}", self.values.len())
    }
    pub(super) fn order(q: &PlaybackViewQuery, reverse: bool) -> String {
        let desc = q.descending != reverse;
        let direction = if desc { "DESC" } else { "ASC" };
        let primary = match q.order {
            FavoriteOrder::FavoritedAt => "favorite_at",
            FavoriteOrder::Title => "title_sort",
            FavoriteOrder::Year => "year",
        };
        let tier = if q.unwatched_first {
            if reverse { "tier DESC, " } else { "tier ASC, " }
        } else {
            ""
        };
        let media = if reverse { "DESC" } else { "ASC" };
        format!("{tier}{primary} {direction}, media_id {media}")
    }
    pub(super) fn seek(&mut self, q: &PlaybackViewQuery, cursor: &Cursor) -> String {
        let media = self.bind(Value::Text(cursor.media.clone()));
        let tie = format!("media_id>{media}");
        let (column, primary) = match &cursor.primary {
            Primary::Time(value) => ("favorite_at", Some(Value::Integer(*value))),
            Primary::Title(value) => ("title_sort", Some(Value::Text(value.clone()))),
            Primary::Year(value) => ("year", value.map(Value::Integer)),
        };
        let primary_after = match primary {
            None if q.descending => format!("({column} IS NULL AND {tie})"),
            None => format!("({column} IS NOT NULL OR ({column} IS NULL AND {tie}))"),
            Some(value) => {
                let value = self.bind(value);
                let operator = if q.descending { "<" } else { ">" };
                let nulls = if q.descending && matches!(q.order, FavoriteOrder::Year) {
                    format!("{column} IS NULL OR ")
                } else {
                    String::new()
                };
                format!("({nulls}{column}{operator}{value} OR ({column}={value} AND {tie}))")
            }
        };
        if q.unwatched_first {
            let tier = self.bind(Value::Integer(cursor.tier));
            format!("(tier>{tier} OR (tier={tier} AND {primary_after}))")
        } else {
            primary_after
        }
    }
}
