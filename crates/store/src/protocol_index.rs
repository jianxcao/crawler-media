//! Write-time protocol index: Jellyfin genre/person ids and the search haystack.
//!
//! The Jellyfin page used to derive genre/person ids and casefold every title/overview of
//! every item in scope on each request (O(library) JSON work per page). The card reduction
//! now stores the derived values once:
//!
//! - `library_item_terms(term_kind='jellyfin_genre')`: `id:<uuid5 simple>` and `name:<lower>`;
//! - `library_item_terms(term_kind='person_id'|'person_tmdb_id')`: compact wire id / tmdb id;
//! - `library_items.search_text`: casefolded title/original title/overview, `\x1f`-joined.
//!
//! Values are taken from **every** stored locale (all valid work NFO inputs and every
//! `media_metadata` row), so the index is a sound prefilter independent of the display
//! locale: a display-language change cannot make it miss a row. The query keeps the exact
//! locale-chained predicate on the (few) candidates, so semantics are unchanged.

use std::collections::BTreeSet;

use rusqlite::{Connection, params};
use uuid::Uuid;

use crate::StoreError;

/// Term kinds this module owns; rebuilt as a unit for one card.
const KINDS: &str = "('jellyfin_genre', 'person_id', 'person_tmdb_id')";

/// Separator between haystack fields; a trimmed search needle never spans fields.
pub(crate) const SEARCH_SEPARATOR: char = '\u{1f}';

/// Rebuild the protocol index of one card. Call after `library_items` and the
/// `genre` terms of the card are current.
pub(crate) fn sync_card(tx: &Connection, lib: &str, media: &str) -> Result<(), StoreError> {
    tx.execute(
        &format!(
            "DELETE FROM library_item_terms
             WHERE library_id = ?1 AND media_id = ?2 AND term_kind IN {KINDS}"
        ),
        params![lib, media],
    )?;
    let mut terms: BTreeSet<(&'static str, String)> = BTreeSet::new();
    for name in genre_names(tx, lib, media)? {
        for key in genre_keys(&name) {
            terms.insert(("jellyfin_genre", key));
        }
    }
    for (role, name, tmdb) in people(tx, lib, media)? {
        terms.insert((
            "person_id",
            compact_id(&person_wire_id(role, &name, tmdb.as_deref())),
        ));
        if let Some(tmdb) = tmdb {
            terms.insert(("person_tmdb_id", tmdb));
        }
    }
    let mut insert = tx.prepare_cached(
        "INSERT OR IGNORE INTO library_item_terms (library_id, term_kind, term_value, media_id)
         VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (kind, value) in &terms {
        insert.execute(params![lib, kind, value, media])?;
    }
    drop(insert);
    let haystack = search_text(tx, lib, media)?;
    tx.execute(
        "UPDATE library_items SET search_text = ?3 WHERE library_id = ?1 AND media_id = ?2",
        params![lib, media, haystack],
    )?;
    Ok(())
}

/// `genre_raw` over one JSON array element (object `{name}` or bare string).
const GENRE_RAW: &str = "CASE g.type WHEN 'object' THEN json_extract(g.value, '$.name') \
     WHEN 'text' THEN g.value END";

fn genre_names(tx: &Connection, lib: &str, media: &str) -> Result<Vec<String>, StoreError> {
    let sql = format!(
        "SELECT name FROM (
           SELECT t.term_value AS name FROM library_item_terms t
            WHERE t.library_id = ?1 AND t.media_id = ?2 AND t.term_kind = 'genre'
           UNION SELECT {GENRE_RAW} FROM media_metadata mm,
             json_each(COALESCE(mm.genres_json, '[]')) g WHERE mm.media_id = ?2
           UNION SELECT {GENRE_RAW} FROM library_metadata_inputs mi,
             json_each(mi.fields_json, '$.genres.value') g
            WHERE mi.library_id = ?1 AND mi.media_id = ?2 AND mi.entity_scope = 'work'
              AND mi.parse_state = 'valid'
         ) WHERE name IS NOT NULL AND trim(name) != ''"
    );
    let mut stmt = tx.prepare_cached(&sql)?;
    let rows = stmt
        .query_map(params![lib, media], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn people(
    tx: &Connection,
    lib: &str,
    media: &str,
) -> Result<Vec<(&'static str, String, Option<String>)>, StoreError> {
    const NFO: &str = "FROM library_metadata_inputs mi, json_each(mi.fields_json, '$.{path}.value') j \
         WHERE mi.library_id = ?1 AND mi.media_id = ?2 AND mi.entity_scope = 'work' \
           AND mi.parse_state = 'valid'";
    const ADOPTED: &str = "FROM media_metadata mm, \
         json_each(COALESCE(mm.details_json, '{}'), '$.fields.{path}.value') j \
         WHERE mm.media_id = ?2";
    let mut parts = Vec::new();
    for from in [NFO, ADOPTED] {
        parts.push(format!(
            "SELECT 'Actor', json_extract(j.value, '$.name'), json_extract(j.value, '$.tmdb_id') {}",
            from.replace("{path}", "cast")
        ));
        parts.push(format!(
            "SELECT 'Director', j.value, NULL {}",
            from.replace("{path}", "directors")
        ));
        parts.push(format!(
            "SELECT 'Writer', j.value, NULL {}",
            from.replace("{path}", "creators")
        ));
    }
    let sql = parts.join(" UNION ");
    let mut stmt = tx.prepare_cached(&sql)?;
    let rows = stmt.query_map(params![lib, media], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<rusqlite::types::Value>>(1)?,
            r.get::<_, Option<rusqlite::types::Value>>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (role, name, tmdb) = row?;
        let Some(role) = role_from_wire(&role) else {
            continue;
        };
        let Some(name) = name.and_then(text).filter(|n| !n.trim().is_empty()) else {
            continue;
        };
        let tmdb = tmdb.and_then(text).map(|t| t.trim().to_string());
        out.push((role, name, tmdb.filter(|t| !t.is_empty())));
    }
    Ok(out)
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
    let sql = "SELECT title FROM library_items WHERE library_id = ?1 AND media_id = ?2
       UNION ALL SELECT json_extract(mi.fields_json, '$.original_title.value')
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

pub(crate) fn role_from_wire(role: &str) -> Option<&'static str> {
    match role {
        "Actor" => Some("Actor"),
        "Director" => Some("Director"),
        "Writer" => Some("Writer"),
        _ => None,
    }
}

pub(crate) fn compact_id(id: &str) -> String {
    id.replace('-', "").to_ascii_lowercase()
}
