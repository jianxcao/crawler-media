use super::{ExpectedProjection, ProjectionKey, expected::OwnedRow};
use crate::{StoreError, library_operations::integrity_guard::IntegrityOwner};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value};

pub(crate) fn repair_projection(
    tx: &Connection,
    owner: &IntegrityOwner,
    key: ProjectionKey,
    expected: &ExpectedProjection,
) -> Result<(), StoreError> {
    owner.validate(tx, key.library_id)?;
    let (lib, media) = (key.library_id.to_string(), key.media_id.to_string());
    // Counters are intentionally untouched: corrupt undercounts cannot prevent repair.
    for table in [
        "library_item_terms",
        "library_protocol_terms",
        "library_protocol_people",
    ] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE library_id=?1 AND media_id=?2"),
            params![lib, media],
        )?;
    }
    let Some(card) = &expected.card else {
        tx.execute(
            "DELETE FROM library_units WHERE library_id=?1 AND media_id=?2",
            params![lib, media],
        )?;
        tx.execute(
            "DELETE FROM library_items WHERE library_id=?1 AND media_id=?2",
            params![lib, media],
        )?;
        return Ok(());
    };
    write_row(tx, "library_items", card)?;
    sync_rows(
        tx,
        "library_units",
        &lib,
        &media,
        &expected.units,
        &["season", "episode"],
    )?;
    sync_rows(
        tx,
        "user_library_items",
        &lib,
        &media,
        &expected.users,
        &["user_id"],
    )?;
    super::browse::sync_terms(
        tx,
        "library_item_terms",
        &lib,
        &media,
        &Default::default(),
        &expected.web,
    )?;
    super::browse::sync_terms(
        tx,
        "library_protocol_terms",
        &lib,
        &media,
        &Default::default(),
        &expected.protocol,
    )?;
    super::people::sync(tx, &lib, &media, &expected.people)?;
    super::mark_current(tx, &lib, &media)?;
    Ok(())
}
fn write_row(tx: &Connection, table: &str, row: &OwnedRow) -> Result<(), StoreError> {
    let mut row = row.clone();
    let revision = match table {
        "library_items" => "item_revision",
        "library_units" => "unit_revision",
        _ => "summary_revision",
    };
    let keys = if table == "library_units" {
        vec!["library_id", "media_id", "season", "episode"]
    } else if table == "user_library_items" {
        vec!["library_id", "media_id", "user_id"]
    } else {
        vec!["library_id", "media_id"]
    };
    let predicate = keys
        .iter()
        .map(|key| format!("{key}=?"))
        .collect::<Vec<_>>()
        .join(" AND ");
    let prior: Option<i64> = tx
        .query_row(
            &format!("SELECT {revision} FROM {table} WHERE {predicate}"),
            params_from_iter(keys.iter().map(|key| &row[*key])),
            |r| r.get(0),
        )
        .optional()?;
    row.insert(revision.into(), prior.unwrap_or(0).saturating_add(1).into());
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
                    "item_revision"
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
pub(crate) fn projection_matches(
    tx: &Connection,
    key: ProjectionKey,
    expected: &ExpectedProjection,
) -> Result<bool, StoreError> {
    let (lib, media) = (key.library_id.to_string(), key.media_id.to_string());
    for (table, rows) in [
        ("library_items", expected.card.clone().into_iter().collect()),
        ("library_units", expected.units.clone()),
        ("user_library_items", expected.users.clone()),
    ] {
        let actual = actual_rows(tx, table, &lib, &media)?;
        if actual.len() != rows.len() || actual.iter().any(|row| !rows.contains(row)) {
            return Ok(false);
        }
    }
    let current:bool=tx.query_row("SELECT NOT EXISTS(SELECT 1 FROM library_items WHERE library_id=?1 AND media_id=?2 AND query_projection_revision<>item_revision)",params![lib,media],|r|r.get(0))?;
    Ok(current
        && super::browse::read_terms(tx, "library_item_terms", &lib, &media)? == expected.web
        && super::browse::read_terms(tx, "library_protocol_terms", &lib, &media)?
            == expected.protocol
        && super::people::read(tx, &lib, &media)? == expected.people)
}

fn sync_rows(
    tx: &Connection,
    table: &str,
    lib: &str,
    media: &str,
    expected: &[OwnedRow],
    keys: &[&str],
) -> Result<(), StoreError> {
    for actual in actual_rows(tx, table, lib, media)? {
        if !expected
            .iter()
            .any(|row| keys.iter().all(|key| row.get(*key) == actual.get(*key)))
        {
            let predicate = keys
                .iter()
                .map(|key| format!("{key}=?"))
                .collect::<Vec<_>>()
                .join(" AND ");
            let mut values = vec![Value::from(lib.to_string()), Value::from(media.to_string())];
            values.extend(keys.iter().map(|key| actual[*key].clone()));
            tx.execute(
                &format!("DELETE FROM {table} WHERE library_id=? AND media_id=? AND {predicate}"),
                params_from_iter(values),
            )?;
        }
    }
    for row in expected {
        write_row(tx, table, row)?;
    }
    Ok(())
}
