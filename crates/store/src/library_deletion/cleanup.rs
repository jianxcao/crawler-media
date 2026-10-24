use rusqlite::params;

use super::{LibraryRemoval, Store, StoreError, clean_unit_facts, snapshot};

pub(super) fn advance(store: &Store, id: &str) -> Result<LibraryRemoval, StoreError> {
    let operation = store
        .library_removal(id)?
        .ok_or_else(|| StoreError::Missing(id.into()))?;
    if operation.state == "succeeded" {
        return Ok(operation);
    }
    if operation.state != "cleaning" {
        return Err(StoreError::Protected("Library 工作尚未排空".into()));
    }
    let tx = crate::write_tx(&store.library)?;
    validate_revision(&tx, id, &operation.library_id)?;
    let ready: bool = tx.query_row(
        "SELECT snapshot_done FROM library_removals WHERE operation_id=?1",
        [id],
        |r| r.get(0),
    )?;
    if !ready {
        return Err(StoreError::Protected("Library 删除快照尚未完成".into()));
    }
    let rows = snapshot::next_batch(&tx, id)?;
    snapshot::validate(&tx, &operation.library_id, &rows)?;
    if !rows.is_empty() {
        let files = rows
            .iter()
            .map(|r| r.ledger_id.parse())
            .collect::<Result<Vec<domain::LedgerId>, _>>()?;
        crate::library_write::delete::delete_files_in_tx(
            &tx,
            operation.library_id.parse()?,
            &files,
        )?;
        clean_unit_facts(&tx, rows.iter())?;
        for row in &rows {
            tx.execute(
                "DELETE FROM library_removal_files WHERE operation_id=?1 AND file_id=?2",
                params![id, row.ledger_id],
            )?;
        }
        tx.commit()?;
        tracing::debug!(
            operation_id = id,
            files = rows.len(),
            "Library removal cleanup window committed"
        );
        #[cfg(test)]
        super::removal_batch_failpoint()?;
        return store
            .library_removal(id)?
            .ok_or_else(|| StoreError::Missing(id.into()));
    }
    let unidentified = tx.execute(
        "DELETE FROM unidentified WHERE rowid IN
         (SELECT rowid FROM unidentified WHERE library_id=?1 LIMIT 100)",
        [&operation.library_id],
    )?;
    if unidentified > 0 {
        tx.commit()?;
        tracing::debug!(
            operation_id = id,
            unidentified,
            "Library removal Unidentified window committed"
        );
        return store
            .library_removal(id)?
            .ok_or_else(|| StoreError::Missing(id.into()));
    }
    #[cfg(test)]
    super::removal_cleanup_failpoint()?;
    // The app transaction must finish before this durable intent can succeed.
    // Release library.db first: two databases never hold write locks together.
    tx.commit()?;
    store.app_write(|store| {
        for prefix in super::LIBRARY_SETTING_KEY_PREFIXES {
            store.delete_setting(&format!("{prefix}{}", operation.library_id))?;
        }
        Ok(())
    })?;
    let tx = crate::write_tx(&store.library)?;
    validate_revision(&tx, id, &operation.library_id)?;
    finalize(&tx, id, &operation.library_id)?;
    tx.commit()?;
    crate::path_key::reload_root_aliases(&store.library)?;
    tracing::info!(operation_id=id,library_id=%operation.library_id,"Library 索引删除完成，磁盘文件保留");
    store
        .library_removal(id)?
        .ok_or_else(|| StoreError::Missing(id.into()))
}

fn validate_revision(tx: &rusqlite::Connection, id: &str, library: &str) -> Result<(), StoreError> {
    let (original, current): (i64, i64) = tx.query_row(
        "SELECT r.config_revision,l.config_revision FROM library_removals r
         JOIN libraries l ON l.library_id=r.library_id WHERE r.operation_id=?1 AND r.library_id=?2",
        params![id, library],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if original != current {
        return Err(StoreError::Protected("删除期间 Library 配置已改变".into()));
    }
    Ok(())
}

fn finalize(tx: &rusqlite::Connection, id: &str, library: &str) -> Result<(), StoreError> {
    // A guarded writer cannot add files after freeze. Refuse bypassed writes rather
    // than deleting an unobserved file or silently losing its dependent facts.
    let remaining: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_files WHERE library_id=?1)",
        [library],
        |r| r.get(0),
    )?;
    if remaining {
        return Err(StoreError::Protected(
            "Library 删除期间出现未捕获文件".into(),
        ));
    }
    tx.execute("DELETE FROM library_roots WHERE library_id=?1", [library])?;
    tx.execute("DELETE FROM libraries WHERE library_id=?1", [library])?;
    tx.execute(
        "UPDATE libraries SET is_default=1 WHERE library_id IN (
         SELECT l.library_id FROM libraries l WHERE NOT EXISTS
         (SELECT 1 FROM libraries d WHERE d.kind=l.kind AND d.is_default=1)
         AND NOT EXISTS (SELECT 1 FROM library_removals r WHERE r.library_id=l.library_id AND r.state<>'succeeded')
         AND l.library_id=(SELECT c.library_id FROM libraries c WHERE c.kind=l.kind
         AND NOT EXISTS (SELECT 1 FROM library_removals r WHERE r.library_id=c.library_id AND r.state<>'succeeded')
         ORDER BY c.sort_order,c.library_id LIMIT 1))",[],
    )?;
    tx.execute("UPDATE library_removals SET state='succeeded',error=NULL,removed_ledger_count=total_files,updated_at_ms=?2 WHERE operation_id=?1",params![id,crate::unix_now()*1000])?;
    Ok(())
}
