//! Media-scoped facts a file move cannot carry (timeline markers, fingerprint season
//! models, season comparison history) and collection membership.

use domain::MediaId;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use crate::StoreError;

/// Media-scoped watch facts that no file move can carry: timeline markers, the
/// fingerprint season models built from the films' samples, and the season
/// comparison history. They follow the identity, so the retired loser keeps none
/// of them; for a key the survivor already holds, the survivor's own row wins.
pub(in crate::media_operations) fn merge_watch_facts(
    tx: &Connection,
    survivor: MediaId,
    loser: MediaId,
) -> Result<(), StoreError> {
    move_scoped_rows(tx, "media_markers", &["season", "episode"], survivor, loser)?;
    move_scoped_rows(
        tx,
        "fingerprint_season_models",
        &["season", "kind"],
        survivor,
        loser,
    )?;
    move_scoped_rows(
        tx,
        "probe_season_comparison_history",
        &["season"],
        survivor,
        loser,
    )?;
    Ok(())
}

/// Re-point the loser's rows of one media-scoped table onto the survivor. `key`
/// names the table's remaining key columns: for a key the survivor already holds
/// its own row wins and the loser's duplicate is dropped, so the loser is left
/// with no row at all.
fn move_scoped_rows(
    tx: &Connection,
    table: &str,
    key: &[&str],
    survivor: MediaId,
    loser: MediaId,
) -> Result<(), StoreError> {
    let rows: Vec<Vec<SqlValue>> = {
        let sql = format!("SELECT {} FROM {table} WHERE media_id = ?1", key.join(", "));
        let mut stmt = tx.prepare(&sql)?;
        
        stmt
            .query_map(params![loser.to_string()], |r| {
                (0..key.len()).map(|i| r.get::<_, SqlValue>(i)).collect()
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let predicate = |offset: usize| {
        key.iter()
            .enumerate()
            .map(|(i, column)| format!("{column} IS ?{}", i + offset))
            .collect::<Vec<_>>()
            .join(" AND ")
    };
    for row in rows {
        let mut owned = vec![SqlValue::Text(survivor.to_string())];
        owned.extend(row.iter().cloned());
        let survivor_has_it: Option<i64> = tx
            .query_row(
                &format!(
                    "SELECT 1 FROM {table} WHERE media_id = ?1 AND {}",
                    predicate(2)
                ),
                params_from_iter(owned),
                |r| r.get(0),
            )
            .optional()?;

        let mut gone = vec![
            SqlValue::Text(survivor.to_string()),
            SqlValue::Text(loser.to_string()),
        ];
        gone.extend(row.iter().cloned());
        if survivor_has_it.is_some() {
            tx.execute(
                &format!(
                    "DELETE FROM {table} WHERE media_id = ?2 AND {}",
                    predicate(3)
                ),
                params_from_iter(gone),
            )?;
            continue;
        }
        tx.execute(
            &format!(
                "UPDATE {table} SET media_id = ?1 WHERE media_id = ?2 AND {}",
                predicate(3)
            ),
            params_from_iter(gone),
        )?;
    }
    Ok(())
}

/// Collection membership: keep one survivor row per collection at the earliest
/// sort position. Other members are untouched.
pub(in crate::media_operations) fn merge_collection_items(
    tx: &Connection,
    survivor: MediaId,
    loser: MediaId,
) -> Result<(), StoreError> {
    let rows: Vec<(String, i64, String)> = {
        let mut stmt = tx.prepare(
            "SELECT collection_id, sort_order, added_at FROM collection_items
             WHERE media_id = ?1",
        )?;
        stmt.query_map(params![loser.to_string()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?
    };
    for (collection_id, sort_order, added_at) in rows {
        let existing: Option<(i64, String)> = tx
            .query_row(
                "SELECT sort_order, added_at FROM collection_items
                 WHERE collection_id = ?1 AND media_id = ?2",
                params![collection_id, survivor.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match existing {
            None => {
                tx.execute(
                    "UPDATE collection_items SET media_id = ?1
                     WHERE collection_id = ?2 AND media_id = ?3",
                    params![survivor.to_string(), collection_id, loser.to_string()],
                )?;
            }
            Some((survivor_sort, survivor_added)) => {
                let sort_order = survivor_sort.min(sort_order);
                let added_at = if survivor_added <= added_at {
                    survivor_added
                } else {
                    added_at
                };
                tx.execute(
                    "UPDATE collection_items SET sort_order = ?1, added_at = ?2
                     WHERE collection_id = ?3 AND media_id = ?4",
                    params![sort_order, added_at, collection_id, survivor.to_string()],
                )?;
                tx.execute(
                    "DELETE FROM collection_items WHERE collection_id = ?1 AND media_id = ?2",
                    params![collection_id, loser.to_string()],
                )?;
            }
        }
    }
    Ok(())
}

