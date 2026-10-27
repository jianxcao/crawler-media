//! Deterministic publication work counters (`test-support` seam).
//!
//! A no-op republish is the common case, so "how much work did it do" must be assertable
//! without wall clock. In production builds [`PublicationCounters`] is an empty struct and
//! every recorder compiles away.

/// Deterministic work counters for one publication call.
///
/// A rescan of an unchanged Library is the common case, so "how much work did an idempotent
/// republish do" has to be assertable without wall clock. `written_rows` comes from SQLite
/// itself (`total_changes`), so it cannot disagree with the database; the rest name the units
/// of work that used to run before the code noticed nothing changed.
#[doc(hidden)]
#[cfg(feature = "test-support")]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct PublishStats {
    /// Rows SQLite reported as inserted, updated or deleted.
    pub written_rows: u64,
    /// Files whose row or coverage changed (and therefore need a summary reduction).
    pub changed_files: u64,
    /// Files already current: compared, nothing written, no reduction.
    pub unchanged_files: u64,
    /// Media whose summaries were reduced.
    pub reduced_media: u64,
    /// Coverage rewrites (`library_file_units`) performed.
    pub coverage_syncs: u64,
}

/// Counters threaded through the publication loop.
///
/// In production builds this is an empty struct and every `record_*` method compiles away, so
/// the hot path is unchanged; the `test-support` feature turns it into the seam that
/// [`Store::publish_files_with_stats`] reports.
#[derive(Default)]
pub(crate) struct PublicationCounters {
    #[cfg(feature = "test-support")]
    stats: PublishStats,
}

impl PublicationCounters {
    pub(crate) fn file_changed(&mut self) {
        #[cfg(feature = "test-support")]
        {
            self.stats.changed_files += 1;
        }
    }

    pub(crate) fn file_unchanged(&mut self) {
        #[cfg(feature = "test-support")]
        {
            self.stats.unchanged_files += 1;
        }
    }

    pub(crate) fn coverage_synced(&mut self) {
        #[cfg(feature = "test-support")]
        {
            self.stats.coverage_syncs += 1;
        }
    }

    pub(crate) fn media_reduced(&mut self) {
        #[cfg(feature = "test-support")]
        {
            self.stats.reduced_media += 1;
        }
    }

    /// The counters collected so far (`test-support` only).
    #[cfg(feature = "test-support")]
    pub(crate) fn into_stats(self) -> PublishStats {
        self.stats
    }
}
