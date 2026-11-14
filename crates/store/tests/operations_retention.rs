//! Retention of the durable operation journals.
//!
//! `library_operations`, `scan_operations`, `media_operations` and `library_removals` are
//! append-only journals: every finished scan or reconfiguration leaves a row behind, and the
//! browse-page integrity lookup reads the *latest* `library_operations` row of a Library, so
//! the table head is a hot path. Retention drops rows whose operation has reached a terminal
//! state and whose last update is older than the window — nothing else.
//!
//! Every seeded timestamp comes from an explicit `now_ms`/SQL value: the rule under test is a
//! cutoff comparison, and the real APIs would stamp "now".

use std::path::{Path, PathBuf};

use domain::{LibraryId, MediaKind};
use rusqlite::params;
use store::ScanOperationStatus;
use store::Store;
use store::library_query::LibraryQuery;

const OLD_MS: i64 = 1_000;
const NEW_MS: i64 = 9_000;
const CUTOFF_MS: i64 = 5_000;

struct Fixture {
    _temp: tempfile::TempDir,
    store: Store,
    library_id: LibraryId,
    db_path: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("data")).unwrap();
        let root = temp.path().join("movies");
        std::fs::create_dir_all(&root).unwrap();
        let library = store
            .create_library(
                MediaKind::Movie,
                "Movies",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let library_id = library.id.parse().unwrap();
        let db_path = temp.path().join("data").join("library.db");
        Self {
            _temp: temp,
            store,
            library_id,
            db_path,
        }
    }

    /// A second connection: seeding and assertions read committed rows, never the Store's own.
    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.db_path).unwrap()
    }

    /// `library_operations` is seeded directly: its state machine stamps `now_ms`, and the
    /// cutoff is the subject here.
    fn seed_library_operation(&self, id: &str, kind: &str, state: &str, at_ms: i64) {
        self.db()
            .execute(
                "INSERT INTO library_operations(operation_id, library_id, kind, state,
                     expected_revision, plan_json, created_at_ms, updated_at_ms, resource_key)
                 VALUES (?1, ?2, ?3, ?4, 0, '{}', ?5, ?5, ?1)",
                params![id, self.library_id.to_string(), kind, state, at_ms],
            )
            .unwrap();
    }

    /// A completed scan through the public API, so the row carries a real status string.
    fn seed_scan(&self, at_ms: i64, status: ScanOperationStatus) -> String {
        let op = self
            .store
            .enqueue_scan(&self.library_id.to_string(), "scope", &[], at_ms)
            .unwrap();
        if status != ScanOperationStatus::Queued {
            self.store
                .update_scan_operation_status(&op.id, status, &serde_json::json!({}))
                .unwrap();
        }
        op.id
    }

    fn seed_scan_seen(&self, operation_id: &str) {
        self.db()
            .execute(
                "INSERT INTO scan_seen(operation_id, path, path_key) VALUES (?1, '/m/a.mkv', '/m/a.mkv')",
                params![operation_id],
            )
            .unwrap();
    }

    fn seed_media_operation(&self, id: &str, state: &str, at_ms: i64) {
        self.db()
            .execute(
                "INSERT INTO media_operations(operation_id, kind, state, survivor_id, loser_id,
                     plan_json, created_at_ms, updated_at_ms)
                 VALUES (?1, 'merge', ?2, 'survivor', 'loser', '{}', ?3, ?3)",
                params![id, state, at_ms],
            )
            .unwrap();
    }

    /// A removal journal row plus both child tables and its `library_query_count_rebuild`
    /// sibling, so an orphaned child would be visible.
    fn seed_removal(&self, id: &str, state: &str, at_ms: i64) -> String {
        let library = LibraryId::new().to_string();
        let db = self.db();
        db.execute(
            "INSERT INTO library_removals(operation_id, library_id, state, config_revision,
                 created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, 0, ?4, ?4)",
            params![id, library, state, at_ms],
        )
        .unwrap();
        db.execute(
            "INSERT INTO library_removal_roots(operation_id, path) VALUES (?1, '/m')",
            params![id],
        )
        .unwrap();
        db.execute(
            "INSERT INTO library_removal_files(operation_id, file_id, root_id, path_key,
                 display_path, media_id, file_revision, source_version, units_json)
             VALUES (?1, 'file-1', 'root-1', '/m/a.mkv', '/m/a.mkv', 'media-1', 1, 'v1', '[]')",
            params![id],
        )
        .unwrap();
        library
    }
}

fn item_query(library_id: LibraryId) -> LibraryQuery {
    LibraryQuery {
        library_id,
        user_id: None,
        filter: Default::default(),
        order: store::library_query::BrowseOrder::Title,
        descending: false,
        page: store::library_query::BrowsePage::Offset {
            start: 0,
            limit: 10,
        },
        include_total: false,
    }
}

fn count(db: &rusqlite::Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn child_rows(db: &Path, table: &str, operation_id: &str) -> i64 {
    rusqlite::Connection::open(db)
        .unwrap()
        .query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE operation_id = ?1"),
            [operation_id],
            |row| row.get(0),
        )
        .unwrap()
}

/// The whole point: a `running` (or retryable-failed) journal row is a *live* operation.
#[test]
fn prunes_only_terminal_operations_before_cutoff() {
    let fix = Fixture::new();
    let seeded = seed_all_journals(&fix);

    let counts = fix
        .store
        .prune_terminal_operations(CUTOFF_MS, 500, false)
        .unwrap();
    assert_eq!(
        counts.deleted, 4,
        "one terminal row per transient journal older than the cutoff: {counts:?}"
    );

    assert_library_operations(&fix);
    assert_scan_operations(&fix, &seeded);
    assert_media_operations(&fix);
    assert_removals(&fix, &seeded);
}

/// One prunable, one too-new and one live-or-retryable row per journal.
struct Seeded {
    scan_old: String,
    scan_new: String,
    scan_running: String,
    removal_old_library: String,
    removal_new_library: String,
}

fn seed_all_journals(fix: &Fixture) -> Seeded {
    // library_operations: prunable / too new / live / retryable failure / non-retryable failure
    fix.seed_library_operation("lib-succeeded-old", "roots", "succeeded", OLD_MS);
    fix.seed_library_operation("lib-succeeded-new", "roots", "succeeded", NEW_MS);
    fix.seed_library_operation("lib-applying-old", "roots", "applying", OLD_MS);
    fix.seed_library_operation("lib-failed-retryable-old", "roots", "failed", OLD_MS);
    fix.seed_library_operation("lib-failed-scrape-old", "scrape", "failed", OLD_MS);
    // scan_operations: terminal / too new / live
    let scan_old = fix.seed_scan(OLD_MS, ScanOperationStatus::Completed);
    fix.seed_scan_seen(&scan_old);
    let scan_new = fix.seed_scan(NEW_MS, ScanOperationStatus::Completed);
    let scan_running = fix.seed_scan(OLD_MS, ScanOperationStatus::Running);
    // media_operations: terminal / retryable failure / too new
    fix.seed_media_operation("media-succeeded-old", "succeeded", OLD_MS);
    fix.seed_media_operation("media-failed-old", "failed", OLD_MS);
    fix.seed_media_operation("media-succeeded-new", "succeeded", NEW_MS);
    // library_removals: terminal (+children) / too new (+children)
    let removal_old_library = fix.seed_removal("removal-succeeded-old", "succeeded", OLD_MS);
    let removal_new_library = fix.seed_removal("removal-succeeded-new", "succeeded", NEW_MS);
    Seeded {
        scan_old,
        scan_new,
        scan_running,
        removal_old_library,
        removal_new_library,
    }
}

fn assert_library_operations(fix: &Fixture) {
    let op = |id: &str| fix.store.get_library_operation(id).unwrap();
    assert!(op("lib-succeeded-old").is_none(), "terminal + old");
    assert!(
        op("lib-succeeded-new").is_some(),
        "terminal but inside the window"
    );
    assert!(
        op("lib-applying-old").is_some(),
        "a live operation is never pruned"
    );
    assert!(
        op("lib-failed-retryable-old").is_some(),
        "a retryable failure resumes from its own row; pruning it strands the Library"
    );
    assert!(
        op("lib-failed-scrape-old").is_none(),
        "a non-retryable failure is terminal"
    );
}

fn assert_scan_operations(fix: &Fixture, seeded: &Seeded) {
    assert!(
        fix.store
            .scan_operation(&seeded.scan_old)
            .unwrap()
            .is_none()
    );
    assert!(
        fix.store
            .scan_operation(&seeded.scan_new)
            .unwrap()
            .is_some(),
        "a terminal scan inside the window stays"
    );
    assert!(
        fix.store
            .scan_operation(&seeded.scan_running)
            .unwrap()
            .is_some(),
        "a running scan is not terminal"
    );
    assert_eq!(
        child_rows(&fix.db_path, "scan_seen", &seeded.scan_old),
        0,
        "scan_seen is not foreign-keyed; retention must delete it explicitly"
    );
}

fn assert_media_operations(fix: &Fixture) {
    let db = fix.db();
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM media_operations WHERE operation_id IN
                 ('media-succeeded-old', 'media-succeeded-new', 'media-failed-old')"
        ),
        2,
        "the older succeeded merge is pruned, the newer one stays"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM media_operations WHERE operation_id = 'media-failed-old'"
        ),
        1,
        "a failed merge is retried from its row"
    );
}

fn assert_removals(fix: &Fixture, seeded: &Seeded) {
    // library_removals and its roots serve as the permanent security boundary
    // against re-ingestion, so they are intentionally preserved across retention.
    assert!(
        fix.store
            .removal_for_library(&seeded.removal_old_library)
            .unwrap()
            .is_some()
    );
    assert!(
        fix.store
            .removal_for_library(&seeded.removal_new_library)
            .unwrap()
            .is_some()
    );
    for table in ["library_removal_roots", "library_removal_files"] {
        assert_eq!(
            child_rows(&fix.db_path, table, "removal-succeeded-old"),
            1,
            "{table} rows must remain preserved to defend the removal boundary"
        );
        assert_eq!(
            child_rows(&fix.db_path, table, "removal-succeeded-new"),
            1,
            "children of a retained removal stay"
        );
    }
    let db = fix.db();
    for (table, parent) in [
        ("library_removal_roots", "library_removals"),
        ("library_removal_files", "library_removals"),
        ("library_query_count_rebuild", "library_operations"),
    ] {
        assert_eq!(
            count(
                &db,
                &format!(
                    "SELECT COUNT(*) FROM {table}
                     WHERE operation_id NOT IN (SELECT operation_id FROM {parent})"
                )
            ),
            0,
            "no orphaned {table} rows"
        );
    }
}

/// A batch never exceeds its budget, so one pathological journal cannot starve the others.
#[test]
fn batch_budget_bounds_one_call() {
    let fix = Fixture::new();
    for index in 0..5 {
        fix.seed_library_operation(
            &format!("lib-old-{index}"),
            "roots",
            "succeeded",
            OLD_MS + index,
        );
    }
    let first = fix
        .store
        .prune_terminal_operations(CUTOFF_MS, 2, false)
        .unwrap();
    assert_eq!(
        first.deleted, 2,
        "the batch budget caps one call: {first:?}"
    );
    assert!(first.window_full(2), "a full window means more rows remain");

    let mut deleted = first.deleted;
    loop {
        let next = fix
            .store
            .prune_terminal_operations(CUTOFF_MS, 2, false)
            .unwrap();
        deleted += next.deleted;
        if !next.window_full(2) {
            break;
        }
    }
    assert_eq!(deleted, 5, "repeated batches drain the whole window");
}

#[test]
fn dry_run_reports_without_deleting() {
    let fix = Fixture::new();
    fix.seed_library_operation("lib-succeeded-old", "roots", "succeeded", OLD_MS);
    fix.seed_library_operation("lib-succeeded-new", "roots", "succeeded", NEW_MS);
    let scan_old = fix.seed_scan(OLD_MS, ScanOperationStatus::Completed);
    fix.seed_media_operation("media-succeeded-old", "succeeded", OLD_MS);
    fix.seed_removal("removal-succeeded-old", "succeeded", OLD_MS);
    let before = count(&fix.db(), "SELECT COUNT(*) FROM library_operations");

    let counts = fix
        .store
        .prune_terminal_operations(CUTOFF_MS, 500, true)
        .unwrap();
    assert_eq!(
        counts.examined, 3,
        "one candidate per prunable journal: {counts:?}"
    );
    assert_eq!(counts.deleted, 0, "a dry run writes nothing");

    assert_eq!(
        count(&fix.db(), "SELECT COUNT(*) FROM library_operations"),
        before
    );
    assert!(fix.store.scan_operation(&scan_old).unwrap().is_some());
    assert_eq!(count(&fix.db(), "SELECT COUNT(*) FROM media_operations"), 1);
    assert_eq!(count(&fix.db(), "SELECT COUNT(*) FROM library_removals"), 1);
}

/// The browse/detail integrity verdict is a subquery over the *latest* surviving
/// `library_operations` row of kind `integrity`, so pruning must keep that row's identity.
#[test]
fn integrity_lookup_ignores_pruned() {
    let fix = Fixture::new();
    // An old integrity verdict (prunable) and the current one (inside the window).
    fix.seed_library_operation("integrity-old", "integrity", "succeeded", OLD_MS);
    fix.seed_library_operation("integrity-current", "integrity", "succeeded", NEW_MS);
    fix.db()
        .execute(
            "UPDATE libraries SET integrity_state = 'failed' WHERE library_id = ?1",
            params![fix.library_id.to_string()],
        )
        .unwrap();

    let query = item_query(fix.library_id);
    let before = fix.store.library_item_page(&query).unwrap_err().to_string();
    assert!(
        before.contains("integrity-current"),
        "the browse read names the current verdict: {before}"
    );

    let counts = fix
        .store
        .prune_terminal_operations(CUTOFF_MS, 500, false)
        .unwrap();
    assert_eq!(counts.deleted, 1, "only the superseded verdict is pruned");

    let after = fix.store.library_item_page(&query).unwrap_err().to_string();
    assert_eq!(
        after, before,
        "the latest surviving integrity row still decides the page"
    );
    assert!(
        fix.store
            .get_library_operation("integrity-current")
            .unwrap()
            .is_some()
    );

    // A healthy Library whose only integrity row survived keeps reading normally.
    let healthy = Fixture::new();
    healthy.seed_library_operation("integrity-healthy", "integrity", "succeeded", NEW_MS);
    healthy
        .store
        .prune_terminal_operations(CUTOFF_MS, 500, false)
        .unwrap();
    assert!(
        healthy
            .store
            .library_item_page(&item_query(healthy.library_id))
            .is_ok(),
        "pruning must not turn a clean Library into an unreadable one"
    );
}
