//! Deterministic work counters for the performance gate (`test-support` seam).
//!
//! The gate can only assert what it can count, and wall clock cannot be asserted: a slow
//! box would fail a healthy tree. Every hot path therefore has to expose two deterministic
//! numbers — SQL statements executed and `FULLSCAN_STEP` rows walked.
//!
//! Two rules shape this seam:
//!
//! - The numbers come from SQLite's own trace (`QueryWorkScope`), never from counters the
//!   path increments by hand. A hand-incremented counter only proves the path agrees with
//!   itself; the trace reports what the database really prepared and ran.
//! - It is a *closure* seam: the production entry point is called **inside** the closure, so
//!   the measurement cannot drift from the statements the code actually executes.
//!
//! The shape matches the other two measured seams on this branch: `#[doc(hidden)]`,
//! compiled only under the `test-support` feature, and production builds unchanged
//! (`Store::publish_files_with_stats` for write-side work, [`PENDING_DISPATCH_SQL`] for a
//! query the production method itself prepares).
//!
//! [`PENDING_DISPATCH_SQL`]: crate::PENDING_DISPATCH_SQL

#[cfg(feature = "test-support")]
use rusqlite::Connection;

#[cfg(feature = "test-support")]
use crate::library_query::items::stats::{QueryWorkScope, QueryWorkStats};

#[cfg(feature = "test-support")]
use crate::Store;

/// Which store connection(s) one measured closure runs on.
#[cfg(feature = "test-support")]
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatsScope {
    /// `library.db`: browse, detail, facets, playback selection, publication, progress writes.
    Library,
    /// `app.db`: authorization only (the role roster lives there).
    App,
    /// Both connections, summed: a request that authorizes first and then reads.
    Both,
}

#[cfg(feature = "test-support")]
impl Store {
    /// Install a temporary SQLite progress hook around a test operation.
    #[doc(hidden)]
    pub fn with_library_progress<T>(
        &self,
        interval: i32,
        callback: impl FnMut() -> bool + Send + 'static,
        work: impl FnOnce() -> T,
    ) -> T {
        struct ProgressGuard<'a>(&'a Connection);
        impl Drop for ProgressGuard<'_> {
            fn drop(&mut self) {
                self.0.progress_handler(0, None::<fn() -> bool>);
            }
        }
        self.library.progress_handler(interval, Some(callback));
        let _guard = ProgressGuard(&self.library);
        work()
    }

    /// Runs `work` and reports the deterministic SQL work it did.
    ///
    /// Use it in a test, around the public entry point under test:
    ///
    /// ```ignore
    /// let (detail, stats) = store.with_stats(StatsScope::Library, || {
    ///     store.library_item_detail(library, media, "en-US")
    /// });
    /// assert!(stats.fullscan_steps == 0, "detail must stay on its indexes");
    /// ```
    ///
    /// `statements`, `fullscan_steps`, `vm_steps`, `sort_operations`, `sql_rows` and
    /// `sql_duration_ns` are deltas for this closure only; connections the closure does not
    /// touch contribute nothing. Nested scopes are supported (the trace keeps a per-connection
    /// reference count), so measuring an operation that itself measures something is safe.
    #[doc(hidden)]
    pub fn with_stats<T>(
        &self,
        scope: StatsScope,
        work: impl FnOnce() -> T,
    ) -> (T, QueryWorkStats) {
        let connections: Vec<&Connection> = match scope {
            StatsScope::Library => vec![&self.library],
            StatsScope::App => vec![&self.app],
            StatsScope::Both => vec![&self.library, &self.app],
        };
        let measured = QueryWorkScope::new(&connections);
        let value = work();
        let stats = measured.stats(0);
        // Unregister before returning: a later operation on the same connection must not be
        // attributed to this measurement.
        drop(measured);
        (value, stats)
    }
}
