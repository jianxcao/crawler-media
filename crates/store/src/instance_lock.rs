//! G-M5: single-instance lease on the data directory. One row; the owner
//! refreshes `heartbeat_at`, and a lease older than the TTL may be taken over.
//!
//! Review L1: the heartbeat runs on its own app.db connection
//! ([`AppHeartbeat`]) instead of the process-wide `Store` lock. A batch
//! (integrity / scan) may hold that lock for minutes; when the heartbeat shares
//! it, the lease goes stale, its TTL expires and a second instance takes the
//! data directory over while the first one is still writing.
//!
//! `heartbeat_at` is Unix **milliseconds**. Whole-second callers keep working
//! through the `_secs`-shaped signatures, which convert; millisecond resolution
//! exists so a test can shrink the TTL below one second.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::{Store, StoreError};

/// Dedicated app.db connection that only refreshes the lease.
///
/// It exists so the lease stays fresh while another thread holds the
/// process-wide `Store` lock. Deliberately exposes only [`Self::heartbeat`]:
/// acquiring and releasing the lease belong to the process that owns it.
pub struct AppHeartbeat {
    conn: Mutex<Connection>,
}

impl AppHeartbeat {
    fn open(app_path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(app_path)?;
        // 连接策略与其它库同一处：`DbKind::Heartbeat` 只把 `busy_timeout` 拉长到
        // 心跳间隔的一半，耐久性（FULL）与其它权威库一致。
        super::open::apply_connection_policy(&conn, super::open::DbKind::Heartbeat)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Refresh the lease held by `owner` at `now_ms` (Unix milliseconds).
    /// `false` means another instance took the lease over.
    pub fn heartbeat(&self, owner: &str, now_ms: i64) -> Result<bool, StoreError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| StoreError::Invalid("instance heartbeat connection poisoned".into()))?;
        heartbeat_on(&conn, owner, now_ms)
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
    /// Open the dedicated lease-heartbeat connection for this store's app.db.
    ///
    /// `open_at` allows a non-default app.db path, so the heartbeat takes the
    /// path from the store rather than assuming `data_dir/app.db`.
    pub fn open_app_heartbeat(&self) -> Result<AppHeartbeat, StoreError> {
        AppHeartbeat::open(&self.app_path)
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
        let tx = rusqlite::Transaction::new_unchecked(&self.app, TransactionBehavior::Immediate)?;
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
    }

    /// Refresh the lease at whole-second resolution. `false` means another
    /// instance took it over.
    pub fn heartbeat_instance_lock(&self, owner: &str, now: i64) -> Result<bool, StoreError> {
        heartbeat_on(&self.app, owner, now.saturating_mul(1000))
    }

    /// Drop the lease if `owner` still holds it (clean shutdown).
    pub fn release_instance_lock(&self, owner: &str) -> Result<(), StoreError> {
        self.app.execute(
            "DELETE FROM instance_lock WHERE id = 1 AND owner = ?1",
            [owner],
        )?;
        Ok(())
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
        store.app.execute("DROP TABLE instance_lock", []).unwrap();
        assert!(heartbeat.heartbeat("owner", 2_000).is_err());
    }
}
