//! G-M5: only one API instance may drive a data directory. The lease lives in
//! the app DB, is refreshed by a heartbeat thread and released on drop; a lease
//! whose heartbeat is older than the TTL can be taken over after a crash.
//!
//! Review L-7: the lease compares wall-clock times (`unix_now`) written by
//! different processes. A large NTP step forward can make a live holder look
//! stale (takeover; the old holder notices on its next heartbeat and turns
//! read-only), and a step backward can delay takeover of a crashed holder.
//! Keep the hosts' clocks synced; the TTL (60 s) is the tolerance window.
//!
//! The heartbeat refreshes the lease through the shared app writer
//! ([`Store::open_app_heartbeat`]) and never takes `state.store.as_ref()`. A batch
//! holding that lock for longer than the TTL must not let a second instance take
//! the directory over. Two consecutive heartbeat errors degrade this instance to
//! read-only instead of letting it write on an unverified lease.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use crate::store::{AppHeartbeat, Store};

/// Lease TTL: a crashed instance's lock is taken over after this many seconds.
pub const INSTANCE_LEASE_TTL_SECS: i64 = 60;
const HEARTBEAT_EVERY: Duration = Duration::from_secs(20);
/// Consecutive heartbeat errors before the instance stops writing.
const HEARTBEAT_ERROR_LIMIT: u32 = 2;
/// Escape hatch for deliberate multi-instance setups (env var, "1"/"true").
pub const ALLOW_MULTI_INSTANCE_ENV: &str = "CRAWLER_MEDIA_ALLOW_MULTI_INSTANCE";
/// Retry cadence override for a read-only instance waiting on the lease.
pub const LEASE_RETRY_ENV: &str = "CRAWLER_MEDIA_LEASE_RETRY_MS";
/// Heartbeat cadence override for the lease holder.
pub const LEASE_HEARTBEAT_ENV: &str = "CRAWLER_MEDIA_LEASE_HEARTBEAT_MS";
/// Lease TTL override (milliseconds) so a test can shorten the takeover window.
pub const LEASE_TTL_ENV: &str = "CRAWLER_MEDIA_LEASE_TTL_MS";

pub struct InstanceLease {
    /// Shared app writer, so refreshing the lease does not
    /// queue behind a long batch holding the global `Store` lock.
    heartbeat: AppHeartbeat,
    owner: String,
    /// H-1: set by the heartbeat when another instance took the lease over.
    lost: AtomicBool,
}

impl InstanceLease {
    pub(crate) fn is_lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }

    fn mark_lost(&self) {
        self.lost.store(true, Ordering::SeqCst);
    }
}

fn env_ms(name: &str, default: Duration) -> Duration {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(default)
}

/// Production TTL, overridable (milliseconds) so tests need not wait 60 s.
fn lease_ttl_ms() -> i64 {
    std::env::var(LEASE_TTL_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .filter(|ms| *ms > 0)
        .unwrap_or(INSTANCE_LEASE_TTL_SECS * 1000)
}

fn unix_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

/// H-1: how often a read-only instance retries the lease (TTL/2).
pub(crate) fn retry_interval() -> Duration {
    env_ms(
        LEASE_RETRY_ENV,
        Duration::from_millis((lease_ttl_ms() / 2).max(1) as u64),
    )
}

impl Drop for InstanceLease {
    fn drop(&mut self) {
        if let Err(error) = self.heartbeat.release(&self.owner) {
            tracing::warn!(%error, "释放实例锁失败");
        }
    }
}

fn multi_instance_allowed(store: &Store) -> bool {
    let truthy = |v: &str| matches!(v.trim(), "1" | "true" | "yes");
    std::env::var(ALLOW_MULTI_INSTANCE_ENV).is_ok_and(|v| truthy(&v))
        || store
            .get_setting(crate::settings_keys::INSTANCE_ALLOW_MULTI)
            .ok()
            .flatten()
            .is_some_and(|v| truthy(&v))
}

/// Acquire the data-directory lease; `Err` when another live instance holds it.
pub(crate) fn acquire(store: &Arc<Store>) -> std::io::Result<Option<Arc<InstanceLease>>> {
    let owner = format!(
        "pid{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );
    let heartbeat = {
        let guard = store.as_ref();
        if multi_instance_allowed(&guard) {
            tracing::warn!("已允许多实例共用数据目录：订阅选种/入库可能重复执行");
            return Ok(None);
        }
        let now = unix_now_ms();
        match guard.try_acquire_instance_lock_ms(&owner, now, lease_ttl_ms()) {
            Ok(None) => {}
            Ok(Some(other)) => {
                return Err(std::io::Error::other(format!(
                    "数据目录已被另一个实例 ({other}) 占用；仅支持单实例运行（设置 {ALLOW_MULTI_INSTANCE_ENV}=1 可跳过）"
                )));
            }
            Err(error) => return Err(std::io::Error::other(error.to_string())),
        }
        guard
            .open_app_heartbeat()
            .map_err(|error| std::io::Error::other(error.to_string()))?
    };
    tracing::info!(%owner, ttl_ms = lease_ttl_ms(), "已获取数据目录实例锁（仅支持单实例运行）");
    let lease = Arc::new(InstanceLease {
        heartbeat,
        owner,
        lost: AtomicBool::new(false),
    });
    spawn_heartbeat(Arc::downgrade(&lease));
    Ok(Some(lease))
}

fn spawn_heartbeat(lease: Weak<InstanceLease>) {
    let spawned = std::thread::Builder::new()
        .name("instance-lease".into())
        .spawn(move || {
            let mut consecutive_errors = 0u32;
            loop {
                std::thread::sleep(env_ms(LEASE_HEARTBEAT_ENV, HEARTBEAT_EVERY));
                let Some(lease) = lease.upgrade() else { return };
                match lease.heartbeat.heartbeat(&lease.owner, unix_now_ms()) {
                    Ok(true) => consecutive_errors = 0,
                    Ok(false) => {
                        tracing::error!(owner = %lease.owner, "实例锁已被其他实例接管：停止后台任务并降级为只读");
                        lease.mark_lost();
                        return;
                    }
                    Err(error) => {
                        consecutive_errors += 1;
                        if consecutive_errors >= HEARTBEAT_ERROR_LIMIT {
                            tracing::error!(
                                %error,
                                owner = %lease.owner,
                                consecutive_errors,
                                "实例锁心跳连续失败：无法确认租约，降级为只读"
                            );
                            lease.mark_lost();
                            return;
                        }
                        tracing::warn!(
                            %error,
                            owner = %lease.owner,
                            consecutive_errors,
                            "实例锁心跳失败，下次心跳重试"
                        );
                    }
                }
            }
        });
    if let Err(error) = spawned {
        tracing::error!(%error, "无法启动实例锁心跳线程");
    }
}
