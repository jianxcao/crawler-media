use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use api::{ApiState, Store};
use axum::body::{Body, to_bytes};
use axum::http::Request;
use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource, UserRole};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::Value;
use store::test_support::{FixtureFile, publish_fixture_file};
use tower::ServiceExt;

pub struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

pub fn make_state(root: &Path) -> ApiState {
    ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
}

pub async fn json_data(response: axum::response::Response) -> Value {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["ok"], true, "status={status} body={body}");
    body.get("data").cloned().unwrap_or(Value::Null)
}

pub async fn get(app: &axum::Router, uri: &str, token: &str) -> Value {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    json_data(app.clone().oneshot(request).await.unwrap()).await
}

pub fn seed_member(store: &Store, login: &str) -> (domain::UserId, String) {
    let id = domain::UserId::new();
    store
        .insert_user_with_password(
            &domain::User {
                id,
                login: login.into(),
                enabled: true,
                role: UserRole::Member,
            },
            "member-pass",
        )
        .unwrap();
    let token = format!("{login}-session");
    store.set_user_token(id, &token).unwrap();
    (id, token)
}

pub fn seed_library(store: &Store, root: &Path, name: &str) -> PathBuf {
    let library_root = root.join(format!("library-{name}"));
    std::fs::create_dir_all(&library_root).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            name,
            &[library_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    library_root
}

pub fn seed_media(store: &Store, title: &str, year: Option<u16>) -> MediaId {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: title.into(),
        year,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    media.id
}

pub fn add_ledger(store: &Store, media_id: MediaId, library_root: &Path, name: &str) -> LedgerId {
    let path = library_root.join(format!("{name}.mkv"));
    std::fs::write(&path, b"x").unwrap();
    let id = LedgerId::new();
    publish_fixture_file(
        store,
        &FixtureFile {
            file_id: Some(id),
            media_id,
            path: path.display().to_string(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
            ..Default::default()
        },
    )
    .unwrap();
    id
}

pub fn favorite(store: &Store, user_id: domain::UserId, media_id: MediaId, favorite: bool) {
    store
        .mutate_playback(
            media_id,
            &store::playback::PlaybackMutation::marks(
                user_id,
                api::store::UNIT_WHOLE,
                api::store::UNIT_WHOLE,
                None,
                Some(favorite),
            ),
            now_secs() * 1000,
        )
        .unwrap();
}
