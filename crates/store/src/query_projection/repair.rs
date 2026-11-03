use super::{ExpectedProjection, ProjectionKey, expected::OwnedRow};
use crate::{StoreError, library_operations::integrity_guard::IntegrityOwner};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value};

pub(crate) fn write_row(tx: &Connection, table: &str, row: &OwnedRow) -> Result<(), StoreError> {
    let mut row = row.clone();
    let revision = match table {
        "library_items" => Some("item_revision"),
        "library_units" => Some("unit_revision"),
        "user_library_items" => Some("summary_revision"),
        _ => None,
    };
    let keys = match table {
        "library_units" => vec!["library_id", "media_id", "season", "episode"],
        "user_library_items" => vec!["library_id", "media_id", "user_id"],
        "user_library_continue_units" => {
            vec!["library_id", "media_id", "user_id", "season", "episode"]
        }
        _ => vec!["library_id", "media_id"],
    };
    if let Some(revision) = revision {
        let predicate = keys
            .iter()
            .map(|key| format!("{key}=?"))
            .collect::<Vec<_>>()
            .join(" AND ");
        let prior: Option<i64> = tx
            .query_row(
                &format!("SELECT {revision} FROM {table} WHERE {predicate}"),
                params_from_iter(keys.iter().map(|key| &row[*key])),
                |row| row.get(0),
            )
            .optional()?;
        row.insert(revision.into(), prior.unwrap_or(0).saturating_add(1).into());
    }
    let names = row.keys().cloned().collect::<Vec<_>>();
    let columns = names.join(",");
    let placeholders = vec!["?"; row.len()].join(",");
    let updates = names
        .iter()
        .map(|n| format!("{n}=excluded.{n}"))
        .collect::<Vec<_>>()
        .join(",");
    tx.execute(&format!("INSERT INTO {table}({columns}) VALUES({placeholders}) ON CONFLICT DO UPDATE SET {updates}"),params_from_iter(row.values()))?;
    Ok(())
}
pub(crate) fn actual_rows(
    tx: &Connection,
    table: &str,
    lib: &str,
    media: &str,
) -> Result<Vec<OwnedRow>, StoreError> {
    let mut stmt = tx.prepare_cached(&format!(
        "SELECT * FROM {table} WHERE library_id=?1 AND media_id=?2"
    ))?;
    let names = stmt
        .column_names()
        .iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>();
    Ok(stmt
        .query_map(params![lib, media], |r| {
            let mut row = OwnedRow::new();
            for (index, name) in names.iter().enumerate() {
                if matches!(
                    name.as_str(),
                    "search_rowid"
                        | "item_revision"
                        | "unit_revision"
                        | "summary_revision"
                        | "query_projection_revision"
                ) {
                    continue;
                }
                row.insert(name.clone(), r.get::<_, Value>(index)?);
            }
            Ok(row)
        })?
        .collect::<Result<_, _>>()?)
}
pub(crate) fn header_matches(
    tx: &Connection,
    key: ProjectionKey,
    expected: &ExpectedProjection,
) -> Result<bool, StoreError> {
    let (lib, media) = (key.library_id.to_string(), key.media_id.to_string());
    let actual = actual_rows(tx, "library_items", &lib, &media)?;
    if actual != expected.card.clone().into_iter().collect::<Vec<_>>() {
        return Ok(false);
    }
    let current:bool=tx.query_row("SELECT NOT EXISTS(SELECT 1 FROM library_items WHERE library_id=?1 AND media_id=?2 AND query_projection_revision<>item_revision)",params![lib,media],|r|r.get(0))?;
    Ok(current
        && super::browse::read_terms(tx, "library_protocol_terms", &lib, &media)?
            == expected.protocol
        && super::people::read(tx, &lib, &media)? == expected.people
        && super::entities::read(tx, &lib, &media)? == expected.entities
        && super::entities::sources_match(tx, &lib, &media)?
        && super::search_integrity::matches(tx, key, expected)?)
}

pub(crate) fn repair_header(
    tx: &Connection,
    owner: &IntegrityOwner,
    key: ProjectionKey,
    expected: &ExpectedProjection,
) -> Result<(), StoreError> {
    owner.validate(tx, key.library_id)?;
    let (lib, media) = (key.library_id.to_string(), key.media_id.to_string());
    let old_search_rowid = super::search::rowid(tx, key)?;
    super::search::remove(tx, old_search_rowid)?;
    let old_entities = super::entities::ownership(tx, &media)?;
    for table in ["library_protocol_terms", "library_protocol_people"] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE library_id=?1 AND media_id=?2"),
            params![lib, media],
        )?;
    }
    super::entities::sync(tx, &lib, &media, &Default::default(), &old_entities)?;
    if let Some(card) = &expected.card {
        write_row(tx, "library_items", card)?;
        super::browse::sync_terms(
            tx,
            "library_protocol_terms",
            &lib,
            &media,
            &Default::default(),
            &expected.protocol,
        )?;
        super::people::sync(tx, &lib, &media, &expected.people)?;
        let before = super::entities::ownership(tx, &media)?;
        super::entities::sync(tx, &lib, &media, &expected.entities, &before)?;
        // The first available coordinate was already reduced from bounded raw
        // coverage pages. Publishing it must not rescan all unavailable units.
        tx.execute("UPDATE library_items SET query_projection_revision=item_revision WHERE library_id=?1 AND media_id=?2",params![lib,media])?;
        super::search::sync(tx, key, &expected.people)?;
    } else {
        tx.execute(
            "DELETE FROM library_items WHERE library_id=?1 AND media_id=?2",
            params![lib, media],
        )?;
    }
    super::search_integrity::refresh(tx, key, old_search_rowid, expected)
}
