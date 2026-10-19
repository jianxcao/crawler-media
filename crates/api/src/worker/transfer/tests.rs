use std::sync::Arc;

use domain::{Media, Subscribe, SubscribeId};

use crate::management::ApiState;

use super::*;

struct EmptyFetcher;

impl indexer::Fetcher for EmptyFetcher {
    fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        Err(indexer::IndexerError::Fetch("unexpected request".into()))
    }
}

fn fixture() -> (tempfile::TempDir, ApiState, Media, Subscribe) {
    let tmp = tempfile::tempdir().unwrap();
    let state = ApiState::new(
        crate::Store::open(tmp.path().join("data")).unwrap(),
        "test-token".into(), indexer::ProfileSet::load(None).unwrap(),
        Arc::new(EmptyFetcher),
        Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
        tmp.path().join("library"),
    ).unwrap();
    let media = Media {
        id: domain::MediaId::new(), kind: domain::MediaKind::Movie,
        title: "The Matrix".into(), year: Some(1999), original_title: None, aliases: Vec::new(),
        tmdb_id: None, douban_id: None, tvdb_id: None, bangumi_id: None, anilist_id: None,
    };
    let filter = domain::Filter {
        id: domain::FilterId::new(), name: "transfer".into(), atoms: Vec::new(),
        keep_old_versions: false,
    };
    let subscribe = Subscribe {
        id: SubscribeId::new(), user_id: domain::UserId::new(), media_id: media.id,
        coverage: domain::Coverage::Movie, fetch_mode: domain::FetchMode::Search,
        filter_id: filter.id, wash_cut: false, keep_old_versions: false,
        wash_cut_filter_id: None, full_season_pack: false, downloader_id: None,
        library_id: None, tracking_state: "active".into(), follow_future: false,
        search_interval_secs: 1800,
    };
    {
        let store = state.store.lock();
        store.insert_media(&media).unwrap();
        store.insert_filter(&filter).unwrap();
        store.insert_subscribe(&subscribe).unwrap();
    }
    (tmp, state, media, subscribe)
}

/// The `FileCoverage` a collected Transfer file carries: the plan's real
/// destination identity plus the source path and mode of this round.
fn candidate(
    media: &Media,
    plan: &TransferPlan,
    source: &std::path::Path,
    mode: library::TransferMode,
) -> domain::FileCoverage {
    let file = domain::LibraryFile {
        file_id: domain::LedgerId::new(),
        library_id: plan.library_id.parse().unwrap(),
        root_id: plan.root_id.clone(),
        media_id: media.id,
        kind: media.kind,
        path: plan.root.join("The Matrix.mkv"),
        version: domain::FileVersion { revision: 1, source_version: "v1".into() },
        presence: domain::FilePresence::Present,
        publication: domain::FilePublication::Pending,
        size_bytes: Some(1024),
        stat_known: true,
        created_ns: None,
        modified_ns: None,
        source_digest: None,
        source_path: Some(source.to_path_buf()),
        transfer_mode: Some(mode.as_str().to_string()),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: domain::QualitySource::Release,
        confidence: domain::Confidence::High,
        filter_score: Some(100),
        release_quality: None,
        duration_ms: None,
        tracks_version: 0,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    };
    domain::FileCoverage { file, units: vec![domain::UnitKey::WHOLE] }
}

fn recorder<'a>(
    state: &'a ApiState,
    subscribe: &'a Subscribe,
    lease: &'a crate::library_lifecycle::LibraryWorkLease,
    plan: &'a TransferPlan,
) -> PlannedPublicationRecorder<'a> {
    PlannedPublicationRecorder::new(
        PublicationRecorder {
            state,
            subscribe,
            lease,
            fingerprint: state.store.lock().read_fingerprint(&plan.library_id, subscribe.media_id).unwrap(),
        },
        plan,
    )
}

#[test]
fn changed_root_is_rejected_before_reservation_even_with_a_new_facts_snapshot() {
    let (tmp, state, media, subscribe) = fixture();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let lease = state.acquire_library_work(&plan.library_id).unwrap();
    state.store.lock().set_library_root(media.kind, tmp.path().join("new-root").to_str().unwrap()).unwrap();
    let fingerprint = state.store.lock().read_fingerprint(&plan.library_id, media.id).unwrap();
    assert_eq!(fingerprint, plan.fingerprint, "root writes evade the scoped fingerprint");
    let recorder = PlannedPublicationRecorder::new(
        PublicationRecorder { state: &state, subscribe: &subscribe, lease: &lease, fingerprint },
        &plan,
    );
    let source = tmp.path().join("source.mkv");
    let candidate_cov = candidate(&media, &plan, &source, library::TransferMode::Copy);
    let result = subscribe::TransferRecorder::prepare(
        &recorder, &candidate_cov, &source, library::TransferMode::Copy,
    );
    assert!(result.is_err(), "obsolete root must not be reserved: {result:?}");
    assert!(state.store.lock().list_ledger().unwrap().is_empty());
    assert!(!candidate_cov.file.path.exists());
}

#[test]
fn changed_transfer_setting_is_rejected_before_reservation() {
    let (tmp, state, media, subscribe) = fixture();
    state.store.lock().set_transfer_mode("copy").unwrap();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let lease = state.acquire_library_work(&plan.library_id).unwrap();
    state.store.lock().set_transfer_mode("move").unwrap();
    let fingerprint = state.store.lock().read_fingerprint(&plan.library_id, media.id).unwrap();
    assert_eq!(fingerprint, plan.fingerprint, "settings evade the scoped fingerprint");
    let recorder = PlannedPublicationRecorder::new(
        PublicationRecorder { state: &state, subscribe: &subscribe, lease: &lease, fingerprint },
        &plan,
    );
    let source = tmp.path().join("source.mkv");
    let candidate_cov = candidate(&media, &plan, &source, library::TransferMode::Copy);
    let result = subscribe::TransferRecorder::prepare(
        &recorder, &candidate_cov, &source, library::TransferMode::Copy,
    );
    assert!(result.is_err(), "obsolete mode must not be reserved: {result:?}");
    assert!(state.store.lock().list_ledger().unwrap().is_empty());
    assert!(!candidate_cov.file.path.exists());
}

#[test]
fn reservation_and_publication_keep_the_destination_identity() {
    let (tmp, state, media, subscribe) = fixture();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let lease = state.acquire_library_work(&plan.library_id).unwrap();
    let recorder = recorder(&state, &subscribe, &lease, &plan);
    let source = tmp.path().join("downloads/The Matrix.mkv");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, b"matrix").unwrap();

    let reserved = subscribe::TransferRecorder::prepare(
        &recorder,
        &candidate(&media, &plan, &source, library::TransferMode::Copy),
        &source,
        library::TransferMode::Copy,
    )
    .unwrap();
    let destination = reserved.file.path.clone();
    assert!(
        state
            .store
            .lock()
            .transfer_pending(&destination.display().to_string())
            .unwrap(),
        "the reservation is a pending row at the destination"
    );

    std::fs::write(&destination, b"matrix").unwrap();
    let mut published = reserved.clone();
    published.file.publication = domain::FilePublication::Published;
    subscribe::TransferRecorder::published(&recorder, &published).unwrap();

    let library_path = state
        .store
        .lock()
        .db_paths()
        .into_iter()
        .find(|path| path.ends_with("library.db"))
        .unwrap();
    let conn = rusqlite::Connection::open(library_path).unwrap();
    let (root_id, source_path, transfer_mode, publication): (
        String,
        Option<String>,
        Option<String>,
        String,
    ) = conn
        .query_row(
            "SELECT root_id, source_path, transfer_mode, publication_state
             FROM library_files WHERE file_id = ?1",
            [published.file.file_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(root_id, plan.root_id, "the real destination root is persisted");
    assert_eq!(source_path.as_deref(), source.to_str());
    assert_eq!(
        transfer_mode.as_deref(),
        Some("copy"),
        "the reservation's transfer mode survives publication"
    );
    assert_eq!(publication, "published");
}
