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
        "test-token".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(EmptyFetcher),
        Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
        tmp.path().join("library"),
    )
    .unwrap();
    let media = Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Movie,
        title: "The Matrix".into(),
        year: Some(1999),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let filter = domain::Filter {
        id: domain::FilterId::new(),
        name: "transfer".into(),
        atoms: Vec::new(),
        keep_old_versions: false,
    };
    let subscribe = Subscribe {
        id: SubscribeId::new(),
        user_id: domain::UserId::new(),
        media_id: media.id,
        coverage: domain::Coverage::Movie,
        fetch_mode: domain::FetchMode::Search,
        filter_id: filter.id,
        wash_cut: false,
        keep_old_versions: false,
        wash_cut_filter_id: None,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
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
        version: domain::FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
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
    domain::FileCoverage {
        file,
        units: vec![domain::UnitKey::WHOLE],
    }
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
            fingerprint: state
                .store
                .lock()
                .read_fingerprint(&plan.library_id, subscribe.media_id)
                .unwrap(),
        },
        plan,
    )
}

#[test]
fn changed_root_is_rejected_before_reservation_even_with_a_new_facts_snapshot() {
    let (tmp, state, media, subscribe) = fixture();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let lease = state.acquire_library_work(&plan.library_id).unwrap();
    state
        .store
        .lock()
        .set_library_root(media.kind, tmp.path().join("new-root").to_str().unwrap())
        .unwrap();
    let fingerprint = state
        .store
        .lock()
        .read_fingerprint(&plan.library_id, media.id)
        .unwrap();
    assert_eq!(
        fingerprint, plan.fingerprint,
        "root writes evade the scoped fingerprint"
    );
    let recorder = PlannedPublicationRecorder::new(
        PublicationRecorder {
            state: &state,
            subscribe: &subscribe,
            lease: &lease,
            fingerprint,
        },
        &plan,
    );
    let source = tmp.path().join("source.mkv");
    let candidate_cov = candidate(&media, &plan, &source, library::TransferMode::Copy);
    let result = subscribe::TransferRecorder::prepare(
        &recorder,
        &candidate_cov,
        &source,
        library::TransferMode::Copy,
    );
    assert!(
        result.is_err(),
        "obsolete root must not be reserved: {result:?}"
    );
    assert!(state.store.lock().list_file_coverage().unwrap().is_empty());
    assert!(!candidate_cov.file.path.exists());
}

#[test]
fn changed_transfer_setting_is_rejected_before_reservation() {
    let (tmp, state, media, subscribe) = fixture();
    state.store.lock().set_transfer_mode("copy").unwrap();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let lease = state.acquire_library_work(&plan.library_id).unwrap();
    state.store.lock().set_transfer_mode("move").unwrap();
    let fingerprint = state
        .store
        .lock()
        .read_fingerprint(&plan.library_id, media.id)
        .unwrap();
    assert_eq!(
        fingerprint, plan.fingerprint,
        "settings evade the scoped fingerprint"
    );
    let recorder = PlannedPublicationRecorder::new(
        PublicationRecorder {
            state: &state,
            subscribe: &subscribe,
            lease: &lease,
            fingerprint,
        },
        &plan,
    );
    let source = tmp.path().join("source.mkv");
    let candidate_cov = candidate(&media, &plan, &source, library::TransferMode::Copy);
    let result = subscribe::TransferRecorder::prepare(
        &recorder,
        &candidate_cov,
        &source,
        library::TransferMode::Copy,
    );
    assert!(
        result.is_err(),
        "obsolete mode must not be reserved: {result:?}"
    );
    assert!(state.store.lock().list_file_coverage().unwrap().is_empty());
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
    assert_eq!(
        root_id, plan.root_id,
        "the real destination root is persisted"
    );
    assert_eq!(source_path.as_deref(), source.to_str());
    assert_eq!(
        transfer_mode.as_deref(),
        Some("copy"),
        "the reservation's transfer mode survives publication"
    );
    assert_eq!(publication, "published");
}

#[test]
fn restarted_pending_reservation_reuses_file_identity_and_version() {
    let (tmp, state, media, subscribe) = fixture();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let lease = state.acquire_library_work(&plan.library_id).unwrap();
    let first_recorder = recorder(&state, &subscribe, &lease, &plan);
    let source = tmp.path().join("downloads/The Matrix.mkv");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, b"matrix").unwrap();
    let candidate = candidate(&media, &plan, &source, library::TransferMode::Copy);
    let first = subscribe::TransferRecorder::prepare(
        &first_recorder,
        &candidate,
        &source,
        library::TransferMode::Copy,
    )
    .unwrap();
    let file_id = first.file.file_id;
    drop(first_recorder);
    drop(lease);
    drop(state);

    let state = ApiState::new(
        crate::Store::open(tmp.path().join("data")).unwrap(),
        "test-token".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(EmptyFetcher),
        Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
        tmp.path().join("library"),
    )
    .unwrap();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let lease = state.acquire_library_work(&plan.library_id).unwrap();
    let restarted = recorder(&state, &subscribe, &lease, &plan);
    let recovered = subscribe::TransferRecorder::prepare(
        &restarted,
        &candidate,
        &source,
        library::TransferMode::Copy,
    )
    .unwrap();
    assert_eq!(recovered.file.file_id, file_id);
    assert_eq!(recovered.file.version, first.file.version);
    assert_eq!(
        recovered.file.source_path.as_deref(),
        Some(source.as_path())
    );
    assert_eq!(recovered.file.transfer_mode.as_deref(), Some("copy"));

    std::fs::write(&recovered.file.path, b"matrix").unwrap();
    let mut published = recovered.clone();
    published.file.publication = domain::FilePublication::Published;
    subscribe::TransferRecorder::published(&restarted, &published).unwrap();
    let store = state.store.lock();
    assert_eq!(
        store
            .file_coverage_by_path(&recovered.file.path.display().to_string())
            .unwrap()
            .unwrap()
            .file
            .file_id,
        file_id
    );
    assert_eq!(
        store
            .library_file_page(plan.library_id.parse().unwrap(), media.id, None, 100)
            .unwrap()
            .files
            .len(),
        1
    );
}

/// `StoreError` carries no `PartialEq`: assert the reservation guard's variant
/// and message instead of widening the public error enum for a test.
fn assert_reservation_changed(error: store::StoreError) {
    match error {
        store::StoreError::Protected(message) => {
            assert_eq!(message, "Transfer reservation changed or was detached")
        }
        other => panic!("expected a Protected reservation error, got {other:?}"),
    }
}

fn write_scope(plan: &TransferPlan) -> store::library_write::WriteScope {
    store::library_write::WriteScope {
        library_id: plan.library_id.parse().unwrap(),
        config_revision: plan.config_revision,
    }
}

fn reserve(
    state: &ApiState,
    plan: &TransferPlan,
    coverage: &domain::FileCoverage,
    source: &std::path::Path,
) -> domain::FileCoverage {
    state
        .store
        .lock()
        .reserve_transfer(
            &write_scope(plan),
            coverage,
            source,
            library::TransferMode::Copy,
            0,
        )
        .unwrap()
}

/// The outcome the collector builds for one reserved file, carrying the
/// confirmed quality verdict (not the reservation's placeholder facts).
fn outcome_of(coverage: domain::FileCoverage, source: &std::path::Path) -> subscribe::RunOutcome {
    subscribe::RunOutcome {
        facts: subscribe::SubscribeFacts::default(),
        file_sources: vec![domain::FileSource {
            file_id: coverage.file.file_id,
            destination: coverage.file.path.clone(),
            source: source.to_path_buf(),
        }],
        files: vec![coverage],
        removed_paths: Vec::new(),
        completed: true,
        transferred_enclosures: Vec::new(),
        rejected_enclosures: Vec::new(),
        awaiting_probe_enclosures: Vec::new(),
        collection_errors: Vec::new(),
        submission_errors: Vec::new(),
        torrents_added: Vec::new(),
    }
}

/// Prepare a reserved destination plus the source file and sidecar artwork, and
/// return the reservation and the confirmed coverage of that file.
fn reserved_round(
    state: &ApiState,
    media: &Media,
    plan: &TransferPlan,
    tmp: &tempfile::TempDir,
) -> (domain::FileCoverage, domain::FileCoverage) {
    let source = tmp.path().join("downloads/The Matrix.mkv");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, b"matrix").unwrap();
    let mut placeholder = candidate(media, plan, &source, library::TransferMode::Copy);
    placeholder.units = vec![domain::UnitKey::new(1, 1)];
    let reserved = reserve(state, plan, &placeholder, &source);
    std::fs::write(&reserved.file.path, b"matrix").unwrap();
    std::fs::write(reserved.file.path.with_file_name("poster.jpg"), b"jpg").unwrap();

    let mut confirmed = reserved.clone();
    confirmed.file.presence = domain::FilePresence::Present;
    confirmed.file.publication = domain::FilePublication::Published;
    confirmed.file.resolution = Some("1080p".into());
    confirmed.file.codec = Some("H264".into());
    confirmed.file.hdr = Some("HDR10".into());
    confirmed.file.quality_source = domain::QualitySource::Probe;
    confirmed.file.confidence = domain::Confidence::High;
    confirmed.file.filter_score = Some(77);
    (reserved, confirmed)
}

/// The confirmed verdict, the observed transfer mode, the source path and the
/// sidecar artwork of a reserved Transfer file all reach the ledger file, under
/// the reservation's identity and without moving its version.
#[test]
fn publication_writes_confirmed_facts_mode_source_and_artwork() {
    let (tmp, state, media, subscribe) = fixture();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let (reserved, confirmed) = reserved_round(&state, &media, &plan, &tmp);
    let source = tmp.path().join("downloads/The Matrix.mkv");
    let outcome = outcome_of(confirmed, &source);

    let published = publish_transfer_file(
        &state.store.lock(),
        &plan.library_id,
        plan.config_revision,
        &outcome.files[0],
        Some(&source),
        "hardlink",
        None,
    )
    .unwrap();

    assert_eq!(published.file_id, reserved.file.file_id);
    assert_eq!(published.transfer_mode.as_deref(), Some("hardlink"));
    assert!(
        published.version == reserved.file.version,
        "the reservation's version is kept"
    );
    let store = state.store.lock();
    let files = store
        .library_file_page(plan.library_id.parse().unwrap(), media.id, None, 100)
        .unwrap()
        .files;
    assert_eq!(files.len(), 1);
    let file = &files[0];
    assert_eq!(file.file_id, reserved.file.file_id);
    assert!(
        file.version == reserved.file.version,
        "publication never bumps the reserved version"
    );
    assert_eq!(file.resolution.as_deref(), Some("1080p"));
    assert_eq!(file.codec.as_deref(), Some("H264"));
    assert_eq!(file.hdr.as_deref(), Some("HDR10"));
    assert_eq!(file.quality_source, domain::QualitySource::Probe);
    assert_eq!(file.confidence, domain::Confidence::High);
    assert_eq!(file.filter_score, Some(77));
    assert_eq!(file.transfer_mode.as_deref(), Some("hardlink"));
    assert_eq!(file.source_path.as_deref(), Some(source.as_path()));
    assert_eq!(
        file.poster_path,
        Some(file.path.with_file_name("poster.jpg"))
    );
    assert_eq!(file.root_id, plan.root_id);
    assert_eq!(file.publication, domain::FilePublication::Published);
    assert_eq!(
        store.get_file_units(reserved.file.file_id).unwrap(),
        vec![domain::UnitKey::new(1, 1)]
    );
}

/// A file whose committed version moved after the reservation is refused instead
/// of being published over the newer one.
#[test]
fn publication_refuses_a_reservation_whose_version_moved() {
    let (tmp, state, media, subscribe) = fixture();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let (reserved, confirmed) = reserved_round(&state, &media, &plan, &tmp);
    let source = tmp.path().join("downloads/The Matrix.mkv");
    let mut bumped = reserved.file.clone();
    bumped.version = domain::FileVersion {
        revision: 1,
        source_version: "v2".into(),
    };
    state
        .store
        .lock()
        .publish_files(
            &write_scope(&plan),
            &[store::library_write::FilePublication {
                file: &bumped,
                units: &[domain::UnitKey::new(1, 1)],
                expected_version: Some(&reserved.file.version),
                nfo_observation: None,
                probe_required: false,
            }],
            store::unix_now() * 1000,
        )
        .unwrap();
    assert_eq!(
        state
            .store
            .lock()
            .get_file_version(reserved.file.file_id)
            .unwrap()
            .unwrap()
            .source_version,
        "v2"
    );

    let outcome = outcome_of(confirmed, &source);
    let error = publish_transfer_file(
        &state.store.lock(),
        &plan.library_id,
        plan.config_revision,
        &outcome.files[0],
        Some(&source),
        "hardlink",
        None,
    )
    .unwrap_err();
    assert_reservation_changed(error);
}

/// A destination with no reservation row can never be published: nothing may
/// recreate an identity the reservation did not own.
#[test]
fn publication_refuses_a_destination_without_a_reservation() {
    let (tmp, state, media, subscribe) = fixture();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let source = tmp.path().join("downloads/The Matrix.mkv");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, b"matrix").unwrap();
    let coverage = candidate(&media, &plan, &source, library::TransferMode::Copy);
    let outcome = outcome_of(coverage, &source);

    let error = publish_transfer_file(
        &state.store.lock(),
        &plan.library_id,
        plan.config_revision,
        &outcome.files[0],
        Some(&source),
        "copy",
        None,
    )
    .unwrap_err();
    assert_reservation_changed(error);
    assert!(state.store.lock().list_file_coverage().unwrap().is_empty());
}

/// The collected coverage widens a span the reservation recorded as one unit:
/// publication must add the carried units instead of dropping them.
#[test]
fn publication_keeps_carried_units_when_the_row_has_none() {
    let (tmp, state, media, subscribe) = fixture();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let (reserved, confirmed) = reserved_round(&state, &media, &plan, &tmp);
    let source = tmp.path().join("downloads/The Matrix.mkv");
    // A reservation without units (the shape an old reservation could leave behind).
    state
        .store
        .lock()
        .publish_files(
            &write_scope(&plan),
            &[store::library_write::FilePublication {
                file: &reserved.file,
                units: &[],
                expected_version: Some(&reserved.file.version),
                nfo_observation: None,
                probe_required: false,
            }],
            store::unix_now() * 1000,
        )
        .unwrap();
    assert!(
        state
            .store
            .lock()
            .get_file_units(reserved.file.file_id)
            .unwrap()
            .is_empty()
    );

    let mut widened = confirmed;
    widened.units = vec![domain::UnitKey::new(2, 3), domain::UnitKey::new(2, 4)];
    let outcome = outcome_of(widened, &source);
    publish_transfer_file(
        &state.store.lock(),
        &plan.library_id,
        plan.config_revision,
        &outcome.files[0],
        Some(&source),
        "copy",
        None,
    )
    .unwrap();
    assert_eq!(
        state
            .store
            .lock()
            .get_file_units(reserved.file.file_id)
            .unwrap(),
        vec![domain::UnitKey::new(2, 3), domain::UnitKey::new(2, 4)]
    );
}

/// No-compat cutover: an unknown coverage is published as unknown, never
/// inferred as the whole work.
#[test]
fn publication_keeps_unknown_coverage_unknown() {
    let (tmp, state, media, subscribe) = fixture();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let (reserved, mut confirmed) = reserved_round(&state, &media, &plan, &tmp);
    let source = tmp.path().join("downloads/The Matrix.mkv");
    state
        .store
        .lock()
        .publish_files(
            &write_scope(&plan),
            &[store::library_write::FilePublication {
                file: &reserved.file,
                units: &[],
                expected_version: Some(&reserved.file.version),
                nfo_observation: None,
                probe_required: false,
            }],
            store::unix_now() * 1000,
        )
        .unwrap();
    confirmed.units = Vec::new();
    let outcome = outcome_of(confirmed, &source);
    publish_transfer_file(
        &state.store.lock(),
        &plan.library_id,
        plan.config_revision,
        &outcome.files[0],
        Some(&source),
        "copy",
        None,
    )
    .unwrap();
    let units = state
        .store
        .lock()
        .get_file_units(reserved.file.file_id)
        .unwrap();
    assert!(units.is_empty(), "unknown coverage became {units:?}");
}

/// The reservation is the source of truth for coverage: a narrower carried
/// coverage never shrinks the units already committed with the row.
#[test]
fn publication_prefers_reserved_units_over_carried_ones() {
    let (tmp, state, media, subscribe) = fixture();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let (reserved, mut confirmed) = reserved_round(&state, &media, &plan, &tmp);
    let source = tmp.path().join("downloads/The Matrix.mkv");
    confirmed.units = vec![domain::UnitKey::WHOLE];
    let outcome = outcome_of(confirmed, &source);

    publish_transfer_file(
        &state.store.lock(),
        &plan.library_id,
        plan.config_revision,
        &outcome.files[0],
        Some(&source),
        "copy",
        None,
    )
    .unwrap();
    assert_eq!(
        state
            .store
            .lock()
            .get_file_units(reserved.file.file_id)
            .unwrap(),
        vec![domain::UnitKey::new(1, 1)]
    );
}

/// A restarted round rebuilds its candidates with fresh UUIDs. Recovering the
/// durable pending A must record A's persisted identity as this round's own
/// write, so publishing A does not make the next file B look externally changed.
#[test]
fn restarted_pending_a_with_fresh_candidate_does_not_block_b() {
    let (tmp, state, media, subscribe) = fixture();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let source = tmp.path().join("downloads/The Matrix.mkv");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, b"matrix").unwrap();
    let durable_a_id = {
        let lease = state.acquire_library_work(&plan.library_id).unwrap();
        let first = recorder(&state, &subscribe, &lease, &plan);
        let a = candidate(&media, &plan, &source, library::TransferMode::Copy);
        subscribe::TransferRecorder::prepare(&first, &a, &source, library::TransferMode::Copy)
            .unwrap()
            .file
            .file_id
    };

    // Restart: the round re-plans against the durable pending A.
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let lease = state.acquire_library_work(&plan.library_id).unwrap();
    let restarted = recorder(&state, &subscribe, &lease, &plan);
    let fresh_a = candidate(&media, &plan, &source, library::TransferMode::Copy);
    let fresh_candidate_id = fresh_a.file.file_id;
    let recovered = subscribe::TransferRecorder::prepare(
        &restarted,
        &fresh_a,
        &source,
        library::TransferMode::Copy,
    )
    .unwrap();
    assert_eq!(recovered.file.file_id, durable_a_id);
    assert_ne!(recovered.file.file_id, fresh_candidate_id);

    std::fs::write(&recovered.file.path, b"matrix").unwrap();
    let mut published = recovered.clone();
    published.file.publication = domain::FilePublication::Published;
    subscribe::TransferRecorder::published(&restarted, &published).unwrap();

    let source_b = tmp.path().join("downloads/The Matrix B.mkv");
    std::fs::write(&source_b, b"matrix-b").unwrap();
    let mut b = candidate(&media, &plan, &source_b, library::TransferMode::Copy);
    b.file.path = plan.root.join("The Matrix B.mkv");
    let prepare_b = subscribe::TransferRecorder::prepare(
        &restarted,
        &b,
        &source_b,
        library::TransferMode::Copy,
    );
    assert!(
        prepare_b.is_ok(),
        "B must not see A's own publication as external: {prepare_b:?}"
    );
}
