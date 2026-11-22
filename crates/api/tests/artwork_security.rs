use api::{ApiState, PosterFetch, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource, UnitKey, UserRole};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use library::TransferMode;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use store::library_write::WriteScope;
use store::test_support::{FixtureFile, publish_fixture_file};
use tower::ServiceExt;

const ADMIN_TOKEN: &str = "artwork-sec-admin";

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}

struct RecordingPosterFetch {
    calls: AtomicUsize,
    payload: Vec<u8>,
}

impl RecordingPosterFetch {
    fn new(payload: Vec<u8>) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            payload,
        }
    }
}

impl PosterFetch for RecordingPosterFetch {
    fn get(&self, _: &str) -> Result<Vec<u8>, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.payload.clone())
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    state: ApiState,
    library_id: String,
    root: PathBuf,
    item_dir: PathBuf,
    media_id: MediaId,
    file_id: LedgerId,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let state = ApiState::new(
            Store::open(tmp.path().join("data")).unwrap(),
            ADMIN_TOKEN.into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )
        .unwrap();
        let root = tmp.path().join("movies");
        let item_dir = root.join("Film");
        std::fs::create_dir_all(&item_dir).unwrap();
        let video = item_dir.join("Film.mkv");
        std::fs::write(&video, b"video").unwrap();
        let store = state.store();
        let library = store
            .create_library(
                MediaKind::Movie,
                "Movies",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: "Film".into(),
            year: Some(2024),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&media).unwrap();
        let file_id = LedgerId::new();
        publish_fixture_file(
            &store,
            &FixtureFile {
                file_id: Some(file_id),
                media_id: media.id,
                path: video.display().to_string(),
                quality_source: QualitySource::Probe,
                confidence: Confidence::High,
                ..Default::default()
            },
        )
        .unwrap();
        drop(store);
        Self {
            _tmp: tmp,
            state,
            library_id: library.id,
            root,
            item_dir,
            media_id: media.id,
            file_id,
        }
    }

    fn select_req(&self, body: Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/v1/libraries/{}/items/{}/artwork/select",
                self.library_id, self.media_id
            ))
            .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }
}

fn valid_jpeg() -> Vec<u8> {
    b"\xff\xd8\xff\xe0jpeg-bytes".to_vec()
}

fn valid_png() -> Vec<u8> {
    b"\x89PNG\r\n\x1a\npng-bytes".to_vec()
}

fn valid_webp() -> Vec<u8> {
    b"RIFF\x00\x00\x00\x00WEBPwebp-bytes".to_vec()
}

fn seed_member(store: &Store, login: &str) -> String {
    let id = domain::UserId::new();
    store
        .insert_user_with_password(
            &domain::User {
                id,
                login: login.into(),
                enabled: true,
                role: UserRole::Member,
            },
            "pass",
        )
        .unwrap();
    let token = format!("{login}-token");
    store.set_user_token(id, &token).unwrap();
    token
}

async fn get_status(app: &axum::Router, uri: &str, token: Option<&str>) -> (StatusCode, Value) {
    let mut builder = Request::get(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let resp = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn select_local_file_path_rejects_outside_root_dir_symlink_disabled_non_image_and_oversized()
{
    let fx = Fixture::new();
    let app = router(fx.state.clone());

    let outside = fx._tmp.path().join("outside.jpg");
    std::fs::write(&outside, valid_jpeg()).unwrap();

    let symlink_escape = fx.item_dir.join("escape.jpg");
    std::os::unix::fs::symlink(&outside, &symlink_escape).unwrap();

    let non_image = fx.item_dir.join("not-image.jpg");
    std::fs::write(&non_image, b"plain text not an image").unwrap();

    let oversized = fx.item_dir.join("huge.jpg");
    let mut huge = valid_jpeg();
    huge.resize(12 * 1024 * 1024 + 1, 0);
    std::fs::write(&oversized, huge).unwrap();

    let disabled_root = fx._tmp.path().join("disabled-root");
    std::fs::create_dir_all(&disabled_root).unwrap();
    let disabled_img = disabled_root.join("disabled.jpg");
    std::fs::write(&disabled_img, valid_jpeg()).unwrap();
    fx.state
        .store()
        .with_writer(api::store::Db::Library, |conn| {
            conn.execute(
                "INSERT INTO library_roots(root_id, library_id, path_key, display_path, enabled, sort_order)
                 VALUES (?1, ?2, ?3, ?4, 0, 1)",
                rusqlite::params![
                    uuid::Uuid::new_v4().to_string(),
                    fx.library_id,
                    disabled_root.display().to_string(),
                    disabled_root.display().to_string(),
                ],
            )?;
            Ok(())
        })
        .unwrap();

    for candidate in [
        outside.as_path(),
        fx.root.as_path(),
        symlink_escape.as_path(),
        disabled_img.as_path(),
        non_image.as_path(),
        oversized.as_path(),
    ] {
        let resp = app
            .clone()
            .oneshot(fx.select_req(json!({ "file_path": candidate.display().to_string() })))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "must reject local file_path {}",
            candidate.display()
        );
        assert!(!fx.item_dir.join("poster.jpg").exists());
    }
}

#[tokio::test]
async fn select_local_file_path_accepts_valid_jpeg_png_and_webp_inside_enabled_root() {
    let fx = Fixture::new();
    let app = router(fx.state.clone());
    for (name, bytes) in [
        ("ok.jpg", valid_jpeg()),
        ("ok.png", valid_png()),
        ("ok.webp", valid_webp()),
    ] {
        let cand = fx.item_dir.join(name);
        std::fs::write(&cand, &bytes).unwrap();
        let resp = app
            .clone()
            .oneshot(fx.select_req(json!({ "file_path": cand.display().to_string() })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{name}");
        assert_eq!(
            std::fs::read(fx.item_dir.join("poster.jpg")).unwrap(),
            bytes
        );
    }
}

#[tokio::test]
async fn select_http_file_path_enforces_allowlist_magic_bytes_and_size_cap() {
    let fx = Fixture::new();
    let disallowed_fetch = Arc::new(RecordingPosterFetch::new(valid_jpeg()));
    let app = router(fx.state.clone().with_poster_fetch(disallowed_fetch.clone()));

    for bad_url in [
        "http://127.0.0.1:9999/x.jpg",
        "http://localhost/x.jpg",
        "https://evil.example/x.jpg",
    ] {
        let resp = app
            .clone()
            .oneshot(fx.select_req(json!({ "file_path": bad_url })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{bad_url}");
    }
    assert_eq!(disallowed_fetch.calls.load(Ordering::SeqCst), 0);

    let bad_bytes_fetch = Arc::new(RecordingPosterFetch::new(b"not-an-image".to_vec()));
    let app_bad_bytes = router(fx.state.clone().with_poster_fetch(bad_bytes_fetch));
    let resp = app_bad_bytes
        .oneshot(fx.select_req(json!({ "file_path": "https://image.tmdb.org/t/p/w780/bad.jpg" })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let mut oversized = valid_jpeg();
    oversized.resize(12 * 1024 * 1024 + 1, 1);
    let oversized_fetch = Arc::new(RecordingPosterFetch::new(oversized));
    let app_oversized = router(fx.state.clone().with_poster_fetch(oversized_fetch));
    let resp = app_oversized
        .oneshot(fx.select_req(json!({ "file_path": "https://image.tmdb.org/t/p/w780/huge.jpg" })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    for payload in [valid_jpeg(), valid_png(), valid_webp()] {
        let ok_fetch = Arc::new(RecordingPosterFetch::new(payload.clone()));
        let app_ok = router(fx.state.clone().with_poster_fetch(ok_fetch));
        let resp = app_ok
            .oneshot(
                fx.select_req(json!({ "file_path": "https://image.tmdb.org/t/p/w780/ok.jpg" })),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            std::fs::read(fx.item_dir.join("poster.jpg")).unwrap(),
            payload
        );
    }
}

#[tokio::test]
async fn pending_file_returns_404_on_images_and_candidates_until_published() {
    let fx = Fixture::new();
    let store = fx.state.store();
    let pending_media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "PendingFilm".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&pending_media).unwrap();
    let pending_dir = fx.root.join("PendingFilm");
    std::fs::create_dir_all(&pending_dir).unwrap();
    let pending_video = pending_dir.join("PendingFilm.mkv");
    let source = fx.item_dir.join("Film.mkv");
    std::fs::write(pending_dir.join("poster.jpg"), valid_jpeg()).unwrap();
    std::fs::write(pending_dir.join("fanart.jpg"), valid_jpeg()).unwrap();
    std::fs::write(pending_dir.join("PendingFilm-thumb.jpg"), valid_jpeg()).unwrap();

    let mut candidate = store.file_coverage_by_id(fx.file_id).unwrap().unwrap();
    candidate.file.file_id = LedgerId::new();
    candidate.file.media_id = pending_media.id;
    candidate.file.path = pending_video.clone();
    candidate.path_key = pending_video.display().to_string();
    candidate.units = vec![UnitKey::WHOLE];
    let scope = WriteScope {
        library_id: candidate.file.library_id,
        config_revision: store.library_config_revision(&fx.library_id).unwrap(),
    };
    let pending = store
        .reserve_transfer(&scope, &candidate, &source, TransferMode::Copy, 1_000)
        .unwrap();
    let pending_file_id = pending.file.file_id;
    drop(store);

    let app = router(fx.state.clone());
    let cand_uri = format!(
        "/api/v1/libraries/{}/items/{}/artwork/candidates",
        fx.library_id, pending_media.id
    );
    for (uri, token) in [
        (format!("/api/v1/posters/{pending_file_id}"), None),
        (format!("/api/v1/fanart/{pending_file_id}"), None),
        (format!("/api/v1/stills/{pending_file_id}"), None),
        (cand_uri.clone(), Some(ADMIN_TOKEN)),
    ] {
        let (status, _) = get_status(&app, &uri, token).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "pending must 404 on {uri}");
    }

    std::fs::copy(&source, &pending_video).unwrap();
    fx.state
        .store()
        .finish_transfer(&scope, &pending, &pending.file.version, 2_000)
        .unwrap();

    for (uri, token) in [
        (format!("/api/v1/posters/{pending_file_id}"), None),
        (format!("/api/v1/fanart/{pending_file_id}"), None),
        (format!("/api/v1/stills/{pending_file_id}"), None),
        (cand_uri, Some(ADMIN_TOKEN)),
    ] {
        let (status, _) = get_status(&app, &uri, token).await;
        assert_eq!(status, StatusCode::OK, "published must 200 on {uri}");
    }
}

#[tokio::test]
async fn artwork_candidates_redacts_host_paths_for_member_and_uses_read_pool() {
    let fx = Fixture::new();
    let poster_file = fx.item_dir.join("poster.jpg");
    std::fs::write(&poster_file, valid_jpeg()).unwrap();
    let member_token = seed_member(&fx.state.store(), "viewer");
    let app = router(fx.state.clone());
    let uri = format!(
        "/api/v1/libraries/{}/items/{}/artwork/candidates",
        fx.library_id, fx.media_id
    );

    let (admin_status, admin_body) = get_status(&app, &uri, Some(ADMIN_TOKEN)).await;
    assert_eq!(admin_status, StatusCode::OK);
    let expected_host_path = poster_file.display().to_string();
    assert_eq!(
        admin_body["data"]["posters"][0]["file_path"],
        expected_host_path
    );
    assert_eq!(admin_body["data"]["current_poster"], expected_host_path);

    let (member_status, member_body) = get_status(&app, &uri, Some(&member_token)).await;
    assert_eq!(member_status, StatusCode::OK);
    let expected_preview = format!("/posters/{}", fx.file_id);
    assert_eq!(
        member_body["data"]["posters"][0]["file_path"],
        expected_preview
    );
    assert_eq!(
        member_body["data"]["posters"][0]["preview_url"],
        expected_preview
    );
    assert_eq!(member_body["data"]["current_poster"], Value::Null);

    fx.state
        .store()
        .reader_pool(api::store::Db::Library)
        .close();
    let (busy_status, busy_body) = get_status(&app, &uri, Some(ADMIN_TOKEN)).await;
    assert_eq!(busy_status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(busy_body["error"]["code"], "runtime.busy");
}
