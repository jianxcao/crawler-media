use domain::{Confidence, LedgerId, LedgerRow, MediaId, QualitySource};
use store::{ProbeJobUnitSpec, Store};

fn row(path: &str) -> LedgerRow {
    LedgerRow {
        id: LedgerId::new(),
        media_id: MediaId::new(),
        path: path.into(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    }
}

fn job(store: &Store, row: &LedgerRow, id: &str) {
    assert!(
        store
            .create_probe_job(
                id,
                "media_probe",
                &row.media_id.to_string(),
                None,
                &format!("ledger:{}", row.id),
                &[ProbeJobUnitSpec {
                    ledger_id: &row.id.to_string(),
                    kind: "movie",
                    force_fingerprint: false,
                    reuse_fingerprint_cache: true,
                    overwrite_markers: false,
                    reuse_media_info_cache: true,
                }]
            )
            .unwrap()
    );
}

#[test]
fn upgrade_rearms_only_work_lost_to_capacity_and_preserves_history() {
    let dir = tempfile::tempdir().unwrap();
    let lost = row("/movies/lost.strm");
    let replaced = row("/movies/replaced.strm");
    let user_cancelled = row("/movies/cancelled.strm");
    {
        let store = Store::open(dir.path()).unwrap();
        for (id, row) in [
            ("lost", &lost),
            ("replaced", &replaced),
            ("manual", &user_cancelled),
        ] {
            store.insert_ledger(row).unwrap();
            job(&store, row, id);
            store
                .cancel_probe_unit(
                    id,
                    &row.id.to_string(),
                    if id == "manual" {
                        "user_cancelled"
                    } else {
                        "dispatch window full"
                    },
                )
                .unwrap();
        }
        job(&store, &replaced, "later");
        store
            .finish_probe_unit("later", &replaced.id.to_string(), true, None)
            .unwrap();
    }
    // Fixture from the previous release. The new repair must run once on open.
    let connection = rusqlite::Connection::open(dir.path().join("library.db")).unwrap();
    connection.pragma_update(None, "user_version", 17).unwrap();
    drop(connection);
    let store = Store::open(dir.path()).unwrap();
    let obligations = store.scan_probe_obligations(100).unwrap();
    assert_eq!(obligations.len(), 1);
    assert_eq!(obligations[0].ledger_id, lost.id);
    assert_eq!(
        store.get_probe_job("lost").unwrap().unwrap().status,
        "cancelled"
    );
    store.ack_scan_probe_obligation(lost.id).unwrap();
    drop(store);
    assert!(
        Store::open(dir.path())
            .unwrap()
            .scan_probe_obligations(100)
            .unwrap()
            .is_empty(),
        "ordinary restarts must not resurrect acknowledged historical work"
    );
}

#[test]
fn obligation_ack_accepts_finished_work_but_requires_proof_for_already_running() {
    for status in ["queued", "succeeded", "failed", "cancelled"] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let row = row("/movies/fast.strm");
        store.insert_scanned_ledger(&row, 1000).unwrap();
        let obligation = store.scan_probe_obligations(1).unwrap().remove(0);
        job(&store, &row, "fast");
        if status != "queued" {
            store
                .finish_probe_unit_with_status("fast", &row.id.to_string(), status, None, None)
                .unwrap();
            assert!(
                !store
                    .ack_scan_probe_obligation_for_work(&obligation, None)
                    .unwrap(),
                "AlreadyRunning needs an active durable unit"
            );
        }
        let accepted = store
            .ack_scan_probe_obligation_for_work(&obligation, Some("fast"))
            .unwrap();
        assert_eq!(accepted, status != "cancelled", "{status}");
        assert_eq!(
            store.scan_probe_obligations(1).unwrap().is_empty(),
            accepted
        );
    }
}
