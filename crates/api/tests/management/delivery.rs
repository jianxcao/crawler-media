use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use api::{ApiState, DownloaderEnv, DynamicDownloader, Store, router};
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use indexer::ProfileSet;
use parking_lot::Mutex;
use serde_json::json;
use tower::ServiceExt;

use super::common::*;

mod fixture;
use fixture::transmission_fixture;

#[tokio::test]
async fn manual_submit_applies_selected_save_path() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader.clone()));
    let response = app
        .oneshot(request(
            "POST",
            "/api/v1/downloaders/submit",
            Some("management-secret"),
            json!({"title":"Dune.2021.1080p", "download_url":"magnet:?xt=urn:btih:dune",
            "save_path":"/downloads/movies"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        json_body(response).await["data"]["save_path"],
        "/downloads/movies"
    );
    assert_eq!(
        downloader.destinations(),
        vec![Some("/downloads/movies".into())]
    );
}

#[tokio::test]
async fn auto_route_save_path_is_translated_into_downloader_namespace() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader.clone()));
    let downloader_id = domain::DownloaderId::new();
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .insert_downloader(&api::store::DownloaderRow {
            id: downloader_id,
            name: "default test downloader".into(),
            kind: "qbittorrent".into(),
            url: "http://127.0.0.1:1".into(),
            username: None,
            password: None,
            category: None,
            path_maps: vec![downloader::PathMap::new("/downloads", "/host/qb")],
            is_default: true,
            enabled: true,
        })
        .unwrap();
    store.set_default_downloader(downloader_id).unwrap();
    let response = app
        .oneshot(request(
            "POST",
            "/api/v1/downloaders/submit",
            Some("management-secret"),
            json!({"title":"Dune.2021.1080p", "download_url":"magnet:?xt=urn:btih:dune",
            "auto_route":true, "save_path":"/host/qb/movies/Dune (2021)"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        downloader.destinations(),
        vec![Some("/downloads/movies/Dune (2021)".into())]
    );
}

#[tokio::test]
async fn legacy_subscribe_run_keeps_the_site_downloader_for_transfer() {
    let tmp = tempfile::tempdir().unwrap();
    let (url, calls) = transmission_fixture();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let default = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, default.clone()));
    let downloader_id = domain::DownloaderId::new();
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .insert_downloader(&api::store::DownloaderRow {
            id: downloader_id,
            name: "site target".into(),
            kind: "transmission".into(),
            url,
            username: None,
            password: None,
            category: None,
            path_maps: Vec::new(),
            is_default: false,
            enabled: true,
        })
        .unwrap();
    let site = create_site(&app).await;
    let changed = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/sites/{}", site["id"].as_str().unwrap()),
            Some("management-secret"),
            json!({"downloader_id": downloader_id.to_string()}),
        ))
        .await
        .unwrap();
    assert_eq!(changed.status(), StatusCode::OK);
    let subscribe = create_subscribe(&app, "search").await;
    let run = app
        .oneshot(request(
            "POST",
            &format!(
                "/api/v1/subscriptions/{}/run",
                subscribe["id"].as_str().unwrap()
            ),
            Some("management-secret"),
            serde_json::Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(run.status(), StatusCode::OK);
    assert!(default.added().is_empty());
    let id = subscribe["id"].as_str().unwrap().parse().unwrap();
    let pending = store.load_pending(id).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].1.downloader_id, Some(downloader_id));
    assert!(calls.lock().iter().any(|call| call.contains("torrent-add")));
}

#[tokio::test]
async fn mteam_manual_delivery_resolves_signed_url_but_keeps_stable_pending_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([(
            "download",
            include_str!("../../../indexer/tests/fixtures/mteam_download.json"),
        )]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher.clone(), downloader.clone()));
    let site = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/sites",
                Some("management-secret"),
                json!({"name":"M-Team","url":"https://kp.m-team.cc/","profile_id":"mteam",
               "api_key":"test-key","enabled":true}),
            ))
            .await
            .unwrap(),
    )
    .await;
    let subscribe = create_subscribe(&app, "search").await;
    let stable_url = "https://kp.m-team.cc/api/torrent/download?id=99";
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/downloaders/submit",
            Some("management-secret"),
            json!({"site_id": site["data"]["id"], "torrent_id":"99", "title":"The.Matrix.1999.1080p",
               "download_url":stable_url, "subscribe_id":subscribe["id"]}),
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        json_body(response).await
    );
    assert_eq!(
        downloader.added()[0].enclosure,
        "https://api.m-team.cc/api/rss/dlv2?sign=signed-token"
    );
    let pending = Store::open(tmp.path().join("data"))
        .unwrap()
        .list_all_pending_routed()
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].2.torrent.enclosure, stable_url);
    assert!(
        fetcher
            .requests
            .lock()
            .iter()
            .any(|key| key.starts_with("download:"))
    );
}

#[tokio::test]
async fn explicitly_selected_downloader_owns_pending_task_and_its_removal() {
    let tmp = tempfile::tempdir().unwrap();
    let (url, calls) = transmission_fixture();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let default = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, default.clone()));
    let id = domain::DownloaderId::new();
    Store::open(tmp.path().join("data"))
        .unwrap()
        .insert_downloader(&api::store::DownloaderRow {
            id,
            name: "second".into(),
            kind: "transmission".into(),
            url,
            username: None,
            password: None,
            category: None,
            path_maps: Vec::new(),
            is_default: false,
            enabled: true,
        })
        .unwrap();
    let subscribe = create_subscribe(&app, "search").await;
    let stable_url = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567";
    let submitted = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/downloaders/submit",
            Some("management-secret"),
            json!({"title":"The.Matrix.1999.1080p", "download_url":stable_url,
               "size_bytes":100, "subscribe_id":subscribe["id"], "downloader_id":id.to_string()}),
        ))
        .await
        .unwrap();
    assert_eq!(submitted.status(), StatusCode::OK);
    assert!(default.added().is_empty());
    let pending = Store::open(tmp.path().join("data"))
        .unwrap()
        .list_all_pending_routed()
        .unwrap();
    assert_eq!(pending[0].2.downloader_id, Some(id));
    let task_id = format!("{}:{stable_url}", subscribe["id"].as_str().unwrap());
    let removed = app
        .oneshot(request(
            "POST",
            "/api/v1/downloaders/tasks/remove",
            Some("management-secret"),
            json!({"task_id":task_id, "delete_files":false}),
        ))
        .await
        .unwrap();
    assert_eq!(removed.status(), StatusCode::OK);
    assert!(default.removed().is_empty());
    let calls = calls.lock();
    assert!(
        calls
            .iter()
            .any(|request| request.contains("torrent-add") && request.contains(stable_url))
    );
    assert!(
        calls
            .iter()
            .any(|request| request.contains("torrent-remove"))
    );
    assert!(
        Store::open(tmp.path().join("data"))
            .unwrap()
            .list_all_pending()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn changing_default_does_not_move_an_existing_pending_task() {
    let tmp = tempfile::tempdir().unwrap();
    let (first_url, first_calls) = transmission_fixture();
    let (second_url, second_calls) = transmission_fixture();
    let store = Arc::new(Mutex::new(Store::open(tmp.path().join("data")).unwrap()));
    let first_id = domain::DownloaderId::new();
    let second_id = domain::DownloaderId::new();
    for (id, name, url, is_default) in [
        (first_id, "first", first_url, true),
        (second_id, "second", second_url, false),
    ] {
        store
            .lock()
            .insert_downloader(&api::store::DownloaderRow {
                id,
                name: name.into(),
                kind: "transmission".into(),
                url,
                username: None,
                password: None,
                category: None,
                path_maps: Vec::new(),
                is_default,
                enabled: true,
            })
            .unwrap();
    }
    let dynamic: Arc<dyn downloader::Downloader> = Arc::new(DynamicDownloader::new(
        store.clone(),
        DownloaderEnv::default(),
        tmp.path(),
    ));
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let app = router(
        ApiState::new_arc(
            store.clone(),
            "management-secret".into(),
            ProfileSet::load(None).unwrap(),
            fetcher,
            dynamic,
            tmp.path().join("library"),
        )
        .unwrap(),
    );
    let subscribe = create_subscribe(&app, "search").await;
    let stable_url = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567";
    let submitted = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/downloaders/submit",
            Some("management-secret"),
            json!({"title":"The.Matrix.1999.1080p", "download_url":stable_url,
               "size_bytes":100, "subscribe_id":subscribe["id"]}),
        ))
        .await
        .unwrap();
    assert_eq!(submitted.status(), StatusCode::OK);
    assert_eq!(
        store.lock().list_all_pending_routed().unwrap()[0]
            .2
            .downloader_id,
        Some(first_id)
    );
    store.lock().set_default_downloader(second_id).unwrap();
    let task_id = format!("{}:{stable_url}", subscribe["id"].as_str().unwrap());
    let removed = app
        .oneshot(request(
            "POST",
            "/api/v1/downloaders/tasks/remove",
            Some("management-secret"),
            json!({"task_id": task_id}),
        ))
        .await
        .unwrap();
    assert_eq!(removed.status(), StatusCode::OK);
    assert!(
        first_calls
            .lock()
            .iter()
            .any(|call| call.contains("torrent-remove"))
    );
    assert!(
        !second_calls
            .lock()
            .iter()
            .any(|call| call.contains("torrent-remove"))
    );
}

#[tokio::test]
async fn manual_submit_rejects_another_media_before_downloader_add() {
    let tmp = tempfile::tempdir().unwrap();
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        downloader.clone(),
    ));
    let subscribe = create_subscribe(&app, "search").await;
    let response = app
        .oneshot(request(
            "POST",
            "/api/v1/downloaders/submit",
            Some("management-secret"),
            json!({"title":"Dune.2024.1080p",
            "download_url":"https://pt.example/dune", "subscribe_id":subscribe["id"]}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(downloader.added().is_empty());
    assert!(
        Store::open(tmp.path().join("data"))
            .unwrap()
            .list_all_pending()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn manual_submit_checks_tv_coverage_and_accepts_matching_episode() {
    let tmp = tempfile::tempdir().unwrap();
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        downloader.clone(),
    ));
    let mut payload = subscribe_payload("search");
    payload["media"] = json!({"kind":"tv", "title":"The Office"});
    payload["coverage"] = json!({"kind":"tv", "season":2, "episode_from":3, "episode_to":5});
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            payload,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let subscribe = json_data(created).await;
    for (title, status) in [
        ("The.Office.S02E06.1080p", StatusCode::BAD_REQUEST),
        ("The.Office.S02E04.1080p", StatusCode::OK),
    ] {
        let response = app.clone().oneshot(request("POST", "/api/v1/downloaders/submit",
            Some("management-secret"), json!({"title":title,
                "download_url":format!("https://pt.example/{title}"), "subscribe_id":subscribe["id"]})))
            .await.unwrap();
        assert_eq!(response.status(), status, "{title}");
    }
    assert_eq!(downloader.added().len(), 1);
    assert_eq!(
        Store::open(tmp.path().join("data"))
            .unwrap()
            .list_all_pending()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn transfer_does_not_ledger_a_stale_mismatched_pending_torrent() {
    let tmp = tempfile::tempdir().unwrap();
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        downloader.clone(),
    ));
    let subscribe = create_subscribe(&app, "search").await;
    let enclosure = "https://pt.example/dune";
    let source = tmp.path().join("Dune.2024.mkv");
    std::fs::write(&source, b"fixture").unwrap();
    downloader.map_enclosure(enclosure, source);
    let store = Store::open(tmp.path().join("data")).unwrap();
    let id = subscribe["id"].as_str().unwrap().parse().unwrap();
    store
        .merge_pending(
            id,
            &[(
                0,
                api::store::PendingDownload {
                    torrent: domain::Torrent {
                        site_id: domain::SiteId::new(),
                        title: "Dune.2024.1080p".into(),
                        enclosure: enclosure.into(),
                        size_bytes: None,
                        seeders: None,
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
                    submitted_at: None,
                },
            )],
        )
        .unwrap();
    let mut transfer_ran = false;
    for now in [1, 31, 61, 91, 121, 151, 181, 211, 241, 271] {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/jobs/tick?now={now}"),
                Some("management-secret"),
                serde_json::Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let jobs = json_data(
            app.clone()
                .oneshot(request(
                    "GET",
                    "/api/v1/jobs",
                    Some("management-secret"),
                    serde_json::Value::Null,
                ))
                .await
                .unwrap(),
        )
        .await;
        let defs = jobs.as_array().unwrap();
        for job in defs {
            if job["kind"] == "transfer" && job["last_status"] == "succeeded" {
                transfer_ran = true;
            }
        }
    }
    assert!(transfer_ran);
    assert!(store.list_ledger().unwrap().is_empty());
}

#[tokio::test]
async fn valid_manual_movie_and_tv_submissions_enter_their_own_library_media() {
    let tmp = tempfile::tempdir().unwrap();
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        downloader.clone(),
    ));
    let movie = create_subscribe(&app, "search").await;
    let mut payload = subscribe_payload("search");
    payload["media"] = json!({"kind":"tv", "title":"The Office"});
    payload["coverage"] = json!({"kind":"tv", "season":2, "episode_from":3, "episode_to":5});
    let tv = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                payload,
            ))
            .await
            .unwrap(),
    )
    .await;
    for (subscribe, title) in [
        (&movie, "The.Matrix.1999.1080p"),
        (&tv, "The.Office.S02E04.1080p"),
    ] {
        let enclosure = format!("https://pt.example/{title}");
        let source = tmp.path().join(format!("{title}.mkv"));
        std::fs::write(&source, b"fixture").unwrap();
        downloader.map_enclosure(&enclosure, source);
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/downloaders/submit",
                Some("management-secret"),
                json!({"title":title, "download_url":enclosure,
                "subscribe_id":subscribe["id"]}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    for now in [1, 31, 61, 91, 121, 151, 181, 211, 241, 271] {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/jobs/tick?now={now}"),
                Some("management-secret"),
                serde_json::Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        if Store::open(tmp.path().join("data"))
            .unwrap()
            .list_ledger()
            .unwrap()
            .len()
            == 2
        {
            break;
        }
    }
    let rows = Store::open(tmp.path().join("data"))
        .unwrap()
        .list_ledger()
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_ne!(rows[0].media_id, rows[1].media_id);
}

fn seed_transmission_downloader(store: &Store, url: String) -> domain::DownloaderId {
    let downloader_id = domain::DownloaderId::new();
    store
        .insert_downloader(&api::store::DownloaderRow {
            id: downloader_id,
            name: "transmission target".into(),
            kind: "transmission".into(),
            url,
            username: None,
            password: None,
            category: None,
            path_maps: Vec::new(),
            is_default: false,
            enabled: true,
        })
        .unwrap();
    downloader_id
}

fn make_test_delivery_torrent() -> domain::Torrent {
    domain::Torrent {
        id: None,
        site_id: domain::SiteId::new(),
        title: "The.Matrix.1999.1080p".into(),
        enclosure: "https://pt.example/dl/movie".into(),
        size_bytes: Some(100),
        seeders: Some(10),
        free: false,
        hr: false,
        imdb_id: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    }
}

#[tokio::test]
async fn deleting_subscription_removes_torrent_from_specific_downloader() {
    let tmp = tempfile::tempdir().unwrap();
    let (url, calls) = transmission_fixture();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let default_dl = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, default_dl.clone()));
    let store = Store::open(tmp.path().join("data")).unwrap();
    let downloader_id = seed_transmission_downloader(&store, url);

    let subscribe = create_subscribe(&app, "search").await;
    let sub_id = domain::SubscribeId::from_str(subscribe["id"].as_str().unwrap()).unwrap();
    let mut torrent = make_test_delivery_torrent();
    torrent.enclosure = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".into();
    store
        .record_pending_submission(
            sub_id,
            100,
            &api::store::PendingDownload {
                submitted_at: Some(1000),
                torrent: torrent.clone(),
                release_override: None,
                downloader_id: Some(downloader_id),
            },
        )
        .unwrap();

    let del = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/subscriptions/{sub_id}?delete_torrents=true"),
            Some("management-secret"),
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(del.status(), StatusCode::OK);

    let recorded = calls.lock().clone();
    assert!(
        recorded.iter().any(|c| c.contains("torrent-remove")),
        "删除订阅必须将删除请求路由到该任务所在的特定下载器，而不是默认下载器"
    );
}
