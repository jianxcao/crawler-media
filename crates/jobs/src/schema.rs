//! The queue owns its identity, complete schema, and read-only layout gate.
use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior};

use crate::JobError;

pub const JOBS_APPLICATION_ID: u32 = 1129139252;
pub const JOBS_USER_VERSION: i64 = 1;
const DDL: &str = include_str!("schema.sql");
const REPAIR_HINT: &str = "jobs.db contains durable Job definitions and states; back up data/jobs.db before repairing it. Do not delete or recreate it automatically";

fn invalid(message: impl std::fmt::Display) -> JobError {
    JobError::Layout(format!("{message}. {REPAIR_HINT}"))
}

fn identity(conn: &Connection) -> Result<(u32, i64), JobError> {
    Ok((
        conn.pragma_query_value(None, "application_id", |r| r.get(0))?,
        conn.pragma_query_value(None, "user_version", |r| r.get(0))?,
    ))
}

fn is_empty(conn: &Connection) -> Result<bool, JobError> {
    if identity(conn)? != (0, 0) {
        return Ok(false);
    }
    Ok(conn.query_row(
        "SELECT count(*)=0 FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
        [],
        |r| r.get(0),
    )?)
}

/// Validate an existing queue without changing headers, journal policy or objects.
pub fn validate_layout(conn: &Connection) -> Result<(), JobError> {
    let found = identity(conn)?;
    if found != (JOBS_APPLICATION_ID, JOBS_USER_VERSION) {
        return Err(invalid(format!(
            "jobs layout mismatch: expected ({JOBS_APPLICATION_ID}, {JOBS_USER_VERSION}), found {found:?}"
        )));
    }
    // Compile the one production DDL into an isolated schema; object definitions
    // are checked without maintaining another hand-written schema manifest.
    let expected = Connection::open_in_memory()?;
    expected.execute_batch(DDL)?;
    let mut stmt = expected.prepare(
        "SELECT type,name,sql FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name",
    )?;
    let objects = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    for object in objects {
        let (kind, name, sql) = object?;
        let actual: Option<String> = conn
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE type=?1 AND name=?2",
                [&kind, &name],
                |r| r.get(0),
            )
            .optional()?;
        let Some(actual) = actual else {
            return Err(invalid(format!("missing required {kind} {name}")));
        };
        if actual.split_whitespace().collect::<Vec<_>>()
            != sql.split_whitespace().collect::<Vec<_>>()
        {
            return Err(invalid(format!("definition mismatch for {kind} {name}")));
        }
    }
    let object_count = |db: &Connection| -> Result<i64, rusqlite::Error> {
        db.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
            [],
            |r| r.get(0),
        )
    };
    if object_count(conn)? != object_count(&expected)? {
        return Err(invalid("jobs layout has unsupported extra schema objects"));
    }
    let singleton: bool = conn.query_row(
        "SELECT count(*)=1 FROM job_change_state WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    if !singleton {
        return Err(invalid("missing job_change_state singleton"));
    }
    Ok(())
}

/// Missing and genuinely empty files may initialize; populated files must be current.
pub fn preflight(path: &Path) -> Result<(), JobError> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() == 0 => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        _ => {}
    }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    if is_empty(&conn)? {
        return Ok(());
    }
    validate_layout(&conn)
}

pub(crate) fn initialize_or_validate(conn: &Connection) -> Result<(), JobError> {
    // Recheck under the write transaction: two first openers must not race DDL.
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    if is_empty(&tx)? {
        tx.execute_batch(DDL)?;
        tx.pragma_update(None, "application_id", JOBS_APPLICATION_ID)?;
        tx.pragma_update(None, "user_version", JOBS_USER_VERSION)?;
    }
    validate_layout(&tx)?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};

    #[test]
    fn initialization_failure_rolls_back_objects_singleton_and_both_headers() {
        for stage in 0..6 {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("jobs.db");
            let conn = Connection::open(&path).unwrap();
            conn.authorizer(Some(move |context: AuthContext<'_>| {
                let reject = match context.action {
                    AuthAction::CreateTable { table_name } => stage == 0 && table_name == "jobs",
                    AuthAction::CreateIndex { .. } => stage == 1,
                    AuthAction::CreateTrigger { .. } => stage == 2,
                    AuthAction::Insert { table_name } => {
                        stage == 5 && table_name == "job_change_state"
                    }
                    AuthAction::Pragma {
                        pragma_name,
                        pragma_value: Some(_),
                    } => {
                        (stage == 3 && pragma_name == "application_id")
                            || (stage == 4 && pragma_name == "user_version")
                    }
                    _ => false,
                };
                if reject {
                    Authorization::Deny
                } else {
                    Authorization::Allow
                }
            }));
            assert!(initialize_or_validate(&conn).is_err());
            drop(conn);
            let conn = Connection::open(&path).unwrap();
            assert!(
                is_empty(&conn).unwrap(),
                "failure at stage {stage} left partial state"
            );
            drop(conn);
            let queue = crate::Queue::open(&path).unwrap();
            assert!(queue.list_defs().unwrap().is_empty());
        }
    }
}
