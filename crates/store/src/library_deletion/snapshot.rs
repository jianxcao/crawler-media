use rusqlite::{Connection, OptionalExtension, params};

use crate::StoreError;

pub(super) struct RemovalRow {
    pub ledger_id: String,
    pub root_id: String,
    pub path_key: String,
    pub path: String,
    pub media_id: String,
    pub revision: i64,
    pub source_version: String,
    pub coverage: Vec<(i32, i32)>,
}

pub(super) fn capture_batch(tx: &Connection, id: &str) -> Result<bool, StoreError> {
    let (library, cursor, done, revision): (String, String, bool, i64) = tx.query_row(
        "SELECT library_id,snapshot_cursor,snapshot_done,config_revision FROM library_removals
         WHERE operation_id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    if done {
        return Ok(true);
    }
    let current: i64 = tx.query_row(
        "SELECT config_revision FROM libraries WHERE library_id=?1",
        [&library],
        |r| r.get(0),
    )?;
    if current != revision {
        return Err(StoreError::Protected("删除期间 Library 配置已改变".into()));
    }
    let mut statement = tx.prepare(
        "SELECT file_id,root_id,path_key,display_path,media_id,file_revision,source_version
         FROM library_files WHERE library_id=?1 AND file_id>?2 ORDER BY file_id LIMIT 100",
    )?;
    let mut rows = statement
        .query_map(params![library, cursor], |r| {
            Ok(RemovalRow {
                ledger_id: r.get(0)?,
                root_id: r.get(1)?,
                path_key: r.get(2)?,
                path: r.get(3)?,
                media_id: r.get(4)?,
                revision: r.get(5)?,
                source_version: r.get(6)?,
                coverage: Vec::new(),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for row in &mut rows {
        row.coverage = units(tx, &row.ledger_id)?;
        tx.execute(
            "INSERT INTO library_removal_files(operation_id,file_id,root_id,path_key,display_path,
             media_id,file_revision,source_version,units_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                id,
                row.ledger_id,
                row.root_id,
                row.path_key,
                row.path,
                row.media_id,
                row.revision,
                row.source_version,
                serde_json::to_string(&row.coverage)?
            ],
        )?;
    }
    let complete = rows.len() < 100;
    tx.execute(
        "UPDATE library_removals SET snapshot_cursor=?2,snapshot_done=?3,total_files=total_files+?4
         WHERE operation_id=?1",
        params![
            id,
            rows.last().map(|r| r.ledger_id.as_str()).unwrap_or(&cursor),
            complete,
            rows.len() as i64
        ],
    )?;
    tracing::debug!(
        operation_id = id,
        files = rows.len(),
        complete,
        "Library removal snapshot window recorded"
    );
    Ok(complete)
}

fn units(tx: &Connection, id: &str) -> Result<Vec<(i32, i32)>, StoreError> {
    let mut statement = tx.prepare(
        "SELECT season,episode FROM library_file_units WHERE file_id=?1 ORDER BY season,episode",
    )?;
    Ok(statement
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?)
}

pub(super) fn next_batch(tx: &Connection, id: &str) -> Result<Vec<RemovalRow>, StoreError> {
    let mut statement = tx.prepare(
        "SELECT file_id,root_id,path_key,display_path,media_id,file_revision,source_version,units_json
         FROM library_removal_files WHERE operation_id=?1 ORDER BY file_id LIMIT 100",
    )?;
    let rows = statement
        .query_map([id], |r| {
            Ok((
                RemovalRow {
                    ledger_id: r.get(0)?,
                    root_id: r.get(1)?,
                    path_key: r.get(2)?,
                    path: r.get(3)?,
                    media_id: r.get(4)?,
                    revision: r.get(5)?,
                    source_version: r.get(6)?,
                    coverage: Vec::new(),
                },
                r.get::<_, String>(7)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(mut row, json)| {
            row.coverage = serde_json::from_str(&json)?;
            Ok(row)
        })
        .collect()
}

pub(super) fn validate(
    tx: &Connection,
    library: &str,
    rows: &[RemovalRow],
) -> Result<(), StoreError> {
    for row in rows {
        let current = tx.query_row(
            "SELECT library_id,root_id,path_key,display_path,media_id,file_revision,source_version
             FROM library_files WHERE file_id=?1", [&row.ledger_id],
            |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,
                r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,i64>(5)?,r.get::<_,String>(6)?)),
        ).optional()?;
        let Some(current) = current else {
            continue;
        };
        let matches = current
            == (
                library.into(),
                row.root_id.clone(),
                row.path_key.clone(),
                row.path.clone(),
                row.media_id.clone(),
                row.revision,
                row.source_version.clone(),
            );
        if !matches || units(tx, &row.ledger_id)? != row.coverage {
            tracing::error!(library_id=library,file_id=%row.ledger_id,"删除快照台账身份或归属已改变");
            return Err(StoreError::Protected("删除快照台账身份或归属已改变".into()));
        }
    }
    Ok(())
}
