//! Full public detail allocation work, including all blocking phases and JSON.
#[path = "jellyfin_shared_coverage/allocations.rs"]
mod allocations;
#[allow(dead_code)]
#[path = "jellyfin_shared_coverage/fixture.rs"]
mod fixture;

use allocations::{MeasuredAllocator, Measurement};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::{Coverage, FetchMode, Subscribe, SubscribeId, Torrent};
use fixture::Fixture;
use serde_json::Value;
use tower::ServiceExt;

#[global_allocator]
static ALLOCATOR: MeasuredAllocator = MeasuredAllocator;

fn seed(f: &Fixture, episodes: u32, pending_count: u32) -> SubscribeId {
    let handle = f.state.store();
    let store = handle.as_ref();
    let id = SubscribeId::new();
    store
        .insert_subscribe(&Subscribe {
            id,
            user_id: f.user,
            media_id: f.media,
            coverage: Coverage::Tv {
                season: 1,
                episode_from: 1,
                episode_to: Some(episodes),
            },
            fetch_mode: FetchMode::Search,
            filter_id: store.list_filters().unwrap()[0].id,
            wash_cut: true,
            wash_cut_filter_id: None,
            keep_old_versions: false,
            full_season_pack: false,
            downloader_id: None,
            library_id: None,
            tracking_state: "active".into(),
            follow_future: false,
            search_interval_secs: 1800,
        })
        .unwrap();
    let pending: Vec<_> = (1..=pending_count)
        .map(|episode| {
            (
                80,
                store::PendingDownload {
                    torrent: Torrent {
                        site_id: domain::SiteId::new(),
                        title: format!("Example.S01E{episode:04}.1080p.WEB-DL.H264"),
                        enclosure: format!("fixture:{episode}"),
                        size_bytes: Some(1024),
                        seeders: Some(1),
                        free: false,
                        hr: false,
                        imdb_id: None,
                        id: None,
                        leechers: None,
                        snatched: None,
                        upload_time: None,
                        detail_url: None,
                        category: None,
                        poster_url: None,
                    },
                    release_override: None,
                    downloader_id: None,
                    submitted_at: Some(1000),
                },
            )
        })
        .collect();
    store.merge_pending(id, &pending).unwrap();
    id
}

fn measure(f: &Fixture, id: SubscribeId, rt: &tokio::runtime::Runtime, episodes: usize) -> usize {
    let app = api::router(f.state.clone());
    let request = Request::builder()
        .uri(format!("/api/v1/subscriptions/{id}"))
        .header("authorization", "Bearer coverage-secret")
        .body(Body::empty())
        .unwrap();
    let measurement = Measurement::start();
    let (status, bytes) = rt.block_on(async {
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        (
            status,
            axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
    });
    let allocated = measurement.finish();
    assert_eq!(status, StatusCode::OK);
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    let wanted = body["data"]["wanted"].as_array().unwrap();
    assert_eq!(wanted.len(), episodes);
    assert_eq!(wanted.first().unwrap()["episode_number"], 1);
    assert_eq!(wanted.last().unwrap()["episode_number"], episodes);
    allocated
}

#[test]
fn wanted_detail_work_grows_with_response_instead_of_reparsing_every_pending_per_unit() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut previous = None;
    for (episodes, pending) in [(100, 100), (1000, 1000), (1000, 5000)] {
        let f = Fixture::new(pending);
        let id = seed(&f, episodes as u32, pending as u32);
        measure(&f, id, &runtime, episodes as usize);
        let bytes = measure(&f, id, &runtime, episodes as usize);
        eprintln!("episodes={episodes} pending={pending} allocated_bytes={bytes}");
        if let Some((prior_pending, prior_bytes)) = previous {
            let factor = if prior_pending == 100 { 15 } else { 8 };
            assert!(
                bytes <= factor * prior_bytes,
                "superlinear wanted work: {previous:?} -> {bytes}"
            );
        }
        previous = Some((pending, bytes));
    }
    overlapping_ranges_keep_first_pending_precedence(&runtime);
}

fn fixture_pending(title: &str, enclosure: &str, id: &str) -> store::PendingDownload {
    store::PendingDownload {
        torrent: Torrent {
            site_id: domain::SiteId::new(),
            title: title.into(),
            enclosure: enclosure.into(),
            size_bytes: Some(1024),
            seeders: Some(1),
            free: false,
            hr: false,
            imdb_id: None,
            id: Some(id.into()),
            leechers: None,
            snatched: None,
            upload_time: None,
            detail_url: None,
            category: None,
            poster_url: None,
        },
        release_override: None,
        downloader_id: None,
        submitted_at: Some(1000),
    }
}

fn detail(f: &Fixture, id: SubscribeId, runtime: &tokio::runtime::Runtime) -> Value {
    runtime.block_on(async {
        let response = api::router(f.state.clone())
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/subscriptions/{id}"))
                    .header("authorization", "Bearer coverage-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap()
    })
}

fn overlapping_ranges_keep_first_pending_precedence(runtime: &tokio::runtime::Runtime) {
    let f = Fixture::new(3);
    let id = seed(&f, 3, 0);
    {
        let handle = f.state.store();
        let store = handle.as_ref();
        store
            .merge_pending(
                id,
                &[
                    (
                        100,
                        fixture_pending(
                            "Example.S01E01-E999999.1080p.WEB-DL",
                            "fixture:range",
                            "range-first",
                        ),
                    ),
                    (
                        50,
                        fixture_pending(
                            "Example.S01E02.2160p.WEB-DL",
                            "fixture:single",
                            "single-later",
                        ),
                    ),
                ],
            )
            .unwrap();
    }
    let body = detail(&f, id, runtime);
    let units = body["data"]["wanted"].as_array().unwrap();
    assert_eq!(
        units.len(),
        3,
        "a large pending span must not expand requested coverage"
    );
    assert!(units.iter().all(|unit| unit["status"] == "grabbed"));
    assert!(
        units
            .iter()
            .all(|unit| unit["resource_timing"]["torrent_id"] == "range-first")
    );
    {
        let handle = f.state.store();
        let store = handle.as_ref();
        store.delete_pending(id, "fixture:range").unwrap();
        store.delete_pending(id, "fixture:single").unwrap();
        store
            .merge_pending(
                id,
                &[(
                    100,
                    fixture_pending("Example.S01.Complete.1080p", "fixture:pack", "pack"),
                )],
            )
            .unwrap();
    }
    let body = detail(&f, id, runtime);
    assert!(
        body["data"]["wanted"]
            .as_array()
            .unwrap()
            .iter()
            .all(|unit| unit["status"] == "wanted")
    );
}
