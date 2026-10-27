use super::{RouterTransport, search_transport, transport};
use crate::common::*;
use api::cli::{Command, Transport, run};
use api::{Store, router};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test]
async fn cli_sites_prints_created_site_name() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    create_site(&t.app).await;
    let listed = t.send("GET", "/api/v1/sites", None).await.unwrap();
    let id = listed.1[0]["id"].as_str().unwrap();
    let out = run(Command::Sites, &t).await.unwrap();
    assert!(out.contains("demo"), "{out}");
    assert!(out.contains("https://pt.example/"), "{out}");
    assert!(out.contains(id), "{out}");
    assert!(out.contains("enabled=true"), "{out}");
    assert!(!out.contains("uid=1"), "{out}");
}

#[tokio::test]
async fn cli_search_prints_torrent_title() {
    let tmp = tempfile::tempdir().unwrap();
    let t = search_transport(&tmp, HashMap::from([("search", nexusphp())]));
    create_site(&t.app).await;
    let out = run(
        Command::Search {
            query: "matrix".into(),
        },
        &t,
    )
    .await
    .unwrap();
    assert!(out.contains("The.Matrix.1999"), "{out}");
    assert!(out.contains("2160p"), "{out}");
}

#[tokio::test]
async fn cli_search_prints_site_failure_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    create_site(&t.app).await;
    let out = run(
        Command::Search {
            query: "matrix".into(),
        },
        &t,
    )
    .await
    .unwrap();
    assert!(out.contains("failed"), "{out}");
}

#[tokio::test]
async fn cli_admit_prints_torrent_title() {
    let tmp = tempfile::tempdir().unwrap();
    let t = search_transport(&tmp, HashMap::from([("search", nexusphp())]));
    create_site(&t.app).await;
    let subscribe = create_subscribe(&t.app, "search").await;
    let searched = t
        .send("GET", "/api/v1/search/torrents?keyword=matrix", None)
        .await
        .unwrap();
    assert_eq!(searched.0, 200);
    let enclosure = searched.1["items"][0]["enclosure"]
        .as_str()
        .unwrap()
        .to_string();
    let out = run(
        Command::Admit {
            subscribe_id: subscribe["id"].as_str().unwrap().into(),
            enclosure,
        },
        &t,
    )
    .await
    .unwrap();
    assert!(out.contains("The.Matrix.1999"), "{out}");
}

#[tokio::test]
async fn cli_sites_add_prints_name_and_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let _out = run(
        Command::SitesAdd {
            name: "demo".into(),
            url: "https://pt.example/".into(),
            profile_id: "demo".into(),
            cookie: Some("uid=1; pass=abc".into()),
            api_key: None,
        },
        &t,
    )
    .await
    .unwrap();
    let app = router(
        state(
            tmp.path(),
            Arc::new(Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )
        .with_catalog(Arc::new(crate::catalog::test_catalog())),
    );
    let t = RouterTransport {
        app,
        token: "management-secret".into(),
    };
    let out = run(
        Command::CatalogSearch {
            query: "matrix".into(),
        },
        &t,
    )
    .await
    .unwrap();
    assert!(out.contains("The Matrix"), "{out}");
    assert!(out.contains("movie"), "{out}");
    assert!(out.contains("tmdb=603"), "{out}");
    assert!(out.contains("douban=-"), "{out}");
    assert!(out.contains("tvdb=-"), "{out}");
    assert!(out.contains("bangumi=-"), "{out}");
    assert!(out.contains("anilist=-"), "{out}");
}

#[tokio::test]
async fn cli_catalog_lists_and_deletes_cache_row() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .put_catalog_cache(
            "tmdb",
            "/search/movie?query=matrix",
            r#"{"results":[{"title":"The Matrix"}]}"#,
            100,
            Some(200),
        )
        .unwrap();
    drop(store);
    let t = transport(&tmp);
    let listed = run(Command::Catalog, &t).await.unwrap();
    assert!(listed.contains("The Matrix"), "{listed}");
    assert!(listed.contains("tmdb"), "{listed}");
    let out = run(
        Command::CatalogDelete {
            source: "tmdb".into(),
            cache_key: "/search/movie?query=matrix".into(),
        },
        &t,
    )
    .await
    .unwrap();
    assert!(out.contains("deleted=true"), "{out}");
    let listed = run(Command::Catalog, &t).await.unwrap();
    assert_eq!(listed, "No cache\n");
}
