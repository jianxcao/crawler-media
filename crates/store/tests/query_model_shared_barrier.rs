mod query_model_support;
use domain::{FilePresence, LedgerId, UnitKey};
use query_model_support::setup;
use store::library_write::probe::ProbePublication;
use store::library_write::{FilePublication, WriteScope};
use store::{LedgerPresenceMode as PresenceMode, LedgerPresenceObservation};

#[test]
fn shared_media_presence_verify_is_frozen() {
    rejects_shared_write("verify");
}
#[test]
fn shared_media_presence_prune_is_frozen() {
    rejects_shared_write("prune");
}
#[test]
fn shared_media_probe_publication_is_frozen() {
    rejects_shared_write("probe");
}
#[test]
fn shared_media_changed_nfo_is_frozen() {
    rejects_shared_write("nfo");
}

fn rejects_shared_write(action: &str) {
    let (tmp, store, frozen, ids) = setup();
    let media = ids[0].parse().unwrap();
    let root = tmp.path().join("ready");
    std::fs::create_dir_all(&root).unwrap();
    let ready = store
        .create_library(
            domain::MediaKind::Movie,
            "Ready",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let ready_id = ready.id.parse().unwrap();
    let mut file = store.file_coverage_for_media(media).unwrap()[0]
        .file
        .clone();
    file.file_id = LedgerId::new();
    file.library_id = ready_id;
    file.root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|r| r.library_id == ready.id)
        .unwrap()
        .id;
    file.path = root.join("movie.mkv");
    std::fs::write(&file.path, b"fixture").unwrap();
    let scope = WriteScope {
        library_id: ready_id,
        config_revision: 0,
    };
    store
        .publish_files(
            &scope,
            &[FilePublication {
                file: &file,
                units: &[UnitKey::WHOLE],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            0,
        )
        .unwrap();
    std::fs::write(
        root.join("movie.nfo"),
        "<movie><title>Changed NFO</title></movie>",
    )
    .unwrap();
    let nfo = store
        .plan_nfo_input_reconcile(&file.path)
        .unwrap()
        .unwrap()
        .observe();
    store
        .begin_library_integrity(frozen, true, "owner", 1)
        .unwrap();
    assert_eq!(
        store
            .advance_library_operation("owner", 2, 1)
            .unwrap()
            .state,
        "applying"
    );
    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    let before = snapshot(&db);
    let result = match action {
        "verify" | "prune" => store
            .apply_ledger_presence(
                &[LedgerPresenceObservation {
                    file: store.file_coverage_by_id(file.file_id).unwrap().unwrap(),
                    path_key: store
                        .file_coverage_by_id(file.file_id)
                        .unwrap()
                        .unwrap()
                        .path_key,
                    missing_at: store.missing_at_by_path().unwrap().values().next().copied(),
                    presence: FilePresence::Missing,
                }],
                if action == "verify" {
                    PresenceMode::Verify
                } else {
                    PresenceMode::Prune
                },
                3,
            )
            .map(|_| ()),
        "probe" => store
            .publish_probe_result(
                file.file_id,
                &ProbePublication {
                    expected: &file.version,
                    tracks_version: 1,
                    resolution: Some("2160p"),
                    codec: Some("HEVC"),
                    hdr: store::library_write::probe::Observed::NotObserved,
                    duration_ms: Some(100),
                    details: None,
                },
                3,
            )
            .map(|_| ()),
        "nfo" => store.apply_nfo_input_reconcile(nfo).map(|_| ()),
        _ => unreachable!(),
    };
    assert!(
        matches!(result, Err(store::StoreError::Protected(_))),
        "{action}: {result:?}"
    );
    assert_eq!(
        snapshot(&db),
        before,
        "{action} must preserve facts, projections, counters and staged members"
    );
}

fn snapshot(db: &rusqlite::Connection) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    [
        "library_files",
        "file_details",
        "library_metadata_inputs",
        "library_items",
        "library_units",
        "library_item_terms",
        "library_protocol_terms",
        "library_protocol_people",
        "user_library_items",
        "library_query_counts",
        "library_query_count_rebuild",
    ]
    .iter()
    .map(|table| {
        // Order by all columns, not `rowid`: the narrow composite-key tables are
        // WITHOUT ROWID (layout v3). Every column list ends with a unique key, so
        // this stays a deterministic, content-only ordering.
        let width = db
            .prepare(&format!("SELECT * FROM {table}"))
            .unwrap()
            .column_count();
        let order = (1..=width)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = db
            .prepare(&format!("SELECT * FROM {table} ORDER BY {order}"))
            .unwrap();
        stmt.query_map([], |row| {
            (0..width)
                .map(|n| row.get(n))
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
    })
    .collect()
}
