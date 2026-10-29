//! Cross-library card search: title `search_text` and cast/crew `name_key`.
//!
//! One SQL page, never a whole-ledger scan. Callers pass the Libraries the
//! viewer may see; results are cards (`library_items`), not every file.

use std::str::FromStr;

use domain::{LibraryId, MediaId, MediaKind};
use rusqlite::params_from_iter;
use serde::{Deserialize, Serialize};

use super::{Store, StoreError};

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 100;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibrarySearchHit {
    pub library_id: LibraryId,
    pub media_id: MediaId,
    pub title: String,
    pub kind: MediaKind,
    pub year: Option<u16>,
    /// `title` when `search_text` matched; `person` when only a cast/crew name matched.
    pub matched_by: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibrarySearchPage {
    pub items: Vec<LibrarySearchHit>,
    pub next_cursor: Option<String>,
}

struct Row {
    hit: LibrarySearchHit,
    title_sort: String,
}

impl Store {
    /// Search visible Libraries for `keyword` in card `search_text` or person `name_key`.
    ///
    /// `limit` defaults to 50 and caps at 100. `cursor` is
    /// `title_sort` + unit separator + `media_id` of the last hit of the previous page.
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
        let needle = keyword.trim().to_lowercase();
        let (after_sort, after_media) = parse_cursor(cursor)?;

        let mut binds: Vec<rusqlite::types::Value> = vec![needle.into()];
        let mut next = 2usize;
        let mut in_parts = Vec::with_capacity(libraries.len());
        for id in libraries {
            in_parts.push(format!("?{next}"));
            binds.push(id.to_string().into());
            next += 1;
        }
        let sort_ph = next;
        next += 1;
        let media_ph = next;
        next += 1;
        let limit_ph = next;
        binds.push(match after_sort {
            Some(value) => value.into(),
            None => rusqlite::types::Value::Null,
        });
        binds.push(match after_media {
            Some(value) => value.into(),
            None => rusqlite::types::Value::Null,
        });
        binds.push(((limit + 1) as i64).into());

        let sql = format!(
            "SELECT i.library_id, i.media_id, i.title, i.kind, i.year, i.title_sort,
                    CASE
                      WHEN ?1 = '' OR instr(i.search_text, ?1) > 0 THEN 'title'
                      ELSE 'person'
                    END AS matched_by
             FROM library_items i
             WHERE i.library_id IN ({libs})
               AND (
                 ?1 = ''
                 OR instr(i.search_text, ?1) > 0
                 OR EXISTS (
                   SELECT 1 FROM library_protocol_people p
                   WHERE p.library_id = i.library_id
                     AND p.media_id = i.media_id
                     AND instr(p.name_key, ?1) > 0
                 )
               )
               AND (
                 ?{sort_ph} IS NULL
                 OR i.title_sort > ?{sort_ph}
                 OR (i.title_sort = ?{sort_ph} AND i.media_id > ?{media_ph})
               )
             ORDER BY i.title_sort ASC, i.media_id ASC
             LIMIT ?{limit_ph}",
            libs = in_parts.join(", ")
        );

        let mut stmt = self.library.prepare(&sql)?;
        let mapped = stmt.query_map(params_from_iter(binds), |row| {
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

        let mut rows = Vec::new();
        for row in mapped {
            let (library_id, media_id, title, kind, year, title_sort, matched_by) = row?;
            rows.push(Row {
                hit: LibrarySearchHit {
                    library_id: library_id.parse()?,
                    media_id: media_id.parse()?,
                    title,
                    kind: MediaKind::from_str(&kind).map_err(StoreError::MediaKind)?,
                    year: year.and_then(|y| u16::try_from(y).ok()),
                    matched_by: if matched_by == "person" {
                        "person"
                    } else {
                        "title"
                    },
                },
                title_sort,
            });
        }

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

fn encode_cursor(title_sort: &str, media_id: &MediaId) -> String {
    format!("{title_sort}\u{1f}{media_id}")
}

fn parse_cursor(cursor: Option<&str>) -> Result<(Option<String>, Option<String>), StoreError> {
    let Some(cursor) = cursor.map(str::trim).filter(|c| !c.is_empty()) else {
        return Ok((None, None));
    };
    let Some((sort, media)) = cursor.split_once('\u{1f}') else {
        return Err(StoreError::Invalid("search cursor malformed".into()));
    };
    let _: MediaId = media
        .parse()
        .map_err(|_| StoreError::Invalid("search cursor media_id".into()))?;
    Ok((Some(sort.to_string()), Some(media.to_string())))
}
