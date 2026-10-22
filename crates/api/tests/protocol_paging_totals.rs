//! Risk 5: protocol listings must report a `TotalRecordCount` equal to the number of
//! distinct items reachable by paging (movie / TV / episode / unknown parent and
//! merged cross-Library listings).
use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Media, MediaId, MediaKind};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use store::test_support::{FixtureFile, publish_fixture_file};
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

struct Fx {
    _tmp: tempfile::TempDir,
    app: axum::Router,
    films: String,
    shows: String,
}

fn media(store: &Store, kind: MediaKind, title: &str) -> MediaId {
    let id = MediaId::new();
    store
        .insert_media(&Media {
            id,
            kind,
            title: title.into(),
            year: Some(2001),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    id
}

fn file(store: &Store, media_id: MediaId, path: &Path, unit: Option<(u32, u32)>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"video").unwrap();
    publish_fixture_file(
        store,
        &FixtureFile {
            media_id,
            path: path.display().to_string(),
            season: unit.map(|u| u.0),
            episode: unit.map(|u| u.1),
            ..Default::default()
        },
    )
    .unwrap();
}

fn library(store: &Store, kind: MediaKind, name: &str, root: &Path) -> String {
    std::fs::create_dir_all(root).unwrap();
    store
        .create_library(kind, name, &[root.to_str().unwrap()], "everyone", true, &[])
        .unwrap()
        .id
}

fn seed_movies(store: &Store, films: &Path, films2: &Path) {
    for title in ["Alpha", "Bravo", "Charlie", "Delta", "Echo"] {
        let id = media(store, MediaKind::Movie, title);
        file(store, id, &films.join(format!("{title}.mkv")), None);
    }
    // Two versions of one movie merge into one item.
    let dual = media(store, MediaKind::Movie, "Dual");
    file(store, dual, &films.join("Dual - 1080p.mkv"), None);
    file(store, dual, &films.join("Dual - 2160p.mkv"), None);
    // One movie owned by two Libraries.
    let shared = media(store, MediaKind::Movie, "Shared");
    file(store, shared, &films.join("Shared.mkv"), None);
    file(store, shared, &films2.join("Shared.mkv"), None);
    let only2 = media(store, MediaKind::Movie, "Foxtrot");
    file(store, only2, &films2.join("Foxtrot.mkv"), None);
}

fn seed_shows(store: &Store, shows: &Path, shows2: &Path) {
    for (title, episodes) in [("Show A", 3u32), ("Show B", 1), ("Show C", 2)] {
        let id = media(store, MediaKind::Tv, title);
        for e in 1..=episodes {
            file(
                store,
                id,
                &shows.join(title).join(format!("S01E0{e}.mkv")),
                Some((1, e)),
            );
        }
    }
    // A series split across two Libraries, plus a merged double-episode file.
    let split = media(store, MediaKind::Tv, "Split");
    file(store, split, &shows.join("Split/S01E01.mkv"), Some((1, 1)));
    file(store, split, &shows2.join("Split/S02E01.mkv"), Some((2, 1)));
    store::test_support::publish_fixture_file_units(
        store,
        &FixtureFile {
            media_id: split,
            path: {
                let p = shows.join("Split/S01E02-E03.mkv");
                std::fs::write(&p, b"video").unwrap();
                p.display().to_string()
            },
            ..Default::default()
        },
        &[domain::UnitKey::new(1, 2), domain::UnitKey::new(1, 3)],
    )
    .unwrap();
}

fn fixture() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let store = Store::open(root.join("data")).unwrap();
    let films = library(&store, MediaKind::Movie, "Films", &root.join("films"));
    library(&store, MediaKind::Movie, "Films2", &root.join("films2"));
    let shows = library(&store, MediaKind::Tv, "Shows", &root.join("shows"));
    library(&store, MediaKind::Tv, "Shows2", &root.join("shows2"));
    seed_movies(&store, &root.join("films"), &root.join("films2"));
    seed_shows(&store, &root.join("shows"), &root.join("shows2"));
    let state = ApiState::new(
        store,
        "admin-token".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap();
    Fx {
        _tmp: tmp,
        app: router(state),
        films,
        shows,
    }
}

async fn get(app: &axum::Router, uri: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{uri}");
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Walk every page of `uri` (size `page`) and return (total, distinct ids).
async fn walk(app: &axum::Router, uri: &str, page: usize) -> (u64, Vec<String>) {
    let sep = if uri.contains('?') { '&' } else { '?' };
    let unpaged = get(app, uri).await;
    let total = unpaged["TotalRecordCount"]
        .as_u64()
        .expect("TotalRecordCount");
    let unpaged_len = unpaged["Items"].as_array().unwrap().len() as u64;
    assert_eq!(
        unpaged_len, total,
        "{uri}: unpaged Items vs TotalRecordCount"
    );
    let mut ids = Vec::new();
    let mut seen = BTreeSet::new();
    let mut start = 0usize;
    loop {
        let body = get(app, &format!("{uri}{sep}StartIndex={start}&Limit={page}")).await;
        assert_eq!(
            body["TotalRecordCount"].as_u64(),
            Some(total),
            "{uri} @ {start}: total drifts"
        );
        let items = body["Items"].as_array().unwrap();
        for item in items {
            let id = item["Id"].as_str().unwrap().to_string();
            assert!(seen.insert(id.clone()), "{uri}: {id} repeats across pages");
            ids.push(id);
        }
        if items.is_empty() || items.len() < page {
            break;
        }
        start += page;
        assert!(start <= 1000, "{uri}: paging does not terminate");
    }
    assert_eq!(
        ids.len() as u64,
        total,
        "{uri}: paged items {ids:?} vs total {total}"
    );
    (total, ids)
}

#[tokio::test]
async fn movie_listings_total_matches_pages() {
    let fx = fixture();
    let (lib, _) = walk(
        &fx.app,
        &format!(
            "/Items?ParentId={}&IncludeItemTypes=Movie&Recursive=true",
            fx.films
        ),
        2,
    )
    .await;
    assert_eq!(lib, 7, "five + merged versions + shared");
    let (merged, _) = walk(&fx.app, "/Items?IncludeItemTypes=Movie&Recursive=true", 3).await;
    assert_eq!(
        merged, 8,
        "a movie owned by two Libraries is one merged item"
    );
}

#[tokio::test]
async fn tv_listings_total_matches_pages() {
    let fx = fixture();
    let (series, ids) = walk(&fx.app, &format!("/Items?ParentId={}", fx.shows), 2).await;
    assert_eq!(series, 4);
    let (merged, _) = walk(&fx.app, "/Items?IncludeItemTypes=Series&Recursive=true", 3).await;
    assert_eq!(
        merged, 4,
        "a series split across Libraries is one merged item"
    );
    let (episodes, _) = walk(&fx.app, "/Items?IncludeItemTypes=Episode&Recursive=true", 2).await;
    assert_eq!(episodes, 10, "a double-episode file lists both units");
    for id in ids {
        walk(&fx.app, &format!("/Items?ParentId={id}"), 2).await;
        walk(&fx.app, &format!("/Shows/{id}/Episodes"), 2).await;
    }
}

#[tokio::test]
async fn unknown_parent_and_mixed_listings_total_matches_pages() {
    let fx = fixture();
    let (unknown, _) = walk(
        &fx.app,
        "/Items?ParentId=ffffffffffffffffffffffffffffffff&Recursive=true",
        2,
    )
    .await;
    assert_eq!(unknown, 0);
    walk(&fx.app, "/Items?Recursive=true", 3).await;
    walk(
        &fx.app,
        "/Items?Recursive=true&IncludeItemTypes=Movie,Series",
        4,
    )
    .await;
}

async fn post(app: &axum::Router, uri: &str) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{uri}");
}

#[tokio::test]
async fn filtered_movie_listings_total_matches_pages() {
    let fx = fixture();
    let base = format!(
        "/Items?ParentId={}&IncludeItemTypes=Movie&Recursive=true",
        fx.films
    );
    let all = get(&fx.app, &format!("{base}&SortBy=SortName")).await;
    let ids: Vec<String> = all["Items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["Id"].as_str().unwrap().to_string())
        .collect();
    post(&fx.app, &format!("/UserFavoriteItems/{}", ids[0])).await;
    post(&fx.app, &format!("/UserFavoriteItems/{}", ids[1])).await;
    post(&fx.app, &format!("/UserPlayedItems/{}", ids[2])).await;
    let (fav, _) = walk(&fx.app, &format!("{base}&Filters=IsFavorite"), 1).await;
    assert_eq!(fav, 2);
    let (played, _) = walk(&fx.app, &format!("{base}&IsPlayed=true"), 1).await;
    assert_eq!(played, 1);
    let (unplayed, _) = walk(&fx.app, &format!("{base}&IsPlayed=false"), 2).await;
    assert_eq!(unplayed, 6);
    walk(&fx.app, &format!("{base}&SearchTerm=a"), 2).await;
    walk(&fx.app, "/Items?Recursive=true&Filters=IsFavorite", 1).await;
}
