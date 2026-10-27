//! Write-time all-locale search haystack and protocol wire-ID helpers.
use std::collections::BTreeSet;

use rusqlite::{Connection, params};
use uuid::Uuid;

use crate::StoreError;

/// Term kinds this module owns; rebuilt as a unit for one card.
/// Separator between haystack fields; a trimmed search needle never spans fields.
pub(crate) const SEARCH_SEPARATOR: char = '\u{1f}';

/// Rebuild the protocol index of one card. Call after `library_items` and the
/// `genre` terms of the card are current.
pub(crate) fn sync_card(tx: &Connection, lib: &str, media: &str) -> Result<(), StoreError> {
    let title: String = tx.query_row(
        "SELECT title FROM library_items WHERE library_id=?1 AND media_id=?2",
        params![lib, media],
        |r| r.get(0),
    )?;
    let haystack = format!(
        "{}{}{}",
        title.to_lowercase(),
        SEARCH_SEPARATOR,
        search_text(tx, lib, media)?
    );
    tx.execute(
        "UPDATE library_items SET search_text = ?3 WHERE library_id = ?1 AND media_id = ?2",
        params![lib, media, haystack],
    )?;
    Ok(())
}

/// JSON scalars come back typed (a numeric `tmdb_id` is an integer); compare as text.
fn text(value: rusqlite::types::Value) -> Option<String> {
    match value {
        rusqlite::types::Value::Text(t) => Some(t),
        rusqlite::types::Value::Integer(i) => Some(i.to_string()),
        rusqlite::types::Value::Real(r) => Some(r.to_string()),
        _ => None,
    }
}

fn search_text(tx: &Connection, lib: &str, media: &str) -> Result<String, StoreError> {
    let sql = "SELECT json_extract(mi.fields_json, '$.original_title.value')
         FROM library_metadata_inputs mi WHERE mi.library_id = ?1 AND mi.media_id = ?2
          AND mi.entity_scope = 'work' AND mi.parse_state = 'valid'
       UNION ALL SELECT json_extract(mi.fields_json, '$.overview.value')
         FROM library_metadata_inputs mi WHERE mi.library_id = ?1 AND mi.media_id = ?2
          AND mi.entity_scope = 'work' AND mi.parse_state = 'valid'
       UNION ALL SELECT original_title FROM media_metadata WHERE media_id = ?2
       UNION ALL SELECT overview FROM media_metadata WHERE media_id = ?2";
    let mut stmt = tx.prepare_cached(sql)?;
    let rows = stmt.query_map(params![lib, media], |r| {
        r.get::<_, Option<rusqlite::types::Value>>(0)
    })?;
    let mut seen = BTreeSet::new();
    let mut fields = Vec::new();
    for row in rows {
        let Some(value) = row?.and_then(text) else {
            continue;
        };
        let folded = value.to_lowercase();
        if !folded.is_empty() && seen.insert(folded.clone()) {
            fields.push(folded);
        }
    }
    Ok(fields.join(&SEARCH_SEPARATOR.to_string()))
}

/// Index keys of one genre name: `name:<lower>` plus `id:<uuid5(NAMESPACE_DNS, name)>`.
pub(crate) fn genre_keys(name: &str) -> [String; 2] {
    let uuid = Uuid::new_v5(&Uuid::NAMESPACE_DNS, name.as_bytes());
    [
        format!("name:{}", name.to_lowercase()),
        format!("id:{}", uuid.simple()),
    ]
}

/// Index keys a request value can hit: its lowercase name, and the id it spells
/// (simple, hyphenated or `genre_`-prefixed).
pub(crate) fn requested_genre_keys(wanted: &str) -> Vec<String> {
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return Vec::new();
    }
    let lower = wanted.to_lowercase();
    let id = compact_id(lower.strip_prefix("genre_").unwrap_or(&lower));
    vec![format!("name:{lower}"), format!("id:{id}")]
}

/// Same derivation as `item_metadata::people_from_nfo`.
pub(crate) fn person_wire_id(role: &str, name: &str, tmdb: Option<&str>) -> String {
    let key = tmdb
        .map(|id| format!("tmdb:{id}:{role}"))
        .unwrap_or_else(|| format!("name:{}:{role}", name.to_lowercase()));
    Uuid::new_v5(&Uuid::NAMESPACE_URL, key.as_bytes()).to_string()
}

pub(crate) fn compact_id(id: &str) -> String {
    id.replace('-', "").to_ascii_lowercase()
}

/// Expected search haystack derived only from raw facts.
pub(crate) fn expected_search(
    tx: &Connection,
    lib: &str,
    media: &str,
    title: &str,
) -> Result<String, StoreError> {
    Ok(format!(
        "{}{}{}",
        title.to_lowercase(),
        SEARCH_SEPARATOR,
        search_text(tx, lib, media)?
    ))
}
