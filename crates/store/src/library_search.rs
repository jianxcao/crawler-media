//! Cross-library card search: title `search_text` and cast/crew `name_key`.
//!
//! Bounded ordered windows with indexed exhaustive fallback. Callers pass the Libraries the
//! viewer may see; each logical Media chooses one matching visible card.

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
        self.with_library_read(|db| db.search_library_items(libraries, keyword, limit, cursor))
    }

    /// Explain the same query and parameters used by the production search reader.
    #[cfg(any(test, feature = "test-support"))]
    pub fn search_library_items_query_plans(
        &self,
        libraries: &[LibraryId],
        keyword: &str,
        limit: Option<usize>,
        cursor: Option<&str>,
    ) -> Result<Vec<String>, StoreError> {
        let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let queries = sql::queries(libraries, keyword, limit, cursor)?;
        self.with_library_read(|db| {
            let ids = db
                .library
                .prepare_cached(&queries.probe.0)?
                .query_map(params_from_iter(queries.probe.1.iter()), |row| {
                    row.get::<_, i64>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let candidates = sql::candidate_page(libraries, keyword, limit, cursor, &ids)?;
            let mut plans = Vec::new();
            for (phase, (sql, binds)) in [
                ("probe", queries.probe),
                ("few_candidates", candidates),
                ("window", queries.window),
                ("fallback", queries.fallback),
            ] {
                let mut statement = db.library.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
                let rows =
                    statement.query_map(params_from_iter(binds), |row| row.get::<_, String>(3))?;
                for row in rows {
                    plans.push(format!("{phase}: {}", row?));
                }
            }
            Ok(plans)
        })
    }
}

/// A bound literal phrase is an FTS candidate filter, never user query syntax.
pub(crate) fn fts_phrase(term: &str) -> Option<String> {
    let term = term.trim().to_lowercase();
    (term.chars().count() >= 3).then(|| format!("\"{}\"", term.replace('"', "\"\"")))
}

pub(crate) fn fts_rowids(parameter: &str) -> String {
    format!("SELECT rowid FROM library_search_fts WHERE library_search_fts MATCH {parameter}")
}

/// Exclusive Unicode-scalar upper bound for a normalized prefix.
pub(crate) fn prefix_upper(prefix: &str) -> Option<String> {
    let mut scalars: Vec<char> = prefix.chars().collect();
    while let Some(last) = scalars.pop() {
        let mut next = u32::from(last) + 1;
        if next == 0xd800 {
            next = 0xe000;
        }
        if let Some(next) = char::from_u32(next) {
            scalars.push(next);
            return Some(scalars.into_iter().collect());
        }
    }
    None
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

#[path = "library_search/sql.rs"]
mod sql;

#[path = "library_search/connection.rs"]
mod connection;
