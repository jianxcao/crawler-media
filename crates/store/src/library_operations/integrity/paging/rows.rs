//! Typed operation scratch keys; payloads preserve SQLite's value types.
use super::state::Key;
use crate::{StoreError, query_projection::expected::OwnedRow};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
enum Cell {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

pub(super) fn encode(row: &OwnedRow) -> Result<String, StoreError> {
    let cells: BTreeMap<_, _> = row
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                match value {
                    Value::Null => Cell::Null,
                    Value::Integer(n) => Cell::Integer(*n),
                    Value::Real(n) => Cell::Real(*n),
                    Value::Text(s) => Cell::Text(s.clone()),
                    Value::Blob(b) => Cell::Blob(b.clone()),
                },
            )
        })
        .collect();
    Ok(serde_json::to_string(&cells)?)
}

pub(super) fn decode(raw: &str) -> Result<OwnedRow, StoreError> {
    Ok(serde_json::from_str::<BTreeMap<String, Cell>>(raw)?
        .into_iter()
        .map(|(key, cell)| {
            (
                key,
                match cell {
                    Cell::Null => Value::Null,
                    Cell::Integer(n) => Value::Integer(n),
                    Cell::Real(n) => Value::Real(n),
                    Cell::Text(s) => Value::Text(s),
                    Cell::Blob(b) => Value::Blob(b),
                },
            )
        })
        .collect())
}

pub(super) fn text(row: &OwnedRow, key: &str) -> String {
    match row.get(key) {
        Some(Value::Text(s)) => s.clone(),
        _ => String::new(),
    }
}

pub(super) fn key(surface: &str, row: &OwnedRow) -> Key {
    let mut key = Key {
        season: -1,
        episode: -1,
        ..Key::default()
    };
    if matches!(surface, "user" | "continue") {
        key.user = text(row, "user_id");
    }
    if matches!(surface, "unit" | "continue") {
        key.season = crate::query_projection::expected::integer(row, "season") as i32;
        key.episode = crate::query_projection::expected::integer(row, "episode") as i32;
    }
    if surface == "web_term" {
        key.a = text(row, "term_kind");
        key.b = text(row, "term_value");
    }
    key
}

pub(super) fn stage(
    conn: &Connection,
    operation: &str,
    media: &str,
    surface: &str,
    row: &OwnedRow,
) -> Result<(), StoreError> {
    let k = key(surface, row);
    conn.execute("INSERT INTO library_integrity_rows(operation_id,media_id,surface,user_id,season,episode,key_a,key_b,key_c,row_json)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT DO UPDATE SET row_json=excluded.row_json",
        params![operation,media,surface,k.user,k.season,k.episode,k.a,k.b,k.c,encode(row)?])?;
    Ok(())
}

pub(super) fn page(
    conn: &Connection,
    operation: &str,
    media: &str,
    surface: &str,
    after: Option<&Key>,
) -> Result<Vec<OwnedRow>, StoreError> {
    let after = after.cloned().unwrap_or(Key {
        season: i32::MIN,
        episode: i32::MIN,
        ..Key::default()
    });
    conn.prepare_cached("SELECT row_json FROM library_integrity_rows WHERE operation_id=?1 AND media_id=?2 AND surface=?3
        AND (user_id,season,episode,key_a,key_b,key_c)>(?4,?5,?6,?7,?8,?9)
        ORDER BY user_id,season,episode,key_a,key_b,key_c LIMIT 100")?
        .query_map(params![operation,media,surface,after.user,after.season,after.episode,after.a,after.b,after.c], |r| r.get::<_,String>(0))?
        .map(|r| decode(&r?)).collect()
}

pub(super) fn expected(
    conn: &Connection,
    operation: &str,
    media: &str,
    surface: &str,
    key: &Key,
) -> Result<Option<OwnedRow>, StoreError> {
    let raw: Option<String> = conn.query_row("SELECT row_json FROM library_integrity_rows WHERE operation_id=?1 AND media_id=?2 AND surface=?3
        AND user_id=?4 AND season=?5 AND episode=?6 AND key_a=?7 AND key_b=?8 AND key_c=?9",
        params![operation,media,surface,key.user,key.season,key.episode,key.a,key.b,key.c], |r| r.get::<_,String>(0))
        .optional()?;
    raw.map(|raw| decode(&raw)).transpose()
}

pub(super) fn actual(
    conn: &Connection,
    table: &str,
    lib: &str,
    media: &str,
    surface: &str,
    key: &Key,
) -> Result<Option<OwnedRow>, StoreError> {
    let (predicate, values) = predicate(lib, media, surface, key);
    let mut statement = conn.prepare_cached(&format!("SELECT * FROM {table} WHERE {predicate}"))?;
    let names = statement
        .column_names()
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    Ok(statement
        .query_row(params_from_iter(values), |row| read(row, &names))
        .optional()?)
}

pub(super) fn read(row: &rusqlite::Row<'_>, names: &[String]) -> rusqlite::Result<OwnedRow> {
    names
        .iter()
        .enumerate()
        .filter(|(_, name)| {
            !matches!(
                name.as_str(),
                "search_rowid"
                    | "item_revision"
                    | "unit_revision"
                    | "summary_revision"
                    | "query_projection_revision"
            )
        })
        .map(|(i, name)| Ok((name.clone(), row.get(i)?)))
        .collect()
}

pub(super) fn predicate(lib: &str, media: &str, surface: &str, key: &Key) -> (String, Vec<Value>) {
    let mut sql = "library_id=? AND media_id=?".to_string();
    let mut values = vec![lib.to_string().into(), media.to_string().into()];
    if matches!(surface, "user" | "continue") {
        sql.push_str(" AND user_id=?");
        values.push(key.user.clone().into());
    }
    if matches!(surface, "unit" | "continue") {
        sql.push_str(" AND season=? AND episode=?");
        values.extend([key.season.into(), key.episode.into()]);
    }
    if surface == "web_term" {
        sql.push_str(" AND term_kind=? AND term_value=?");
        values.extend([key.a.clone().into(), key.b.clone().into()]);
    }
    (sql, values)
}

pub(super) const TABLES: [(&str, &str); 4] = [
    ("unit", "library_units"),
    ("user", "user_library_items"),
    ("continue", "user_library_continue_units"),
    ("web_term", "library_item_terms"),
];
