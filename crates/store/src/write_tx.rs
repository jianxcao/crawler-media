//! C03: every read-then-write transaction takes the SQLite write lock at BEGIN.
//!
//! A DEFERRED transaction that reads first and upgrades later fails with
//! BUSY_SNAPSHOT when another connection commits in between; IMMEDIATE waits
//! under `busy_timeout` instead and then reads the committed state.

use rusqlite::{Connection, Transaction, TransactionBehavior};

pub(crate) fn write_tx(conn: &Connection) -> rusqlite::Result<Transaction<'_>> {
    Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
}
