//! Owned write transaction timing includes BEGIN through commit/rollback/drop.
//! Helpers borrowing an existing transaction do not double-count it.
use crate::writers::OwnedWriteTimer;
use rusqlite::{Connection, Transaction, TransactionBehavior};

pub(crate) struct OwnedTransaction<'a> {
    // Fields drop in declaration order: rollback precedes the timer's final read.
    tx: Transaction<'a>,
    _timer: Option<OwnedWriteTimer>,
}
pub(crate) enum SqlTransaction<'a> {
    Owned(OwnedTransaction<'a>),
    Borrowed(&'a Connection),
}
impl std::ops::Deref for SqlTransaction<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        match self {
            Self::Owned(owned) => &owned.tx,
            Self::Borrowed(conn) => conn,
        }
    }
}
impl SqlTransaction<'_> {
    pub(crate) fn rollback(self) -> rusqlite::Result<()> {
        match self {
            Self::Owned(OwnedTransaction { tx, _timer }) => tx.rollback(),
            Self::Borrowed(_) => Err(rusqlite::Error::InvalidQuery),
        }
    }
    pub(crate) fn commit(self) -> rusqlite::Result<()> {
        match self {
            Self::Owned(OwnedTransaction { tx, _timer }) => tx.commit(),
            Self::Borrowed(_) => Ok(()),
        }
    }
}
pub(crate) fn write_tx(conn: &Connection) -> rusqlite::Result<SqlTransaction<'_>> {
    if conn.is_autocommit() {
        let started = std::time::Instant::now();
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        let timer = OwnedWriteTimer::start(started);
        Ok(SqlTransaction::Owned(OwnedTransaction {
            tx,
            _timer: Some(timer),
        }))
    } else {
        Ok(SqlTransaction::Borrowed(conn))
    }
}
pub(crate) fn read_tx(conn: &Connection) -> rusqlite::Result<SqlTransaction<'_>> {
    if conn.is_autocommit() {
        conn.unchecked_transaction()
            .map(|tx| SqlTransaction::Owned(OwnedTransaction { tx, _timer: None }))
    } else {
        Ok(SqlTransaction::Borrowed(conn))
    }
}
