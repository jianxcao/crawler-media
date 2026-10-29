use domain::{Confidence, LedgerId, MediaId, MediaKind, QualitySource};
use store::test_support::FixtureFile;
use store::{ProbeDispatchStage, ProbeJobUnitSpec, Store};

const METADATA_PENDING: usize = 100_000;
const FINGERPRINT_PENDING: usize = 100_000;

fn spec<'a>(ledger_id: &'a str) -> ProbeJobUnitSpec<'a> {
    ProbeJobUnitSpec {
        ledger_id,
        kind: "movie",
        force_fingerprint: false,
        reuse_fingerprint_cache: false,
        overwrite_markers: false,
        reuse_media_info_cache: true,
    }
}

fn durable_counts(store: &Store) -> (i64, i64) {
    let path = store
        .db_paths()
        .into_iter()
        .find(|path| path.ends_with("library.db"))
        .unwrap();
    let connection = rusqlite::Connection::open(path).unwrap();
    let pending = connection
        .query_row(
            "SELECT COUNT(*) FROM probe_job_units WHERE status IN ('queued','running')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let terminal = connection
        .query_row(
            "SELECT COUNT(*) FROM probe_job_units
             WHERE status IN ('succeeded','failed','cancelled','partial')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    (pending, terminal)
}

#[test]
fn large_pending_recovery_loads_only_dispatch_windows() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();

    // The first metadata ledger is real so a snapshot can resolve its row.
    let media_id = MediaId::new();
    store
        .insert_media(&domain::Media {
            id: media_id,
            kind: MediaKind::Movie,
            title: "Window Fixture".into(),
            year: None,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let row = FixtureFile {
        file_id: Some(LedgerId::new()),
        media_id,
        path: temp
            .path()
            .join("window.mkv")
            .to_string_lossy()
            .into_owned(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
        ..Default::default()
    };
    let library = store
        .create_library(
            MediaKind::Movie,
            "Window",
            &[temp.path().to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store
        .set_library_intro_settings(&library.id, true, true)
        .unwrap();
    store::test_support::publish_fixture_file(&store, &row).unwrap();

    let metadata_ids: Vec<String> =
        std::iter::once(row.file_id.expect("fixture file id").to_string())
            .chain((1..METADATA_PENDING).map(|index| format!("ledger-m-{index:06}")))
            .collect();
    let metadata_specs: Vec<_> = metadata_ids.iter().map(|id| spec(id)).collect();
    assert!(
        store
            .create_probe_job(
                "metadata-job",
                "media_probe",
                "media-1",
                None,
                "scope-metadata",
                &metadata_specs,
            )
            .unwrap()
    );

    let fingerprint_ids: Vec<String> = (0..FINGERPRINT_PENDING)
        .map(|index| format!("ledger-f-{index:06}"))
        .collect();
    record_fingerprint_owners(&store, &row, &fingerprint_ids, temp.path());
    let fingerprint_specs: Vec<_> = fingerprint_ids.iter().map(|id| spec(id)).collect();
    assert!(
        store
            .create_probe_job(
                "fingerprint-job",
                "fingerprint_probe",
                "media-1",
                None,
                "scope-fingerprint",
                &fingerprint_specs,
            )
            .unwrap()
    );

    let metadata_window = store
        .pending_probe_dispatch_ids(ProbeDispatchStage::Metadata, 64)
        .unwrap();
    assert_eq!(
        metadata_window.len(),
        64,
        "metadata window is bounded at 64"
    );
    assert!(
        metadata_window
            .iter()
            .all(|id| id.stage == ProbeDispatchStage::Metadata)
    );

    let fingerprint_window = store
        .pending_probe_dispatch_ids(ProbeDispatchStage::Fingerprint, 16)
        .unwrap();
    assert_eq!(
        fingerprint_window.len(),
        16,
        "fingerprint window is bounded at 16"
    );
    assert!(
        fingerprint_window
            .iter()
            .all(|id| id.stage == ProbeDispatchStage::Fingerprint)
    );

    let snapshot = store
        .load_probe_dispatch_snapshot(&metadata_window[0])
        .unwrap()
        .expect("a queued dispatch id loads exactly one snapshot");
    assert_eq!(snapshot.job.id, "metadata-job");
    assert_eq!(snapshot.unit.job_id, "metadata-job");
    assert_eq!(snapshot.unit.ledger_id, metadata_window[0].ledger_id);
    assert_eq!(snapshot.unit.stage, ProbeDispatchStage::Metadata);
    assert_eq!(
        snapshot.row.file.file_id.to_string(),
        metadata_window[0].ledger_id
    );
    assert_eq!(snapshot.source_version, "");

    // Metadata -> fingerprint is persisted, not guessed from memory.
    let first = metadata_window[0].clone();
    assert!(
        store
            .start_probe_unit(&first.job_id, &first.ledger_id)
            .unwrap()
    );
    assert!(
        store
            .advance_probe_unit_to_fingerprint(&first.job_id, &first.ledger_id)
            .unwrap()
    );
    // The transition is asserted on the persisted unit, not on its position in the next
    // window: dispatch order is job order (`crates/store/src/probe_dispatch.rs`), so with
    // 100k other pending fingerprint units this unit's slot in a 64-wide page says nothing
    // about whether the stage was persisted.
    let advanced = store
        .load_probe_dispatch_snapshot(&store::ProbeDispatchId {
            job_id: first.job_id.clone(),
            ledger_id: first.ledger_id.clone(),
            stage: ProbeDispatchStage::Fingerprint,
        })
        .unwrap()
        .expect("the advanced unit is still a dispatchable snapshot");
    assert_eq!(advanced.unit.stage, ProbeDispatchStage::Fingerprint);
    assert_eq!(advanced.unit.status, "queued");
    assert!(
        !store
            .pending_probe_dispatch_ids(ProbeDispatchStage::Metadata, 64)
            .unwrap()
            .iter()
            .any(|id| id.ledger_id == first.ledger_id)
    );
    assert!(
        store
            .start_probe_unit(&first.job_id, &first.ledger_id)
            .unwrap()
    );
    store
        .finish_probe_unit(&first.job_id, &first.ledger_id, true, None)
        .unwrap();

    let (pending, terminal) = durable_counts(&store);
    assert_eq!(
        pending + terminal,
        (METADATA_PENDING + FINGERPRINT_PENDING) as i64,
        "durable pending plus terminal must equal the original row count"
    );
    assert_eq!(terminal, 1);

    // The window queries never materialize more than the requested bound.
    assert!(
        store
            .pending_probe_dispatch_ids(ProbeDispatchStage::Metadata, 8)
            .unwrap()
            .len()
            <= 8
    );
    assert!(
        store
            .pending_probe_dispatch_ids(ProbeDispatchStage::Fingerprint, 4)
            .unwrap()
            .len()
            <= 4
    );
}

fn record_fingerprint_owners(
    store: &Store,
    template: &FixtureFile,
    ids: &[String],
    root: &std::path::Path,
) {
    let database = store
        .db_paths()
        .into_iter()
        .find(|path| path.ends_with("library.db"))
        .unwrap();
    let mut connection = rusqlite::Connection::open(database).unwrap();
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .unwrap();
    let tx = connection.transaction().unwrap();
    {
        let mut insert = tx
            .prepare(
                "INSERT INTO library_files (
                 file_id, library_id, root_id, media_id, kind, path_key, display_path,
                 presence_state, publication_state, file_revision, source_version,
                 quality_source, confidence)
             SELECT ?2, library_id, root_id, media_id, kind, ?3, ?3,
                    'present', 'published', 1, source_version, quality_source, confidence
             FROM library_files WHERE file_id=?1",
            )
            .unwrap();
        for id in ids {
            let path = root.join(format!("{id}.mkv"));
            insert
                .execute(rusqlite::params![
                    template.file_id.unwrap().to_string(),
                    id,
                    path.to_str().unwrap(),
                ])
                .unwrap();
        }
    }
    tx.commit().unwrap();
}
