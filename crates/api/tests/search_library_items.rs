//! `/search/library-items` must page results and match actor names, not only titles.
//! A prefix that hits thousands of cards must not dump the whole set (e2e: 6.6MB).

use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::{Media, MediaId, MediaKind, UserRole};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::Value;
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn request(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

struct Fixture {
    _dir: tempfile::TempDir,
    state: ApiState,
    token: String,
    actor_media: MediaId,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let state = ApiState::new(
            Store::open(dir.path().join("data")).unwrap(),
            "search-secret".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
            dir.path().join("library"),
        )
        .unwrap();
        let token = "search-token".to_string();
        let handle = state.store();
        let store = handle.as_ref();
        let user = domain::User {
            id: domain::UserId::new(),
            login: "search-admin".into(),
            enabled: true,
            role: UserRole::Admin,
        };
        store.insert_user_with_password(&user, "password").unwrap();
        store.set_user_token(user.id, &token).unwrap();

        let library = store.default_library(MediaKind::Movie).unwrap().unwrap();
        std::fs::create_dir_all(&library.root_paths[0]).unwrap();

        // Three titles sharing the prefix "压测" — enough to prove limit/cursor.
        for index in 1..=3 {
            let media_id = MediaId::new();
            store
                .insert_media(&Media {
                    id: media_id,
                    kind: MediaKind::Movie,
                    title: format!("压测电影{index:04}"),
                    year: Some(2024),
                    original_title: None,
                    aliases: vec![],
                    tmdb_id: None,
                    douban_id: None,
                    tvdb_id: None,
                    bangumi_id: None,
                    anilist_id: None,
                })
                .unwrap();
            let path = library.root_paths[0].join(format!("stress-{index}.mkv"));
            std::fs::write(&path, b"fixture").unwrap();
            store::test_support::publish_fixture_file(
                store,
                &store::test_support::FixtureFile {
                    media_id,
                    path: path.display().to_string(),
                    ..Default::default()
                },
            )
            .unwrap();
        }

        // One movie whose title does not match "压测", but whose cast does.
        let actor_media = MediaId::new();
        store
            .insert_media(&Media {
                id: actor_media,
                kind: MediaKind::Movie,
                title: "无关片名".into(),
                year: Some(2020),
                original_title: None,
                aliases: vec![],
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        let path = library.root_paths[0].join("actor.mkv");
        std::fs::write(&path, b"fixture").unwrap();
        store::test_support::publish_fixture_file(
            store,
            &store::test_support::FixtureFile {
                media_id: actor_media,
                path: path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        let library_id = library.id.clone();
        let data_dir = store.data_dir().to_path_buf();
        let conn = rusqlite::Connection::open(data_dir.join("library.db")).unwrap();
        conn.execute(
            "INSERT INTO library_protocol_people
             (library_id, media_id, role, name_key, tmdb_id, wire_id)
             VALUES (?1, ?2, 'Actor', '演员001', '', 'actor-001')",
            rusqlite::params![library_id, actor_media.to_string()],
        )
        .unwrap();

        Self {
            _dir: dir,
            state,
            token,
            actor_media,
        }
    }
}

#[test]
fn library_item_search_pages_and_matches_actor_names() {
    let f = Fixture::new();
    let app = router(f.state.clone());
    let body = runtime().block_on(async {
        let response = app
            .clone()
            .oneshot(request(
                "/api/v1/search/library-items?keyword=%E5%8E%8B%E6%B5%8B&limit=2",
                &f.token,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        serde_json::from_slice::<Value>(&bytes).unwrap()
    });
    let data = body["data"].as_array().expect("data array");
    assert_eq!(data.len(), 2, "limit=2 must cap the page: {body}");
    assert!(
        body["next_cursor"].as_str().is_some(),
        "more than one page of prefix hits must expose next_cursor: {body}"
    );
    for item in data {
        let title = item["title"].as_str().unwrap_or_default();
        assert!(
            title.contains("压测"),
            "page must only contain title matches: {item}"
        );
        assert!(
            item.get("file_id").is_none(),
            "search returns cards, not every file: {item}"
        );
        assert!(item["library_id"].as_str().is_some(), "{item}");
        assert!(item["media_item_id"].as_str().is_some(), "{item}");
    }

    let actor = runtime().block_on(async {
        let response = app
            .oneshot(request(
                "/api/v1/search/library-items?keyword=%E6%BC%94%E5%91%98001",
                &f.token,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        serde_json::from_slice::<Value>(&bytes).unwrap()
    });
    let hits = actor["data"].as_array().expect("data array");
    assert_eq!(hits.len(), 1, "actor name must find the cast card: {actor}");
    assert_eq!(
        hits[0]["media_item_id"].as_str(),
        Some(f.actor_media.to_string().as_str())
    );
    assert_eq!(hits[0]["title"].as_str(), Some("无关片名"));
}
