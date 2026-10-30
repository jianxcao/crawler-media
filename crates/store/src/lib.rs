//! System-of-record store for crawler-media (ADR-0008).
//! Backed by 4 SQLite files in the data directory.

#![allow(
    clippy::type_complexity,
    clippy::too_many_arguments,
    clippy::cloned_ref_to_slice_refs,
    clippy::needless_borrows_for_generic_args
)]

mod app_write;
mod transfer_settings;
pub use transfer_settings::{TransferSettings, TransferSettingsPatch};
mod catalog;
mod chapter_frame_tasks;
mod file_reads;
mod instance_lock;
pub use instance_lock::AppHeartbeat;
mod app_auth;
mod writers;
pub use writers::{App, Db, Writers};
mod pending_file_deletions;
mod pending_progress;
mod protocol_index;
mod query_projection;
pub use chapter_frame_tasks::{ChapterFrameTask, ChapterFrameTaskCounts};
mod collections;
mod data_retention;
mod downloaders;
mod fingerprint_attempts;
mod fingerprint_cache;
mod fingerprint_models;
mod fingerprint_samples;
mod ledger;
pub use ledger::FileReadScope;
pub use library_query::{
    FavoriteOrder, HydratedFile, PageSnapshot, PageValidation, PlaybackViewPage, PlaybackViewQuery,
    PlaybackViewRow,
};
mod ledger_cleanup;
mod ledger_presence;
mod ledger_quality;
pub use ledger_presence::{
    AppliedLedgerPresence, FilePresence, LedgerPresenceMode, LedgerPresenceObservation,
    LedgerPresenceOutcome, LedgerPresenceResult,
};
mod libraries;
mod library_stats;
pub use library_stats::LibraryRosterStats;
pub mod files;
mod library;
mod library_read;
pub use library_read::{
    LibraryReadContext, LibraryReadError, LibraryReadLease, LibraryReadPool, LibraryReadRequest,
    LibraryReadResult, QueryBudget, ReadCredential,
};
mod unidentified;
pub use unidentified::UnidentifiedRow;
mod library_covers;
mod library_deletion;
pub mod library_query;
mod library_search;
pub use library_search::{LibrarySearchHit, LibrarySearchPage};
pub mod library_write;
pub use library_deletion::LibraryRemoval;
mod library_key_owner;
mod library_ownership;
mod library_paths;
pub use library_paths::path_under_root;
mod library_scope;
pub use library_scope::{LibraryPathResolver, LibraryPathScope, StrictOwner};
mod subscribe_library_snapshot;
pub use pending_file_deletions::{MAX_FILE_DELETION_ATTEMPTS, QueuedFileDeletion, file_identity};
pub use subscribe_library_snapshot::SubscribeLibrarySnapshot;
mod subscribe_library;
pub use subscribe_library::{
    ResolvedSubscribeFacts, resolve_subscribe_facts, resolve_subscribe_facts_with,
};
mod maps;
mod media;
mod media_operations;
pub mod metadata;
mod open;
mod operations_retention;
pub use operations_retention::PruneCounts;
pub mod path_key;
pub mod playback;
mod playback_devices;
mod probe_dispatch;
mod probe_tasks;
pub use probe_dispatch::{
    PENDING_DISPATCH_SQL, ProbeDispatchId, ProbeDispatchSnapshot, ProbeDispatchStage,
};
pub mod library_operations;
mod roots;
pub use library_operations::LibraryReconfiguration;
pub use roots::RootReassignment;
mod scan_context;
mod scan_operations;
mod scan_probe_obligations;
mod scan_seen;
pub use scan_context::ScanWriteContext;
mod scanned_ledger;
pub mod schema;
pub use scan_operations::{ScanOperation, ScanOperationStatus};
pub use scan_probe_obligations::ScannedLedger;
pub use scan_probe_obligations::{MAX_SCAN_BATCH, ScanProbeObligation};
mod search_history;
pub use search_history::{HistoryDeleteOutcome, HistoryDeletePlan};
mod sites;
mod subscribe_wanted;
mod subscribes;
mod users;
pub mod work_counters;
mod write_tx;
pub(crate) use write_tx::write_tx;

pub mod password;
pub mod probe_state;
/// Explicit SQL diagnostic scopes. No tracer is installed until a caller opts in.
#[doc(hidden)]
pub mod stats;
/// Test-only seams (fixtures, diagnostics, the bounded test gate). Compiled for this
/// crate's own tests or when a dependent enables `test-support` from its
/// `[dev-dependencies]`; never part of the published surface, so `cargo doc` does
/// not list it.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod test_support;

pub use file_reads::PlaybackUnitSnapshot;
mod read_freshness;
pub use catalog::CatalogCacheRow;
pub use data_retention::{DataRetentionBatch, DataRetentionCursor, RetentionClass};
pub use downloaders::DownloaderRow;
pub use fingerprint_attempts::StoredFingerprintAttempt;
pub use fingerprint_cache::{FingerprintCacheEntry, MediaInfoCacheVersion};
pub use fingerprint_models::{StoredFingerprintModel, StoredFingerprintModelMember};
pub use fingerprint_samples::{FingerprintSampleQuery, StoredFingerprintSample};
pub use libraries::{Library, PreparedLibraryRoots};
pub use library::{
    MarkerResultReplacement, SourceObservation, SourceSnapshot, StoredMediaMarker, observe_sources,
    observe_sources_with,
};
pub use library_ownership::{LibraryOwnership, classify_new_file};
pub use playback::{
    PlayLogRow, PlaySessionBinding, PlaySessionIdState, PlaybackLogPage, PlaybackLogScope,
    PlaybackRetentionBatch, PlaybackRetentionPreview, ResolvedResume, ResumeStatus, SessionRow,
    UNIT_WHOLE, UnitRow, UnitState, UserLibrarySummary, WatchClient, WatchDay, WatchMedia,
    WatchMember, WatchReport, WatchTotals, WatchWindow,
};
pub use probe_state::{
    ProbeStage, ProbeStageCompletion, ProbeStageKey, ProbeStageState, ProbeStageStatus,
    retry_delay_ms,
};
pub use probe_tasks::{ProbeJob, ProbeJobUnit, ProbeJobUnitSpec};
pub use read_freshness::{FileReadRevision, ReadFingerprint};
pub use roots::LibraryRoot;
pub use subscribe_wanted::WantedHistory;
pub use subscribes::PendingDownload;

use std::path::PathBuf;

use rusqlite::Connection;

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// RFC3339 UTC string for the current wall-clock time (second precision).
pub fn now_rfc3339() -> String {
    rfc3339_from_secs(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    )
}

/// Format a Unix timestamp as RFC3339 UTC (`YYYY-MM-DDTHH:MM:SSZ`), hand-rolled
/// to avoid a chrono dependency (civil-from-days, valid for our range).
pub fn rfc3339_from_secs(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let h = rem / 3600;
    let m = (rem % 3600) / 60;
    let sec = rem % 60;
    let month_days = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut year = 1970i64;
    let mut d = days;
    loop {
        let ydays = if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
            366
        } else {
            365
        };
        if d < ydays {
            break;
        }
        d -= ydays;
        year += 1;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let mut month = 0usize;
    let mut day = d;
    for (i, mdays) in month_days.iter().enumerate() {
        let dim = if i == 1 && leap { 29 } else { *mdays };
        if day < dim {
            month = i + 1;
            break;
        }
        day -= dim;
    }
    format!(
        "{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{sec:02}Z",
        day = day + 1
    )
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("app writer is temporarily busy")]
    WriteBusy,
    #[error("library {library_id} query projection is temporarily unavailable")]
    QueryUnavailable { library_id: domain::LibraryId },
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("invalid media kind: {0}")]
    MediaKind(String),
    #[error("invalid fetch mode: {0}")]
    FetchMode(String),
    #[error("invalid uuid: {0}")]
    Uuid(#[from] uuid::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("missing {0}")]
    Missing(String),
    #[error("invalid argument: {0}")]
    Invalid(String),
    /// A database file exists but was written by another layout. The layout is
    /// not migrated (see `docs/superpowers/specs/2026-10-18-library-storage-redesign.md`):
    /// the file is refused and the operator rebuilds it.
    #[error("layout mismatch: {0}")]
    LayoutMismatch(String),
    #[error("protected: {0}")]
    Protected(String),
    #[error("credential conflict: {0}")]
    CredentialConflict(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("password hashing error: {0}")]
    PasswordHash(String),
}

impl StoreError {
    /// The connection's on-disk layout is not the one this build expects, so
    /// nothing may be read or written through it.
    ///
    /// `Invalid` still carries refusals for the app file, while the library and
    /// subscribe files report the same condition as `LayoutMismatch`; callers that
    /// react to a refusal (the read pool closes on it) must match both, so they ask
    /// here instead of repeating the variant list at each site.
    pub fn is_layout_refusal(&self) -> bool {
        matches!(self, Self::Invalid(_) | Self::LayoutMismatch(_))
    }
}

pub struct Store {
    data_dir: PathBuf,
    /// Times this store's `list_file_coverage` executed its unfiltered
    /// `FROM library_files f LEFT JOIN library_file_units u` statement.
    /// Per store family (a writer and its `open_reader` readers share it), so
    /// concurrent tests never share the diagnostic.
    pub(crate) root_ledger_scans: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Calls to `file_coverage_for_media` on this store. Selection can load the
    /// same media twice; the logical candidate count does not show that read.
    pub(crate) media_ledger_reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Per-store role lookup diagnostic for bounded roster work.
    pub(crate) role_lookups: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    app_path: PathBuf,
    pub(crate) app: Connection,
    writers: std::sync::Arc<Writers>,
    read_only: bool,
    pub(crate) cache: Connection,
    pub(crate) library: Connection,
    pub(crate) subscribe: Connection,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("data_dir", &self.data_dir)
            .field("app_path", &self.app_path)
            .finish()
    }
}

impl Store {
    /// Times `list_file_coverage` executed its unfiltered
    /// `FROM library_files f LEFT JOIN library_file_units u` statement.
    ///
    /// Incremented beside that statement, so filtering the returned rows cannot
    /// hide the scan. `file_coverage_for_media` does not increment it.
    pub fn root_ledger_scans(&self) -> usize {
        self.root_ledger_scans
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Times `file_coverage_for_media` executed its indexed read.
    pub fn media_ledger_reads(&self) -> usize {
        self.media_ledger_reads
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn role_lookups(&self) -> usize {
        self.role_lookups.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn reset_role_lookups(&self) {
        self.role_lookups
            .store(0, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn cache_db_path(&self) -> PathBuf {
        self.data_dir.join("cache.db")
    }

    /// Freeze committed root aliases; the returned observer performs IO without Store.
    pub fn nfo_input_observer(&self) -> crate::metadata::nfo_input::NfoInputObserver {
        crate::metadata::nfo_input::NfoInputObserver {
            aliases: crate::path_key::alias_snapshot(&self.library),
        }
    }
}

mod metadata_language;
pub use metadata_language::{MetadataLanguageOperation, MetadataLanguageState};
