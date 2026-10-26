use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use store::{FilePresence, LedgerPresenceMode, LedgerPresenceObservation, ProbeJobUnitSpec, Store};

fn fixture() -> (tempfile::TempDir, Store, LedgerRow) {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Presence fixture".into(),
        year: None,
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id: media.id,
        path: temp
            .path()
            .join("episode.strm")
            .to_string_lossy()
            .into_owned(),
        season: Some(1),
        episode: Some(1),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    store.insert_scanned_ledger(&row, 1).unwrap();
    let tracks = library::Tracks {
        video: Some(library::VideoTrack::default()),
        ..Default::default()
    };
    store
        .put_file_meta_versioned(
            &row.id.to_string(),
            &tracks,
            Some("retained-version"),
            Some(1_200_000),
        )
        .unwrap();
    (temp, store, row)
}

fn job(store: &Store, id: &str, kind: &str, rows: &[&LedgerRow]) {
    let ids: Vec<_> = rows.iter().map(|row| row.id.to_string()).collect();
    let units: Vec<_> = ids
        .iter()
        .map(|id| ProbeJobUnitSpec {
            ledger_id: id,
            kind: "tv",
            force_fingerprint: true,
            reuse_fingerprint_cache: true,
            overwrite_markers: false,
            reuse_media_info_cache: true,
        })
        .collect();
    assert!(
        store
            .create_probe_job(id, kind, &rows[0].media_id.to_string(), Some(1), id, &units)
            .unwrap()
    );
}

fn observe(store: &Store, row: &LedgerRow, missing_at: Option<i64>, presence: FilePresence) {
    store
        .apply_ledger_presence(
            &[LedgerPresenceObservation {
                row: row.clone(),
                missing_at,
                presence,
            }],
            LedgerPresenceMode::Verify,
            100,
        )
        .unwrap();
}

#[test]
fn missing_file_cancels_queued_and_running_units_without_deleting_facts() {
    for running in [false, true] {
        let (_temp, store, row) = fixture();
        job(&store, "probe", "media_probe", &[&row]);
        if running {
            assert!(
                store
                    .start_probe_unit("probe", &row.id.to_string())
                    .unwrap()
            );
        }
        observe(&store, &row, None, FilePresence::Missing);

        assert!(store.scan_probe_obligations(100).unwrap().is_empty());

        let probe = store.get_probe_job("probe").unwrap().unwrap();
        assert_eq!(probe.status, "cancelled");
        assert_eq!((probe.total, probe.completed, probe.failed), (1, 1, 0));
        let units = store.probe_job_units("probe").unwrap();
        assert_eq!(units[0].status, "cancelled");
        assert_eq!(units[0].error.as_deref(), Some("file_deleted"));
        assert!(
            store
                .active_probe_unit_for_ledger(&row.id.to_string())
                .unwrap()
                .is_none()
        );
        assert!(store.get_ledger(&row.id.to_string()).unwrap().is_some());
        assert!(store.get_media(row.media_id).unwrap().is_some());
        assert_eq!(
            store
                .get_media_info_cache_version(&row.id.to_string())
                .unwrap()
                .unwrap()
                .source_version,
            "retained-version"
        );
    }
}

#[test]
fn missing_refresh_member_cancels_atomic_refresh_but_keeps_independent_work() {
    let (_temp, store, row) = fixture();
    let mut other = row.clone();
    other.id = LedgerId::new();
    other.path.push_str(".other");
    other.episode = Some(2);
    store.insert_ledger(&other).unwrap();
    let mut independent = other.clone();
    independent.id = LedgerId::new();
    independent.path.push_str(".independent");
    store.insert_ledger(&independent).unwrap();
    job(&store, "refresh", "marker_refresh", &[&row, &other]);
    job(&store, "other", "media_probe", &[&independent]);
    observe(&store, &row, None, FilePresence::Missing);
    let refresh = store.get_probe_job("refresh").unwrap().unwrap();
    assert_eq!(refresh.status, "cancelled");
    assert_eq!((refresh.total, refresh.completed), (2, 2));
    assert!(
        store
            .probe_job_units("refresh")
            .unwrap()
            .iter()
            .all(|unit| unit.status == "cancelled")
    );
    assert!(store.get_probe_job("other").unwrap().unwrap().is_active());
}

#[test]
fn restored_file_rearms_one_obligation_and_retains_prior_probe_history() {
    let (_temp, store, row) = fixture();
    job(&store, "probe", "media_probe", &[&row]);
    observe(&store, &row, None, FilePresence::Missing);
    store.ack_scan_probe_obligation(row.id).unwrap();
    observe(&store, &row, Some(100), FilePresence::Present);
    observe(&store, &row, None, FilePresence::Present);
    let obligations = store.scan_probe_obligations(100).unwrap();
    assert_eq!(obligations.len(), 1);
    assert_eq!(obligations[0].ledger_id, row.id);
    assert_eq!(
        store.get_probe_job("probe").unwrap().unwrap().status,
        "cancelled"
    );
    assert_eq!(
        store
            .get_ledger(&row.id.to_string())
            .unwrap()
            .unwrap()
            .media_id,
        row.media_id
    );
    assert!(store.get_file_meta(&row.id.to_string()).unwrap().is_some());
}

#[test]
fn watcher_reappearance_rearms_probe_obligation_only_when_missing() {
    let (_temp, store, row) = fixture();
    observe(&store, &row, None, FilePresence::Missing);
    store.ack_scan_probe_obligation(row.id).unwrap();
    assert!(store.clear_ledger_missing(&row.path).unwrap());
    assert!(!store.clear_ledger_missing(&row.path).unwrap());
    let obligations = store.scan_probe_obligations(100).unwrap();
    assert_eq!(obligations.len(), 1);
    assert_eq!(obligations[0].ledger_id, row.id);
}

#[test]
fn unknown_or_stale_presence_does_not_cancel_probe_work() {
    let (_temp, store, row) = fixture();
    job(&store, "probe", "media_probe", &[&row]);
    observe(&store, &row, None, FilePresence::Unknown);
    observe(&store, &row, Some(100), FilePresence::Missing);
    assert_eq!(store.probe_job_units("probe").unwrap()[0].status, "queued");
}

#[test]
fn reopening_legacy_missing_rows_cancels_probe_work_once() {
    let (temp, store, row) = fixture();
    job(&store, "probe", "media_probe", &[&row]);
    drop(store);
    let conn = rusqlite::Connection::open(temp.path().join("library.db")).unwrap();
    conn.execute(
        "UPDATE ledger SET missing_at = 100 WHERE id = ?1",
        [row.id.to_string()],
    )
    .unwrap();
    conn.execute_batch("PRAGMA user_version = 17").unwrap();
    drop(conn);
    for _ in 0..2 {
        let store = Store::open(temp.path()).unwrap();
        assert_eq!(
            store.get_probe_job("probe").unwrap().unwrap().status,
            "cancelled"
        );
        assert!(store.scan_probe_obligations(100).unwrap().is_empty());
        assert!(store.get_ledger(&row.id.to_string()).unwrap().is_some());
    }
}

#[test]
fn stale_complete_ack_does_not_remove_restored_source_obligation() {
    let (_temp, store, row) = fixture();
    let old = store.scan_probe_obligations(1).unwrap().remove(0);
    observe(&store, &row, None, FilePresence::Missing);
    observe(&store, &row, Some(100), FilePresence::Present);
    assert!(!store.ack_scan_probe_obligation_if_current(&old).unwrap());
    let restored = store.scan_probe_obligations(1).unwrap().remove(0);
    assert_eq!(restored.reason, "source_restored");
    assert!(
        store
            .ack_scan_probe_obligation_if_current(&restored)
            .unwrap()
    );
    assert!(store.scan_probe_obligations(1).unwrap().is_empty());
}
