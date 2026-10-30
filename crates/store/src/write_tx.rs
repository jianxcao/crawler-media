//! Transaction ownership follows an explicit borrowed connection. An outer SQL
//! scope owns BEGIN/COMMIT; helpers on that same connection only borrow its transaction.
use rusqlite::{Connection, Transaction, TransactionBehavior};

pub(crate) enum SqlTransaction<'a> {
    Owned(Transaction<'a>),
    Borrowed(&'a Connection),
}
impl std::ops::Deref for SqlTransaction<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        match self {
            Self::Owned(tx) => tx,
            Self::Borrowed(conn) => conn,
        }
    }
}
impl SqlTransaction<'_> {
    pub(crate) fn rollback(self) -> rusqlite::Result<()> {
        match self {
            Self::Owned(tx) => tx.rollback(),
            Self::Borrowed(_) => Err(rusqlite::Error::InvalidQuery),
        }
    }
    pub(crate) fn commit(self) -> rusqlite::Result<()> {
        match self {
            Self::Owned(tx) => tx.commit(),
            Self::Borrowed(_) => Ok(()),
        }
    }
}
pub(crate) fn write_tx(conn: &Connection) -> rusqlite::Result<SqlTransaction<'_>> {
    if conn.is_autocommit() {
        Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map(SqlTransaction::Owned)
    } else {
        Ok(SqlTransaction::Borrowed(conn))
    }
}
pub(crate) fn read_tx(conn: &Connection) -> rusqlite::Result<SqlTransaction<'_>> {
    if conn.is_autocommit() {
        conn.unchecked_transaction().map(SqlTransaction::Owned)
    } else {
        Ok(SqlTransaction::Borrowed(conn))
    }
}
