//! Publish exactly one owner-fenced prepared page.
use super::*;
use rusqlite::params_from_iter;

pub(crate) fn commit(
    conn: &Connection,
    operation: &str,
    now: i64,
    mut prepared: Prepared,
) -> Result<OperationStatus, StoreError> {
    let tx = crate::write_tx(conn)?;
    let current: Option<String> = tx.query_row(
        "SELECT cursor_json FROM library_operations WHERE operation_id=?1",
        [operation],
        |r| r.get(0),
    )?;
    if current != prepared.before {
        tx.commit()?;
        return Store::read_library_operation(conn, operation);
    }
    let result = (|| {
        let owner = IntegrityOwner::acquire(&tx, operation, prepared.library.parse()?)?;
        apply(&tx, &owner, operation, &mut prepared)?;
        let final_state = if prepared.finish {
            let (damage, more) = crate::query_projection::search_integrity::finish(
                &tx,
                operation,
                prepared.plan.repair,
                100,
            )?;
            if damage {
                prepared.state.report.query_count_mismatch = true;
                prepared.state.issues_found += 1;
                if prepared.plan.repair {
                    prepared.state.issues_repaired += 1;
                }
                prepared.state.report.totals.query_projection_mismatches += 1;
            }
            if more {
                "applying"
            } else {
                finish_integrity_batch(
                    &tx,
                    &owner,
                    &prepared.library,
                    prepared.library.parse()?,
                    &prepared.plan,
                    operation,
                    &mut prepared.state,
                )?
            }
        } else {
            "applying"
        };
        write_integrity_cursor(&tx, operation, &prepared.state, final_state, now)?;
        Ok(final_state)
    })();
    match result {
        Ok(final_state) => {
            tx.commit()?;
            Ok(OperationStatus {
                operation_id: operation.into(),
                phase: final_state.into(),
                state: final_state.into(),
                error: None,
            })
        }
        Err(error) => {
            tx.rollback()?;
            record_failure(conn, operation, now, &error)?;
            Err(error)
        }
    }
}

fn apply(
    tx: &Connection,
    owner: &IntegrityOwner,
    operation: &str,
    prepared: &mut Prepared,
) -> Result<(), StoreError> {
    let Some(page) = prepared.state.current.as_mut() else {
        crate::query_projection::search_integrity::prepare(tx, operation, &prepared.library)?;
        return Ok(());
    };
    let media: MediaId = page.media.parse()?;
    for action in &prepared.actions {
        match action {
            Mutation::Stage(surface, row) => rows::stage(tx, operation, &page.media, surface, row)?,
            Mutation::Upsert(table, row) => {
                crate::query_projection::repair::write_row(tx, table, row)?
            }
            Mutation::Delete(table, surface, key) => {
                let (predicate, values) =
                    rows::predicate(&prepared.library, &page.media, surface, key);
                tx.execute(
                    &format!("DELETE FROM {table} WHERE {predicate}"),
                    params_from_iter(values),
                )?;
            }
            Mutation::MissingItem => sample(
                &mut prepared.state.report.missing_items,
                &mut prepared.state.report.totals.missing_items,
                &mut prepared.state.report.truncated,
                media,
            ),
            Mutation::OrphanItem => sample(
                &mut prepared.state.report.orphan_items,
                &mut prepared.state.report.totals.orphan_items,
                &mut prepared.state.report.truncated,
                media,
            ),
            Mutation::Resolution(value) => {
                tx.execute("INSERT INTO library_query_count_rebuild(operation_id,surface,dimension,value,member_count) VALUES(?1,'browse','resolution',?2,1) ON CONFLICT DO UPDATE SET member_count=member_count+1",params![operation,value])?;
            }
            Mutation::MissingUser(user) => {
                if let Ok(user) = user.parse() {
                    sample(
                        &mut prepared.state.report.missing_user_items,
                        &mut prepared.state.report.totals.missing_user_items,
                        &mut prepared.state.report.truncated,
                        (user, media),
                    );
                }
            }
            Mutation::OrphanUser(user) => {
                if let Ok(user) = user.parse() {
                    sample(
                        &mut prepared.state.report.orphan_user_items,
                        &mut prepared.state.report.totals.orphan_user_items,
                        &mut prepared.state.report.truncated,
                        (user, media),
                    );
                }
            }
            Mutation::Header(expected) => {
                crate::query_projection::search_integrity::prepare(
                    tx,
                    operation,
                    &prepared.library,
                )?;
                let key = ProjectionKey {
                    library_id: prepared.library.parse()?,
                    media_id: media,
                };
                let mismatch = !crate::query_projection::repair::header_matches(tx, key, expected)?;
                page.mismatch |= mismatch;
                if prepared.plan.repair {
                    crate::query_projection::repair::repair_header(tx, owner, key, expected)?;
                    if !crate::query_projection::repair::header_matches(tx, key, expected)? {
                        return Err(StoreError::Protected(
                            "Integrity header verification failed".into(),
                        ));
                    }
                }
                integrity_query::stage(tx, operation, expected, key)?;
                let issues = page.presence_issues.max(usize::from(page.mismatch));
                prepared.state.issues_found += issues;
                if prepared.plan.repair {
                    prepared.state.issues_repaired += issues;
                }
                if page.mismatch {
                    sample(
                        &mut prepared.state.report.mismatched_media,
                        &mut prepared.state.report.totals.mismatched_media,
                        &mut prepared.state.report.truncated,
                        media,
                    );
                    sample(
                        &mut prepared.state.report.query_projection_media,
                        &mut prepared.state.report.totals.query_projection_mismatches,
                        &mut prepared.state.report.truncated,
                        media,
                    );
                }
            }
            Mutation::Cleanup(surface, key) => {
                tx.execute("DELETE FROM library_integrity_rows WHERE operation_id=?1 AND media_id=?2 AND surface=?3 AND user_id=?4 AND season=?5 AND episode=?6 AND key_a=?7 AND key_b=?8 AND key_c=?9",
                    params![operation,page.media,surface,key.user,key.season,key.episode,key.a,key.b,key.c])?;
            }
        }
    }
    if prepared.finished_media {
        prepared.state.last_media_id = Some(page.media.clone());
        prepared.state.current = None;
    }
    Ok(())
}

pub(crate) fn record_failure(
    conn: &Connection,
    operation: &str,
    now: i64,
    error: &StoreError,
) -> Result<(), StoreError> {
    let changed=conn.execute("UPDATE library_operations SET state='failed',error=?2,updated_at_ms=?3 WHERE operation_id=?1 AND kind='integrity' AND state<>'succeeded'",params![operation,error.to_string(),now])?;
    if changed > 0 {
        tracing::error!(operation_id=operation,%error,"Integrity page failed; owner and cursor retained");
    }
    Ok(())
}
