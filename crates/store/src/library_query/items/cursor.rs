use rusqlite::types::Value as SqlValue;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::page::{KeyKind, OrderTerm};
use crate::{StoreError, library_query::types::LibraryQuery};

const CURSOR_VERSION: u8 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub(super) enum CursorKey {
    Int(i64),
    Real(f64),
    Text(String),
}

impl CursorKey {
    pub(super) fn to_sql(&self) -> SqlValue {
        match self {
            Self::Int(n) => (*n).into(),
            Self::Real(f) => (*f).into(),
            Self::Text(s) => s.clone().into(),
        }
    }

    fn matches(&self, kind: KeyKind) -> bool {
        matches!(
            (self, kind),
            (Self::Int(_), KeyKind::Int)
                | (Self::Real(_), KeyKind::Real)
                | (Self::Text(_), KeyKind::Text)
        )
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PageCursor {
    version: u8,
    scope: String,
    pub(super) keys: Vec<CursorKey>,
    pub(super) media_id: String,
}

pub(super) fn malformed_cursor() -> StoreError {
    StoreError::Invalid("malformed cursor".into())
}

fn normalize(values: &mut Vec<String>) {
    values.sort_unstable();
    values.dedup();
}

/// Canonicalize OR dimensions so equivalent requests share a cursor scope.
pub(super) fn scope(query: &LibraryQuery) -> Result<String, StoreError> {
    let mut filter = query.filter.clone();
    filter
        .countries
        .iter_mut()
        .for_each(|v| *v = v.to_uppercase());
    filter
        .languages
        .iter_mut()
        .for_each(|v| *v = v.to_lowercase());
    for terms in [
        &mut filter.genres,
        &mut filter.countries,
        &mut filter.resolutions,
    ] {
        terms.retain(|value| !value.trim().is_empty());
    }
    for values in [
        &mut filter.genres,
        &mut filter.countries,
        &mut filter.decades,
        &mut filter.runtimes,
        &mut filter.languages,
        &mut filter.resolutions,
        &mut filter.stock,
    ] {
        normalize(values);
    }
    filter.watch = filter.watch.map(|v| match v.as_str() {
        "watched" => "played".into(),
        "unplayed" | "unseen" => "unwatched".into(),
        _ => v,
    });
    if filter.min_rating == Some(0.0) {
        filter.min_rating = Some(0.0);
    }
    let data = serde_json::to_vec(&(
        query.library_id,
        query.user_id,
        filter,
        query.order,
        query.descending,
    ))
    .map_err(|_| malformed_cursor())?;
    Ok(format!("{:x}", Sha256::digest(data)))
}

pub(super) fn encode(
    scope: &str,
    keys: Vec<CursorKey>,
    media_id: &str,
) -> Result<String, StoreError> {
    serde_json::to_string(&PageCursor {
        version: CURSOR_VERSION,
        scope: scope.to_owned(),
        keys,
        media_id: media_id.to_owned(),
    })
    .map_err(|_| malformed_cursor())
}

pub(super) fn decode(
    raw: &str,
    expected_scope: &str,
    spec: &[OrderTerm],
) -> Result<PageCursor, StoreError> {
    let cursor: PageCursor = serde_json::from_str(raw).map_err(|_| malformed_cursor())?;
    if cursor.version != CURSOR_VERSION
        || cursor.scope != expected_scope
        || cursor.media_id.parse::<domain::MediaId>().is_err()
        || cursor.keys.len() + 1 != spec.len()
        || !cursor
            .keys
            .iter()
            .zip(spec)
            .all(|(key, term)| key.matches(term.kind))
    {
        return Err(malformed_cursor());
    }
    Ok(cursor)
}
