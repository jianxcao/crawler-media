//! R06: Jellyfin streams consume the PlaybackInfo binding. The advertised
//! MediaSourceId is the versioned `f1_` physical source; an old session after a
//! source replacement, a wrong device or a bare file UUID gets no media bytes.

use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use store::test_support::{FixtureFile, publish_fixture_file};
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

struct Fixture {
    app: axum::Router,
    item_id: String,
    file_id: LedgerId,
    path: PathBuf,
}

fn fixture(root: &Path) -> Fixture {
    let store = Store::open(root.join("data")).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Bound".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let dir = root.join("data/library/movies");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bound.mkv");
    std::fs::write(&path, b"AAAAAAAA").unwrap();
    let file_id = LedgerId::new();
    publish_fixture_file(
        &store,
        &FixtureFile {
            file_id: Some(file_id),
            media_id: media.id,
            path: path.display().to_string(),
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            ..Default::default()
        },
    )
    .unwrap();
    let app = router(
        ApiState::new(
            store,
            "admin-token".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(root.join("stage"))),
            root.join("library"),
        )
        .unwrap(),
    );
    Fixture {
        app,
        item_id: domain::TypedItemId::Work(media.id).to_wire_string(),
        file_id,
        path,
    }
}

async fn get(app: &axum::Router, method: &str, uri: &str, device: &str) -> (StatusCode, Vec<u8>) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Bearer admin-token")
        .header("X-Emby-Device-Id", device)
        .header("X-Emby-Client", "Infuse")
        .header("content-type", "application/json")
        .header("range", "bytes=0-3")
        .body(Body::from("{}"))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    (status, to_bytes(response.into_body(), usize::MAX).await.unwrap().to_vec())
}

fn stream_uri(f: &Fixture, source: &str, session: &str, device: &str) -> String {
    format!(
        "/Videos/{}/stream?MediaSourceId={source}&PlaySessionId={session}&DeviceId={device}",
        f.item_id
    )
}

#[tokio::test]
async fn production_jellyfin_stream_uses_issued_binding() {
    let tmp = tempfile::tempdir().unwrap();
    let f = fixture(tmp.path());
    let (status, body) = get(&f.app, "POST", &format!("/Items/{}/PlaybackInfo", f.item_id), "tv-1").await;
    assert_eq!(status, StatusCode::OK);
    let info: Value = serde_json::from_slice(&body).unwrap();
    let source = info["MediaSources"][0]["Id"].as_str().unwrap().to_string();
    let session = info["PlaySessionId"].as_str().unwrap().to_string();
    let physical = domain::PhysicalSourceId { file_id: f.file_id, revision: 1 };
    assert_eq!(source, physical.to_wire_string());

    let (status, bytes) = get(&f.app, "GET", &stream_uri(&f, &source, &session, "tv-1"), "tv-1").await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(bytes, b"AAAA");

    let (status, _) = get(&f.app, "GET", &stream_uri(&f, &source, &session, "tv-2"), "tv-2").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a binding from another device is refused");
    let bare = f.file_id.to_string();
    let (status, _) = get(&f.app, "GET", &stream_uri(&f, &bare, &session, "tv-1"), "tv-1").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a bare file UUID is not a MediaSourceId");

    // The source is replaced: the old session/source must not reach the new bytes.
    std::fs::write(&f.path, b"XXXXXXXX").unwrap();
    {
        let store = Store::open(tmp.path().join("data")).unwrap();
        let version = store.get_file_version(f.file_id).unwrap().unwrap();
        let file = store.file_coverage_by_path(&f.path.display().to_string()).unwrap().unwrap().file;
        let scope = store::library_write::WriteScope {
            library_id: file.library_id,
            config_revision: store.library_config_revision(&file.library_id.to_string()).unwrap(),
        };
        let observation = api::fingerprint_job::observe_source(&f.path).unwrap();
        store.replace_file_source(&scope, f.file_id, &version, &observation, 5_000).unwrap();
    }
    let (status, bytes) = get(&f.app, "GET", &stream_uri(&f, &source, &session, "tv-1"), "tv-1").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_ne!(bytes, b"XXXX");
}
