//! A reserved opening owns capacity until a connection takes over or is closed.
use std::sync::Arc;

use rusqlite::Connection;

use super::{LibraryReadError, ReaderLease, pool::Inner};

pub(super) struct Admission {
    pool: Arc<Inner>,
    epoch: u64,
    connection: Option<Connection>,
    reserved: bool,
}

impl Admission {
    pub(super) fn new(pool: Arc<Inner>, epoch: u64, connection: Option<Connection>) -> Self {
        Self {
            pool,
            epoch,
            connection,
            reserved: true,
        }
    }

    pub(super) fn open(mut self) -> Result<ReaderLease, LibraryReadError> {
        let conn = match self.connection.take() {
            Some(conn) => conn,
            None => {
                let opened = self.pool.open_connection();
                let valid = self.pool.check_identity();
                match (opened, valid) {
                    (Ok(conn), Ok(())) => conn,
                    (opened, valid) => {
                        // Close an invalid connection before making its slot available.
                        let error = opened.err();
                        valid?;
                        let error = error.ok_or(LibraryReadError::Closed)?;
                        tracing::error!(database = self.pool.database, %error, "reader connection opening failed");
                        return Err(error.into());
                    }
                }
            }
        };
        let lease = ReaderLease::new(conn, Arc::clone(&self.pool), self.epoch);
        self.reserved = false;
        crate::stats::record_reader_checkout(self.pool.database);
        Ok(lease)
    }

    pub(super) async fn open_async(self) -> Result<ReaderLease, LibraryReadError> {
        if self.connection.is_some() {
            return self.open();
        }
        let database = self.pool.database;
        // Dropping the awaiting task detaches the opener. Its reservation remains
        // counted, and an unclaimed lease returns itself on drop.
        tokio::task::spawn_blocking(move || self.open())
            .await
            .map_err(|error| {
                tracing::error!(database, %error, "reader connection opener failed");
                crate::StoreError::Protected(format!(
                    "{database} reader connection opener failed: {error}"
                ))
            })?
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        if self.reserved {
            match self.connection.take() {
                Some(conn) => self.pool.put_back(conn, self.epoch),
                None => self.pool.retired(),
            }
        }
    }
}
