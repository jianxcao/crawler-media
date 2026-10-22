//! Regression tests for the P1/P2 design work: DB-sourced path maps,
//! filter-atom JSON storage (legacy DSL rows are refused, not decoded), and the
//! legacy `playback_progress` table migration.

use api::{ChosenDownloader, DownloaderEnv, Store, choose_downloader};
use domain::{AtomRule, Filter, FilterAtom, FilterId};

fn empty_env() -> DownloaderEnv {
    DownloaderEnv {
        qb_url: None,
        qb_user: None,
        qb_pass: None,
        qb_category: None,
        qb_path_maps: vec![],
        tr_path_maps: vec![],
    }
}

// ---------------------------------------------------------------------------
// path_maps: DB is the source of truth, env overrides when set
// ---------------------------------------------------------------------------

#[test]
fn downloader_path_maps_come_from_db_when_env_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let row = api::store::DownloaderRow {
        id: domain::DownloaderId::new(),
        name: "nas".into(),
        kind: "qbittorrent".into(),
        url: "http://qb.example:8080".into(),
        username: Some("admin".into()),
        password: Some("secret".into()),
        category: None,
        path_maps: vec![downloader::PathMap::new("/downloads", "/volume1/downloads")],
        is_default: true,
        enabled: true,
    };
    store.insert_downloader(&row).unwrap();

    let ChosenDownloader::Qbittorrent(cfg) = choose_downloader(&store, &empty_env()).unwrap()
    else {
        panic!("expected qBittorrent from DB row");
    };
    assert_eq!(cfg.path_maps.len(), 1);
    assert_eq!(cfg.path_maps[0].from, "/downloads");
}

#[test]
fn downloader_path_maps_env_overrides_db() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let row = api::store::DownloaderRow {
        id: domain::DownloaderId::new(),
        name: "nas".into(),
        kind: "qbittorrent".into(),
        url: "http://qb.example:8080".into(),
        username: Some("admin".into()),
        password: Some("secret".into()),
        category: None,
        path_maps: vec![downloader::PathMap::new("/db-from", "/db-to")],
        is_default: true,
        enabled: true,
    };
    store.insert_downloader(&row).unwrap();

    let env = DownloaderEnv {
        qb_path_maps: vec![downloader::PathMap::new("/env-from", "/env-to")],
        ..empty_env()
    };
    let ChosenDownloader::Qbittorrent(cfg) = choose_downloader(&store, &env).unwrap() else {
        panic!("expected qBittorrent from DB row");
    };
    assert_eq!(cfg.path_maps.len(), 1);
    assert_eq!(cfg.path_maps[0].from, "/env-from");
}

// ---------------------------------------------------------------------------
// Filter atoms: JSON round-trip + legacy DSL compatibility
// ---------------------------------------------------------------------------

fn sample_atoms() -> Vec<FilterAtom> {
    vec![
        FilterAtom {
            priority: 100,
            rule: AtomRule::Resolution("2160p".into()),
            exclude: false,
        },
        FilterAtom {
            priority: 50,
            rule: AtomRule::Size {
                min_mb: Some(1000),
                max_mb: Some(50_000),
            },
            exclude: false,
        },
        FilterAtom {
            priority: 10,
            rule: AtomRule::Hr,
            exclude: true,
        },
    ]
}

#[test]
fn filter_atoms_round_trip_through_storage() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let filter = Filter {
        id: FilterId::new(),
        name: "测试".into(),
        atoms: sample_atoms(),
        keep_old_versions: false,
    };
    store.insert_filter(&filter).unwrap();

    let loaded = store.get_filter(filter.id).unwrap().unwrap();
    assert_eq!(loaded.atoms, sample_atoms());

    // The stored payload must be JSON, not the legacy DSL.
    let conn = rusqlite::Connection::open(dir.path().join("app.db")).unwrap();
    let raw: String = conn
        .query_row(
            "SELECT atoms_json FROM filters WHERE id = ?1",
            rusqlite::params![filter.id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        raw.trim_start().starts_with('['),
        "expected JSON, got {raw}"
    );
}

#[test]
fn legacy_dsl_filter_rows_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let id = FilterId::new();
    let conn = rusqlite::Connection::open(dir.path().join("app.db")).unwrap();
    conn.execute(
        "INSERT INTO filters (id, name, atoms_json) VALUES (?1, ?2, ?3)",
        rusqlite::params![id.to_string(), "旧库", "100:resolution=2160p|50:!hr"],
    )
    .unwrap();
    drop(conn);

    // 切割后旧 shape 必须被拒，且不得静默退化成空 atoms。
    let err = store
        .get_filter(id)
        .expect_err("legacy DSL rows must be refused");
    assert!(
        err.to_string().contains("legacy shape"),
        "expected a legacy-shape rejection, got {err}"
    );
    assert!(
        store.list_filters().is_err(),
        "listing must surface the unreadable row instead of dropping it"
    );
}

#[test]
fn seeded_default_filter_parses() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let filters = store.list_filters().unwrap();
    let default = filters
        .iter()
        .find(|f| f.name == "默认")
        .expect("seeded default filter");
    assert_eq!(default.atoms.len(), 2);
    assert!(
        default
            .atoms
            .iter()
            .all(|a| matches!(a.rule, AtomRule::Resolution(_)))
    );
}

// ---------------------------------------------------------------------------
// Legacy playback_progress table migration
// ---------------------------------------------------------------------------

#[test]
fn legacy_playback_progress_is_merged_and_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let subscribe_db = dir.path().join("subscribe.db");
    let user_id = domain::UserId::new();
    let media_id = domain::MediaId::new();

    // 旧库：subscribe.db 里还有 legacy playback_progress 表。
    {
        let conn = rusqlite::Connection::open(&subscribe_db).unwrap();
        conn.execute_batch(
            "CREATE TABLE playback_progress (
                user_id TEXT NOT NULL,
                media_id TEXT NOT NULL,
                position_ms INTEGER NOT NULL,
                played INTEGER NOT NULL DEFAULT 0,
                favorite INTEGER NOT NULL DEFAULT 0,
                season INTEGER,
                episode INTEGER,
                PRIMARY KEY(user_id, media_id)
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO playback_progress (user_id, media_id, position_ms, played, favorite)
             VALUES (?1, ?2, 4242, 1, 0)",
            rusqlite::params![user_id.to_string(), media_id.to_string()],
        )
        .unwrap();
    }

    // v1 布局没有迁移阶梯：旧 subscribe.db 被明确拒绝，不会被静默改写。
    let error = Store::open(dir.path()).expect_err("旧 subscribe 布局必须被拒绝");
    assert!(
        error.to_string().contains("layout mismatch"),
        "拒绝原因必须指明布局不匹配: {error}"
    );

    // 旧表仍原样保留，Store 没有删除或改写它。
    let conn = rusqlite::Connection::open(&subscribe_db).unwrap();
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM playback_progress", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 1, "被拒绝的旧库不得被改写");
}

#[test]
fn fresh_install_never_creates_playback_progress() {
    let dir = tempfile::tempdir().unwrap();
    let _store = Store::open(dir.path()).unwrap();
    let conn = rusqlite::Connection::open(dir.path().join("subscribe.db")).unwrap();
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='playback_progress')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!exists, "new installs must not create the legacy table");
}

// ---------------------------------------------------------------------------
// Downloader enabled flag
// ---------------------------------------------------------------------------

#[test]
fn disabled_default_downloader_is_not_selected() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let row = api::store::DownloaderRow {
        id: domain::DownloaderId::new(),
        name: "nas".into(),
        kind: "qbittorrent".into(),
        url: "http://qb.example:8080".into(),
        username: Some("admin".into()),
        password: Some("secret".into()),
        category: None,
        path_maps: vec![],
        is_default: true,
        enabled: false,
    };
    store.insert_downloader(&row).unwrap();

    assert!(
        store.default_downloader().unwrap().is_none(),
        "a disabled default must not be auto-selected"
    );
    assert!(choose_downloader(&store, &empty_env()).is_err());
}

#[test]
fn enabled_flag_round_trips_and_toggles() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let row = api::store::DownloaderRow {
        id: domain::DownloaderId::new(),
        name: "nas".into(),
        kind: "qbittorrent".into(),
        url: "http://qb.example:8080".into(),
        username: Some("admin".into()),
        password: Some("secret".into()),
        category: None,
        path_maps: vec![],
        is_default: true,
        enabled: true,
    };
    store.insert_downloader(&row).unwrap();
    assert!(store.get_downloader(row.id).unwrap().unwrap().enabled);

    assert!(store.set_downloader_enabled(row.id, false).unwrap());
    assert!(!store.get_downloader(row.id).unwrap().unwrap().enabled);

    assert!(store.set_downloader_enabled(row.id, true).unwrap());
    assert!(store.get_downloader(row.id).unwrap().unwrap().enabled);

    // Unknown id reports "not found" rather than silently succeeding.
    assert!(
        !store
            .set_downloader_enabled(domain::DownloaderId::new(), false)
            .unwrap()
    );
}

// ---------------------------------------------------------------------------
// Schema version tracking
// ---------------------------------------------------------------------------

#[test]
fn schema_versions_are_stamped() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let versions = store.schema_versions();
    assert_eq!(versions.len(), 4);
    for (name, version) in versions {
        assert!(version > 0, "{name}.db has no schema version stamped");
    }
}
