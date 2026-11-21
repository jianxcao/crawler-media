//! Regression test for file-scoped playback asset endpoints (`/playback/files/{id}/fonts`
//! and `/playback/files/{id}/trickplay`) assembled with the real `ApiServerProvider`.
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, Media, MediaId,
    MediaKind, QualitySource, UnitKey, User, UserId, UserRole,
};
use store::Store;
use store::library_write::{FilePublication as StorePublication, WriteScope};
use tower::ServiceExt;

struct NoopFetcher;

impl indexer::Fetcher for NoopFetcher {
    fn fetch(&self, _req: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        Ok(String::new())
    }
}

struct SeededAssets {
    _tmp: tempfile::TempDir,
    app: axum::Router,
    file_without_assets: LedgerId,
    file_with_assets: LedgerId,
    visible_token: String,
    hidden_token: String,
}

fn make_movie_file(
    file_id: LedgerId,
    library_id: domain::LibraryId,
    root_id: &str,
    media_id: MediaId,
    path: &std::path::Path,
) -> LibraryFile {
    LibraryFile {
        file_id,
        library_id,
        root_id: root_id.to_string(),
        media_id,
        kind: MediaKind::Movie,
        path: path.to_path_buf(),
        version: FileVersion {
            revision: 1,
            source_version: library::current_source_version(path),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(11),
        stat_known: true,
        created_ns: Some(1),
        modified_ns: Some(2),
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: Some(120_000),
        tracks_version: 1,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    }
}

fn seed_two_version_movie() -> SeededAssets {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let root = tmp.path().join("private-movies");
    std::fs::create_dir_all(&root).unwrap();
    let store = Store::open(&data).unwrap();

    let visible_user = UserId::new();
    let hidden_user = UserId::new();
    for (id, login, token) in [
        (visible_user, "visible_member", "visible-token"),
        (hidden_user, "hidden_member", "hidden-token"),
    ] {
        store
            .insert_user(&User {
                id,
                login: login.into(),
                enabled: true,
                role: UserRole::Member,
            })
            .unwrap();
        store.set_user_token(id, token).unwrap();
    }

    let library = store
        .create_library(
            MediaKind::Movie,
            "Private Movies",
            &[root.to_str().unwrap()],
            "selected",
            true,
            &[visible_user],
        )
        .unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|r| r.library_id == library.id)
        .unwrap()
        .id;

    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Two Versions".into(),
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

    let path_a = root.join("Two.Versions.1080p.mkv");
    let path_b = root.join("Two.Versions.2160p.mkv");
    std::fs::write(&path_a, b"1080p-bytes").unwrap();
    std::fs::write(&path_b, b"2160p-bytes").unwrap();

    // Only version B (2160p) has fonts and trickplay assets beside it.
    let fonts_dir = root.join("Two.Versions.2160p.fonts");
    std::fs::create_dir_all(&fonts_dir).unwrap();
    std::fs::write(fonts_dir.join("ass-font.ttf"), b"TTF_BYTES").unwrap();
    std::fs::write(
        root.join("Two.Versions.2160p.trickplay.jpg"),
        b"\xFF\xD8\xFF\xE0TRICKPLAY_2160P",
    )
    .unwrap();

    let file_a_id = LedgerId::new();
    let file_b_id = LedgerId::new();
    let lib_id: domain::LibraryId = library.id.parse().unwrap();
    let file_a = make_movie_file(file_a_id, lib_id, &root_id, media.id, &path_a);
    let file_b = make_movie_file(file_b_id, lib_id, &root_id, media.id, &path_b);
    let scope = WriteScope {
        library_id: library.id.parse().unwrap(),
        config_revision: store.library_config_revision(&library.id).unwrap(),
    };
    let publications = [&file_a, &file_b].map(|file| StorePublication {
        file,
        units: &[UnitKey::WHOLE],
        expected_version: None,
        nfo_observation: None,
        probe_required: false,
    });
    store.publish_files(&scope, &publications, 1_000).unwrap();
    drop(store);

    let state = api::ApiState::new(
        Store::open(&data).unwrap(),
        "admin-secret".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(NoopFetcher),
        Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
    )
    .unwrap();
    let app = api::router(state);
    SeededAssets {
        _tmp: tmp,
        app,
        file_without_assets: file_a_id,
        file_with_assets: file_b_id,
        visible_token: "visible-token".into(),
        hidden_token: "hidden-token".into(),
    }
}

#[tokio::test]
async fn real_provider_serves_fonts_and_trickplay_by_file_id_and_enforces_visibility() {
    let fx = seed_two_version_movie();

    // Visible user requesting version B's file ID gets its fonts and trickplay.
    let fonts_res = fx
        .app
        .clone()
        .oneshot(
            Request::get(format!(
                "/playback/files/{}/fonts?token={}",
                fx.file_with_assets, fx.visible_token
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(fonts_res.status(), StatusCode::OK);
    let fonts_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(fonts_res.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(fonts_json["fonts"][0], "ass-font.ttf");

    let trickplay_res = fx
        .app
        .clone()
        .oneshot(
            Request::get(format!(
                "/playback/files/{}/trickplay?token={}",
                fx.file_with_assets, fx.visible_token
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(trickplay_res.status(), StatusCode::OK);
    assert_eq!(
        trickplay_res.headers().get("content-type").unwrap(),
        "image/jpeg"
    );
    let bytes = axum::body::to_bytes(trickplay_res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&bytes[..], b"\xFF\xD8\xFF\xE0TRICKPLAY_2160P");

    // Version A has no fonts or trickplay beside it -> 404.
    for kind in ["fonts", "trickplay"] {
        let res = fx
            .app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/playback/files/{}/{kind}?token={}",
                    fx.file_without_assets, fx.visible_token
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    // Hidden user without Library access -> 404 on both endpoints.
    for kind in ["fonts", "trickplay"] {
        let res = fx
            .app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/playback/files/{}/{kind}?token={}",
                    fx.file_with_assets, fx.hidden_token
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    // Logical stream route still rejects bare file UUIDs.
    let stream_res = fx
        .app
        .oneshot(
            Request::get(format!(
                "/Videos/{}/stream?token={}",
                fx.file_with_assets, fx.visible_token
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stream_res.status(), StatusCode::NOT_FOUND);
}
