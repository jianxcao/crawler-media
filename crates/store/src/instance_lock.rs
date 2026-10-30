//! Single-instance lease on the data directory. App writes use the shared
//! writer handle, so a Library scan cannot delay lease refresh or shutdown.
//! Lease times are stored in Unix milliseconds.

use super::{Store, StoreError, Writers};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::sync::Arc;

/// Shared App writer handle for instance lease refresh and clean shutdown.
pub struct AppHeartbeat {
    writers: Arc<Writers>,
}

impl AppHeartbeat {
    pub fn heartbeat(&self, owner: &str, now_ms: i64) -> Result<bool, StoreError> {
        self.writers
            .with_app(|app| app.heartbeat_instance_lock_ms(owner, now_ms))
    }

    pub fn release(&self, owner: &str) -> Result<(), StoreError> {
        self.writers
            .with_app(|app| app.release_instance_lock(owner))
    }
}

impl crate::App<'_> {
    pub fn heartbeat_instance_lock_ms(&self, owner: &str, now_ms: i64) -> Result<bool, StoreError> {
        heartbeat_on(self.app, owner, now_ms)
    }

    pub fn release_instance_lock(&self, owner: &str) -> Result<(), StoreError> {
        self.app
            .execute("DELETE FROM instance_lock WHERE id=1 AND owner=?1", [owner])?;
        Ok(())
    }
}

fn heartbeat_on(conn: &Connection, owner: &str, now_ms: i64) -> Result<bool, StoreError> {
    let changed = conn.execute(
        "UPDATE instance_lock SET heartbeat_at = ?2 WHERE id = 1 AND owner = ?1",
        params![owner, now_ms],
    )?;
    Ok(changed == 1)
}

impl Store {
    /// Clone the App writer handle used for lease refresh.
    ///
    /// No connection or outer Store guard is needed after cloning this handle.
    pub fn open_app_heartbeat(&self) -> Result<AppHeartbeat, StoreError> {
        Ok(AppHeartbeat {
            writers: self.writers(),
        })
    }

    /// Take the lease for `owner` at `now` (Unix seconds) with a
    /// `ttl_secs`-long TTL. Returns `Some(other_owner)` when another instance
    /// holds a live lease.
    pub fn try_acquire_instance_lock(
        &self,
        owner: &str,
        now: i64,
        ttl_secs: i64,
    ) -> Result<Option<String>, StoreError> {
        self.try_acquire_instance_lock_ms(owner, now.saturating_mul(1000), ttl_secs * 1000)
    }

    /// [`Self::try_acquire_instance_lock`] at millisecond resolution, so a test
    /// can exercise the takeover window without waiting a whole TTL.
    pub fn try_acquire_instance_lock_ms(
        &self,
        owner: &str,
        now_ms: i64,
        ttl_ms: i64,
    ) -> Result<Option<String>, StoreError> {
        self.with_app(|app| {
        let tx = rusqlite::Transaction::new_unchecked(app.app, TransactionBehavior::Immediate)?;
        let current: Option<(String, i64)> = tx
            .query_row(
                "SELECT owner, heartbeat_at FROM instance_lock WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((other, beat)) = current
            && other != owner
            && now_ms - beat < ttl_ms
        {
            return Ok(Some(other));
        }
        tx.execute(
            "INSERT INTO instance_lock (id, owner, heartbeat_at) VALUES (1, ?1, ?2)
             ON CONFLICT(id) DO UPDATE SET owner = excluded.owner, heartbeat_at = excluded.heartbeat_at",
            params![owner, now_ms],
        )?;
        tx.commit()?;
        Ok(None)
        })
    }

    /// Refresh the lease at whole-second resolution. `false` means another
    /// instance took it over.
    pub fn heartbeat_instance_lock(&self, owner: &str, now: i64) -> Result<bool, StoreError> {
        self.with_app(|app| heartbeat_on(app.app, owner, now.saturating_mul(1000)))
    }

    /// Drop the lease if `owner` still holds it (clean shutdown).
    pub fn release_instance_lock(&self, owner: &str) -> Result<(), StoreError> {
        self.with_app(|app| {
            app.app.execute(
                "DELETE FROM instance_lock WHERE id = 1 AND owner = ?1",
                [owner],
            )?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Store;

    #[test]
    fn heartbeat_only_refreshes_the_owner_lease() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let heartbeat = store.open_app_heartbeat().unwrap();
        let now = 1_000_000;
        assert_eq!(
            store
                .try_acquire_instance_lock_ms("owner", now, 60_000)
                .unwrap(),
            None
        );
        assert!(heartbeat.heartbeat("owner", now + 1_000).unwrap());
        assert!(
            !heartbeat.heartbeat("intruder", now + 2_000).unwrap(),
            "a non-owner must not refresh the lease"
        );
    }

    #[test]
    fn heartbeat_error_surfaces_instead_of_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let heartbeat = store.open_app_heartbeat().unwrap();
        store
            .try_acquire_instance_lock_ms("owner", 1_000, 60_000)
            .unwrap();
        store
            .writers()
            .with_writer(crate::Db::App, |conn| {
                conn.execute("DROP TABLE instance_lock", [])?;
                Ok(())
            })
            .unwrap();
        assert!(heartbeat.heartbeat("owner", 2_000).is_err());
    }
}
