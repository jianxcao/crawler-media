//! Term maintenance for the incremental card reduction.
//!
//! Terms follow the same rule as the card: only the changed file's resolution can appear
//! or disappear, and the display-driven genre/country/collection rows are diffed against
//! the freshly resolved facts instead of being deleted and rebuilt.

use rusqlite::{Connection, params};

use super::ChangedFile;
use crate::StoreError;
use crate::library_write::insert_term;
use crate::library_write::reduce::Facts;

/// Keep `library_item_terms` in sync without rebuilding the whole card.
pub(super) fn sync_terms_delta(
    tx: &Connection,
    lib: &str,
    media: &str,
    facts: &Facts,
    change: &ChangedFile<'_>,
) -> Result<(), StoreError> {
    let before_resolution = change
        .before
        .filter(|file| file.published)
        .and_then(|file| trimmed(file.resolution.as_deref()));
    let after_resolution = change
        .after
        .published
        .then(|| trimmed(change.after.resolution.as_deref()))
        .flatten();
    if before_resolution != after_resolution {
        if let Some(old) = &before_resolution {
            let covered: i64 = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM library_files
                               WHERE library_id = ?1 AND media_id = ?2
                                 AND publication_state = 'published' AND resolution = ?3)",
                params![lib, media, old],
                |r| r.get(0),
            )?;
            if covered == 0 {
                tx.execute(
                    "DELETE FROM library_item_terms
                     WHERE library_id = ?1 AND media_id = ?2
                       AND term_kind = 'resolution' AND term_value = ?3",
                    params![lib, media, old],
                )?;
            }
        }
        if let Some(new) = &after_resolution {
            insert_term(tx, lib, media, "resolution", new)?;
        }
    }

    let mut desired: Vec<(&str, String)> = Vec::new();
    for genre in &facts.genres {
        if !genre.trim().is_empty() {
            desired.push(("genre", genre.clone()));
        }
    }
    for country in &facts.countries {
        if !country.trim().is_empty() {
            desired.push(("country", country.clone()));
        }
    }
    let mut stmt = tx.prepare(
        "SELECT c.id FROM collections c
         JOIN collection_items ci ON ci.collection_id = c.id
         WHERE ci.media_id = ?1 AND c.visibility = 'public' AND c.user_id IS NULL",
    )?;
    let collections = stmt
        .query_map(params![media], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    for collection in collections {
        desired.push(("collection", collection));
    }

    let mut stmt = tx.prepare(
        "SELECT term_kind, term_value FROM library_item_terms
         WHERE library_id = ?1 AND media_id = ?2
           AND term_kind IN ('genre', 'country', 'collection')",
    )?;
    let existing = stmt
        .query_map(params![lib, media], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    for (kind, value) in &desired {
        if !existing
            .iter()
            .any(|(current_kind, current)| current_kind == kind && current == value)
        {
            insert_term(tx, lib, media, kind, value)?;
        }
    }
    for (kind, value) in &existing {
        if !desired
            .iter()
            .any(|(wanted_kind, wanted)| wanted_kind == kind && wanted == value)
        {
            tx.execute(
                "DELETE FROM library_item_terms
                 WHERE library_id = ?1 AND media_id = ?2 AND term_kind = ?3 AND term_value = ?4",
                params![lib, media, kind, value],
            )?;
        }
    }
    crate::protocol_index::sync_card(tx, lib, media)?;
    Ok(())
}

fn trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}
