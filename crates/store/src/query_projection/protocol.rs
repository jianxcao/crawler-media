use super::types::{Person, Terms};
use crate::StoreError;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use std::collections::BTreeSet;

/// NFO and adopted are selected independently, exactly as protocol scoped SQL.
pub(super) fn selected(
    tx: &Connection,
    lib: &str,
    media: &str,
) -> Result<(Value, Value, Value), StoreError> {
    let locale: Option<String> = tx.query_row("SELECT COALESCE(NULLIF(TRIM(display_locale),''),(SELECT default_locale FROM library_query_config WHERE id=1)) FROM libraries WHERE library_id=?1", [lib], |r|r.get(0))?;
    let nfo: Option<String> = tx.query_row("SELECT fields_json FROM library_metadata_inputs WHERE library_id=?1 AND media_id=?2 AND entity_scope='work' AND parse_state='valid' ORDER BY (locale=?3) DESC,locale,input_key LIMIT 1", params![lib,media,locale], |r|r.get(0)).optional()?;
    let adopted: Option<(String,String)> = tx.query_row("SELECT details_json,genres_json FROM media_metadata WHERE media_id=?1 ORDER BY (locale=?2) DESC,locale LIMIT 1", params![media,locale], |r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let (details, genres) = adopted.unwrap_or_else(|| ("{}".into(), "[]".into()));
    Ok((
        serde_json::from_str(nfo.as_deref().unwrap_or("{}"))?,
        serde_json::from_str(&details)?,
        serde_json::from_str(&genres)?,
    ))
}
pub(super) fn derive(
    tx: &Connection,
    lib: &str,
    media: &str,
    web: &Terms,
) -> Result<(Terms, BTreeSet<Person>), StoreError> {
    let (nfo, adopted, genres) = selected(tx, lib, media)?;
    let mut terms = Terms::new();
    for (kind, value) in web {
        if kind == "genre" {
            add_genre(&mut terms, value);
        }
        if kind == "studio" {
            add_studio(&mut terms, value);
        }
    }
    for values in [&genres, &nfo["genres"]["value"]] {
        for value in values.as_array().into_iter().flatten() {
            if let Some(name) = value.as_str().or_else(|| value["name"].as_str()) {
                add_genre(&mut terms, name);
            }
        }
    }
    for values in [
        &nfo["studios"]["value"],
        &adopted["fields"]["studios"]["value"],
        &adopted["fields"]["studios"],
    ] {
        for value in values.as_array().into_iter().flatten() {
            if let Some(name) = value.as_str() {
                add_studio(&mut terms, name);
            }
        }
    }
    Ok((terms, super::people::derive(&nfo, &adopted["fields"])))
}
fn add_genre(terms: &mut Terms, name: &str) {
    if !name.trim().is_empty() {
        for key in crate::protocol_index::genre_keys(name) {
            terms.insert(("genre".into(), key));
        }
    }
}
fn add_studio(terms: &mut Terms, name: &str) {
    if !name.trim().is_empty() {
        terms.insert(("studio".into(), name.to_lowercase()));
    }
}
