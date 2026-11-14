//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn search_library_items(
        &self,
        libraries: &[LibraryId],
        keyword: &str,
        limit: Option<usize>,
        cursor: Option<&str>,
    ) -> Result<LibrarySearchPage, StoreError> {
        let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        if libraries.is_empty() {
            return Ok(LibrarySearchPage {
                items: Vec::new(),
                next_cursor: None,
            });
        }
        for library in libraries {
            crate::library_query::items::page::ensure_library_query_ready_on(
                self.library,
                *library,
            )?;
        }
        let queries = super::sql::queries(libraries, keyword, limit, cursor)?;
        let candidates = self
            .library
            .prepare_cached(&queries.probe.0)?
            .query_map(params_from_iter(queries.probe.1.iter()), |row| {
                row.get::<_, i64>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let rows = if candidates.len() > limit {
            let window = read_rows(self.library, &queries.window)?;
            if window.len() > limit {
                window
            } else {
                read_rows(self.library, &queries.fallback)?
            }
        } else {
            // The bounded probe exhausted every potential hit. Reuse those keys
            // instead of initializing/seeking the FTS posting lists a second time.
            let query = super::sql::candidate_page(libraries, keyword, limit, cursor, &candidates)?;
            read_rows(self.library, &query)?
        };
        let mut rows = rows;

        let next_cursor = if rows.len() > limit {
            rows.pop();
            rows.last()
                .map(|row| encode_cursor(&row.title_sort, &row.hit.media_id))
        } else {
            None
        };
        Ok(LibrarySearchPage {
            items: rows.into_iter().map(|row| row.hit).collect(),
            next_cursor,
        })
    }
}

fn read_rows(
    connection: &rusqlite::Connection,
    query: &super::sql::Query,
) -> Result<Vec<Row>, StoreError> {
    let mut statement = connection.prepare_cached(&query.0)?;
    let mapped = statement.query_map(params_from_iter(query.1.iter()), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<i64>>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
        ))
    })?;
    mapped
        .map(|row| {
            let (library_id, media_id, title, kind, year, title_sort, matched_by) = row?;
            Ok(Row {
                hit: LibrarySearchHit {
                    library_id: library_id.parse()?,
                    media_id: media_id.parse()?,
                    title,
                    kind: MediaKind::from_str(&kind).map_err(StoreError::MediaKind)?,
                    year: year.and_then(|year| u16::try_from(year).ok()),
                    matched_by: if matched_by == "person" {
                        "person"
                    } else {
                        "title"
                    },
                },
                title_sort,
            })
        })
        .collect()
}
