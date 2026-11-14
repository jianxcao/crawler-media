use std::sync::{Arc, mpsc};
use std::time::Duration;

use axum::{body::Body, http::Request};
use domain::{
    Coverage, FetchMode, Filter, FilterId, Media, MediaId, MediaKind, Subscribe, SubscribeId,
};
use tower::ServiceExt;

struct Offline;
impl indexer::Fetcher for Offline {
    fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        Err(indexer::IndexerError::Fetch("offline".into()))
    }
}

#[test]
fn tick_does_not_abort_active_subscribe_creation() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let state = crate::ApiState::new(
        crate::Store::open(temp.path().join("data")).unwrap(),
        "test".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(Offline),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
    )
    .unwrap();
    let user = state.store().user_by_token("test").unwrap().unwrap().id;
    let filter = Filter::new(FilterId::new(), "Unrestricted", vec![]);
    state.store().insert_filter(&filter).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Active creation".into(),
        year: None,
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let subscribe = Subscribe {
        id: SubscribeId::new(),
        user_id: user,
        media_id: media.id,
        coverage: Coverage::Movie,
        fetch_mode: FetchMode::Search,
        filter_id: filter.id,
        wash_cut: false,
        wash_cut_filter_id: None,
        keep_old_versions: false,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    };
    let id = subscribe.id;
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let create_state = state.clone();
    let creator = std::thread::spawn(move || {
        let first = std::cell::Cell::new(true);
        let observer_store = create_state.store();
        observer_store.with_writer_diagnostics(
            move |sample| {
                if sample.database == store::Db::Subscribe && first.replace(false) {
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                }
            },
            || super::create(&create_state, media, subscribe, None),
        )
    });
    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let response = runtime
        .block_on(
            crate::router(state.clone()).oneshot(
                Request::post("/api/v1/jobs/tick?now=1")
                    .header("authorization", "Bearer test")
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .unwrap();
    assert!(
        response.status().is_success()
            || response.status() == axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
    release_tx.send(()).unwrap();
    let (media, created) = creator.join().unwrap().unwrap();
    assert_eq!(created.id, id);
    assert_eq!(state.store().get_subscribe(id).unwrap(), Some(created));
    assert_eq!(state.store().get_media(media.id).unwrap(), Some(media));
    assert!(
        state
            .jobs()
            .acquire()
            .unwrap()
            .def_for_identity(jobs::JobIdentity::subscribe_search(id))
            .unwrap()
            .is_some()
    );
}

#[test]
fn tick_skips_active_candidate_and_recovers_other_candidates() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let state = crate::ApiState::new(
        crate::Store::open(temp.path().join("data")).unwrap(),
        "test".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(Offline),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
    )
    .unwrap();
    let user = state.store().user_by_token("test").unwrap().unwrap().id;
    let mut media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Recovery".into(),
        year: None,
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    state.store().insert_media(&media).unwrap();
    let mut subscribe = Subscribe {
        id: SubscribeId::new(),
        user_id: user,
        media_id: media.id,
        coverage: Coverage::Movie,
        fetch_mode: FetchMode::Search,
        filter_id: FilterId::new(),
        wash_cut: false,
        wash_cut_filter_id: None,
        keep_old_versions: false,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    };
    let active = state.store().begin_subscribe_creation(&subscribe).unwrap();
    let guard = state.subscribe_guard(subscribe.id);
    let held = guard.lock();
    media.id = MediaId::new();
    state.store().insert_media(&media).unwrap();
    subscribe.id = SubscribeId::new();
    subscribe.media_id = media.id;
    let abandoned = state.store().begin_subscribe_creation(&subscribe).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let tick = || {
        runtime
            .block_on(
                crate::router(state.clone()).oneshot(
                    Request::post("/api/v1/jobs/tick?now=1")
                        .header("authorization", "Bearer test")
                        .body(Body::empty())
                        .unwrap(),
                ),
            )
            .unwrap()
    };
    assert!(tick().status().is_success());
    assert!(state.store().retain_subscribe_creation(&abandoned).is_err());
    drop(held);
    assert!(tick().status().is_success());
    assert!(state.store().retain_subscribe_creation(&active).is_err());
}
