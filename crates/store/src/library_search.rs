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
        self.with_library_read(|db| db.search_library_items(libraries, keyword, limit, cursor))
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

#[path = "library_search/connection.rs"]
mod connection;
