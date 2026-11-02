//! System-of-record store for crawler-media (ADR-0008).
//! Backed by 4 SQLite files in the data directory.

#![allow(
    clippy::type_complexity,
    clippy::too_many_arguments,
    clippy::cloned_ref_to_slice_refs,
    clippy::needless_borrows_for_generic_args
)]

mod app_write;
mod scrape_settings;
mod transfer_settings;
pub use scrape_settings::RawScrapeSettings;
pub use transfer_settings::{TransferSettings, TransferSettingsPatch};
mod catalog;
mod chapter_frame_tasks;
mod file_reads;
mod instance_lock;
pub use instance_lock::AppHeartbeat;
mod app_auth;
mod connections;
mod progress;
mod read_scope;
mod writer_rank;
mod writers;
pub use connections::{LibraryConnection, SubscribeConnection};
pub use writer_rank::{WriterRank, WriterRankGuard};
pub use writers::{App, Db, Writers};
mod pending_file_deletions;
mod pending_progress;
mod protocol_index;
mod query_projection;
pub use chapter_frame_tasks::{ChapterFrameTask, ChapterFrameTaskCounts};
mod collections;
pub use collections::{CollectionItemFacts, CollectionItemPage, CollectionRead, UserCollection};
mod data_retention;
mod downloaders;
mod fingerprint_attempts;
mod fingerprint_cache;
mod fingerprint_models;
mod fingerprint_samples;
mod ledger;
pub use ledger::{FileReadScope, LedgerRow};
pub use library_query::{
    ContinueEntry, ContinuePage, ContinueQuery, FavoriteOrder, HomeEntry, HomeQuery, HomeWindow,
    HydratedFile, PageSnapshot, PageValidation, PlaybackViewPage, PlaybackViewQuery,
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
mod library_statistics;
mod library_stats;
pub use library_stats::LibraryRosterStats;
pub mod files;
mod library;
mod library_read;
pub use library_read::{
    LibraryReadContext, LibraryReadError, LibraryReadRequest, LibraryReadResult, QueryBudget,
    ReadCredential, ReaderLease, ReaderPool,
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
mod subscribe_detach;
mod subscribe_job_sync;
mod subscribe_wanted;
mod subscribes;
pub use subscribe_job_sync::SubscribeJobSync;
mod users;
pub mod work_counters;
mod write_tx;
pub(crate) use write_tx::{read_tx, write_tx};

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
    MarkerResultReplacement, MissingFile, SourceObservation, SourceSnapshot, StoredMediaMarker,
    observe_sources, observe_sources_with,
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

mod error;
mod handle;
mod time;

pub use error::StoreError;
pub use handle::Store;
pub use time::{now_rfc3339, rfc3339_from_secs, unix_now};

mod metadata_language;
pub use metadata_language::{MetadataLanguageOperation, MetadataLanguageState};

pub use library_query::protocol::{
    ProtocolCounts, ProtocolEntity, ProtocolEntityKind, ProtocolEntityPage, ProtocolEntityQuery,
    ProtocolPerson, ProtocolSeason,
};
