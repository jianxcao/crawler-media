//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn playback_log_page(
        &self,
        scope: &PlaybackLogScope,
        limit: usize,
    ) -> Result<PlaybackLogPage, StoreError> {
        if limit == 0 || limit > MAX_PAGE_LIMIT {
            return Err(StoreError::Invalid(format!(
                "playback log limit must be 1..={MAX_PAGE_LIMIT}, got {limit}"
            )));
        }
        if scope.before_id.is_some() && scope.before.is_none() {
            return Err(StoreError::Invalid(
                "playback log cursor requires before when before_id is set".into(),
            ));
        }
        let visible = scope.visible_media.as_deref();
        self.scoped_log_page(scope, visible, limit)
    }

    pub(super) fn scoped_log_page(
        &self,
        scope: &PlaybackLogScope,
        visible: Option<&[MediaId]>,
        limit: usize,
    ) -> Result<PlaybackLogPage, StoreError> {
        let (filter, mut args) = scope_filter(scope);
        args.insert(
            0,
            rusqlite::types::Value::Text(serde_json::to_string(&visible.unwrap_or_default())?),
        );
        let mut page_args = args.clone();
        page_args.push(rusqlite::types::Value::Integer((limit + 1) as i64));
        let page_sql = format!(
            "WITH request_visible_media AS (SELECT value AS id FROM json_each(?)) SELECT {LOG_COLS}{filter}{} ORDER BY l.started_at DESC, l.id DESC LIMIT ?",
            visible_clause(visible)
        );
        let mut stmt = self.library.prepare(&page_sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(page_args), map_log)?;
        let mut entries = rows.collect::<Result<Vec<_>, _>>()?;
        let has_more = entries.len() > limit;
        entries.truncate(limit);
        let hidden_count = match visible {
            None => 0,
            Some(_) => {
                let count_sql = format!(
                    "WITH request_visible_media AS (SELECT value AS id FROM json_each(?)) SELECT COUNT(*){filter}{}",
                    " AND NOT EXISTS (SELECT 1 FROM request_visible_media v WHERE v.id = l.media_id)"
                );
                let mut stmt = self.library.prepare(&count_sql)?;
                let count: i64 =
                    stmt.query_row(rusqlite::params_from_iter(args), |row| row.get(0))?;
                count.max(0) as usize
            }
        };
        Ok(PlaybackLogPage {
            entries,
            has_more,
            hidden_count,
        })
    }
}
