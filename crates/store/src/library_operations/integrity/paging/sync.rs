//! Compare expected/actual indexed pages; delete dependants before a parent.
use super::{
    Mutation, rows,
    state::{Key, MediaPage, Phase},
};
use crate::{StoreError, query_projection::expected::OwnedRow};
use rusqlite::{Connection, params, params_from_iter};

pub(super) fn expected_page(
    conn: &Connection,
    operation: &str,
    library: &str,
    page: &mut MediaPage,
    repair: bool,
    verify: bool,
    out: &mut Vec<Mutation>,
) -> Result<(), StoreError> {
    let (surface, table) = rows::TABLES[page.table];
    let expected = rows::page(conn, operation, &page.media, surface, page.after.as_ref())?;
    for row in &expected {
        let key = rows::key(surface, row);
        let actual = rows::actual(conn, table, library, &page.media, surface, &key)?;
        if actual.as_ref() != Some(row) {
            if verify {
                return Err(StoreError::Protected(
                    "Integrity page failed post-repair verification".into(),
                ));
            }
            page.mismatch = true;
            if surface == "user" && actual.is_none() {
                out.push(Mutation::MissingUser(key.user.clone()));
                page.presence_issues += 1;
            }
            if repair {
                out.push(Mutation::Upsert(table, row.clone()));
            }
        }
        page.after = Some(key);
    }
    if expected.len() < 100 {
        next_table(
            page,
            if verify {
                Phase::VerifyReverse
            } else {
                Phase::Reverse
            },
        );
    }
    Ok(())
}

fn next_table(page: &mut MediaPage, next: Phase) {
    page.after = None;
    page.table += 1;
    if page.table == rows::TABLES.len() {
        page.table = 0;
        page.phase = next;
    }
}

pub(super) fn actual_page(
    conn: &Connection,
    operation: &str,
    library: &str,
    page: &mut MediaPage,
    repair: bool,
    verify: bool,
    out: &mut Vec<Mutation>,
) -> Result<(), StoreError> {
    let (surface, table) = rows::TABLES[page.table];
    let actual = read_page(
        conn,
        table,
        library,
        &page.media,
        surface,
        page.after.as_ref(),
    )?;
    let mut remaining = 100usize.saturating_sub(
        out.iter()
            .filter(|action| matches!(action, Mutation::Delete(..)))
            .count(),
    );
    for row in &actual {
        let key = rows::key(surface, row);
        if rows::expected(conn, operation, &page.media, surface, &key)?.is_none() {
            if verify {
                return Err(StoreError::Protected(
                    "Integrity reverse page failed post-repair verification".into(),
                ));
            }
            page.mismatch = true;
            if repair && remaining == 0 {
                return Ok(());
            }
            if repair && matches!(surface, "unit" | "user") {
                let children = children(conn, library, &page.media, surface, &key, remaining)?;
                if !children.is_empty() {
                    for child in children {
                        out.push(Mutation::Delete(
                            "user_library_continue_units",
                            "continue",
                            child,
                        ));
                    }
                    return Ok(());
                }
            }
            if surface == "user" {
                out.push(Mutation::OrphanUser(key.user.clone()));
                page.presence_issues += 1;
            }
            if repair {
                out.push(Mutation::Delete(table, surface, key.clone()));
                remaining -= 1;
            }
        }
        page.after = Some(key);
    }
    if actual.len() < 100 {
        next_table(
            page,
            if verify {
                Phase::VerifyHeader
            } else if repair {
                Phase::Verify
            } else {
                Phase::VerifyHeader
            },
        );
    }
    Ok(())
}

fn children(
    conn: &Connection,
    lib: &str,
    media: &str,
    surface: &str,
    key: &Key,
    limit: usize,
) -> Result<Vec<Key>, StoreError> {
    let (predicate, mut values) = rows::predicate(lib, media, surface, key);
    values.push((limit as i64).into());
    Ok(conn.prepare_cached(&format!("SELECT user_id,season,episode FROM user_library_continue_units WHERE {predicate} ORDER BY user_id,season,episode LIMIT ?"))?
        .query_map(params_from_iter(values),|r|Ok(Key{user:r.get(0)?,season:r.get(1)?,episode:r.get(2)?,..Key::default()}))?
        .collect::<Result<_,_>>()?)
}

fn read_page(
    conn: &Connection,
    table: &str,
    lib: &str,
    media: &str,
    surface: &str,
    after: Option<&Key>,
) -> Result<Vec<OwnedRow>, StoreError> {
    let after = after.cloned().unwrap_or(Key {
        season: i32::MIN,
        episode: i32::MIN,
        ..Key::default()
    });
    let (columns, values) = match surface {
        "unit" => (
            "season,episode",
            vec![after.season.into(), after.episode.into()],
        ),
        "user" => ("user_id", vec![after.user.into()]),
        "continue" => (
            "user_id,season,episode",
            vec![after.user.into(), after.season.into(), after.episode.into()],
        ),
        _ => ("term_kind,term_value", vec![after.a.into(), after.b.into()]),
    };
    let placeholders = vec!["?"; values.len()].join(",");
    let mut bind = vec![
        rusqlite::types::Value::from(lib.to_string()),
        media.to_string().into(),
    ];
    bind.extend(values);
    let mut stmt=conn.prepare_cached(&format!("SELECT * FROM {table} WHERE library_id=? AND media_id=? AND ({columns})>({placeholders}) ORDER BY {columns} LIMIT 100"))?;
    let names = stmt
        .column_names()
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    Ok(stmt
        .query_map(params_from_iter(bind), |r| rows::read(r, &names))?
        .collect::<Result<_, _>>()?)
}

pub(super) fn cleanup(
    conn: &Connection,
    operation: &str,
    page: &MediaPage,
) -> Result<Vec<(String, Key)>, StoreError> {
    Ok(conn.prepare_cached("SELECT surface,user_id,season,episode,key_a,key_b,key_c FROM library_integrity_rows WHERE operation_id=?1 AND media_id=?2 ORDER BY surface,user_id,season,episode,key_a,key_b,key_c LIMIT 100")?
        .query_map(params![operation,page.media],|r|Ok((r.get(0)?,Key{user:r.get(1)?,season:r.get(2)?,episode:r.get(3)?,a:r.get(4)?,b:r.get(5)?,c:r.get(6)?})))?
        .collect::<Result<_,_>>()?)
}
