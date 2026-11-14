//! Public image URLs resolve only the exact current published artifact.
#[path = "management/common.rs"]
mod common;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use api::chapter_frame_service::{chapter_frame_artifact_path, chapter_frame_generation};
use axum::http::StatusCode;
use domain::{LibraryFile, Media, MediaId, MediaKind, UnitKey};
use parking_lot::Mutex;
use serde_json::Value;
use store::test_support::{FixtureFile, publish_fixture_file_units};
use store::{ChapterFrameArtifact, ChapterFrameTargetResult};
use tower::ServiceExt;

struct Fixture {
    _temp: tempfile::TempDir,
    store: api::Store,
    app: axum::Router,
    state: api::ApiState,
    files: Vec<LibraryFile>,
}

fn chapter(start: i64, end: i64) -> library::ChapterMarker {
    library::ChapterMarker {
        start_ms: start,
        end_ms: end,
        title: None,
        marker_type: None,
        synthetic: false,
    }
}

impl Fixture {
    fn new(units: &[UnitKey]) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = api::Store::open(temp.path().join("data")).unwrap();
        let root = temp.path().join("episodes");
        std::fs::create_dir(&root).unwrap();
        store
            .create_library(
                MediaKind::Tv,
                "Episodes",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "Episodes".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&media).unwrap();
        let mut files = Vec::new();
        for name in ["first", "second"] {
            let path = root.join(format!("{name}.mkv"));
            std::fs::write(&path, b"registered fixture").unwrap();
            let file = publish_fixture_file_units(
                &store,
                &FixtureFile {
                    media_id: media.id,
                    path: path.display().to_string(),
                    ..Default::default()
                },
                units,
            )
            .unwrap();
            store
                .put_cached_chapters(
                    &file.file_id.to_string(),
                    &[
                        chapter(20_000, 30_000),
                        chapter(0, 10_000),
                        chapter(20_000, 30_000),
                    ],
                )
                .unwrap();
            files.push(file);
        }
        let fetcher = Arc::new(common::Fixtures {
            requests: Mutex::new(vec![]),
            bodies: HashMap::new(),
        });
        let downloader = Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage")));
        let state = common::state(temp.path(), fetcher, downloader);
        let app = api::router(state.clone());
        Self {
            _temp: temp,
            store,
            app,
            state,
            files,
        }
    }

    fn publish(&self, which: usize) -> Vec<(String, Vec<u8>, std::path::PathBuf)> {
        let file = &self.files[which];
        let current = self
            .store
            .current_chapter_frame_task(file.file_id)
            .unwrap()
            .unwrap();
        let status = self
            .store
            .ensure_chapter_frame_task(&current.task, true)
            .unwrap();
        let claim = self
            .store
            .claim_chapter_frame_task(&status.task_id)
            .unwrap()
            .unwrap();
        let mut frames = vec![];
        for target in &claim.targets {
            let artifact = target.candidate.as_ref().unwrap();
            let path =
                chapter_frame_artifact_path(&file.path, &claim.generation, target.index, artifact);
            image::RgbImage::from_pixel(
                2,
                2,
                image::Rgb([which as u8 * 100, (target.index % 8) as u8 * 30, 30]),
            )
            .save_with_format(&path, image::ImageFormat::Jpeg)
            .unwrap();
            let bytes = std::fs::read(&path).unwrap();
            let url = format!(
                "/chapters/{}/{}?generation={}&artifact={}-{}",
                file.file_id, target.index, claim.generation, artifact.attempt, artifact.id
            );
            frames.push((url, bytes, path));
        }
        assert!(
            self.store
                .finish_chapter_frame_attempt(
                    &claim.task.id,
                    claim.attempt,
                    &claim
                        .targets
                        .iter()
                        .filter(|target| !target.ready)
                        .map(|target| ready(target.index))
                        .collect::<Vec<_>>()
                )
                .unwrap()
        );
        frames
    }

    fn register_replacement_source(&self, which: usize) -> LibraryFile {
        let file = &self.files[which];
        let observation = library::observe_source(&file.path).unwrap();
        let scope = store::library_write::WriteScope {
            library_id: file.library_id,
            config_revision: self
                .store
                .library_config_revision(&file.library_id.to_string())
                .unwrap(),
        };
        self.store
            .replace_file_source(&scope, file.file_id, &file.version, &observation, 1)
            .unwrap();
        self.store
            .file_coverage_by_id(file.file_id)
            .unwrap()
            .unwrap()
            .file
    }

    async fn get(&self, url: &str) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(common::request(
                "GET",
                &format!("/api/v1{url}"),
                None,
                Value::Null,
            ))
            .await
            .unwrap()
    }
    async fn chapters(&self, unit: UnitKey) -> Vec<Value> {
        let file = &self.files[0];
        let url = format!(
            "/api/v1/libraries/{}/items/{}/chapters?file_id={}&season={}&episode={}",
            file.library_id, file.media_id, file.file_id, unit.season, unit.episode
        );
        let response = self
            .app
            .clone()
            .oneshot(common::request(
                "GET",
                &url,
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        common::json_body(response).await["data"]
            .as_array()
            .unwrap()
            .clone()
    }
}

fn ready(index: usize) -> ChapterFrameTargetResult {
    ChapterFrameTargetResult {
        index,
        ready: true,
        error: None,
    }
}

#[test]
fn artifact_paths_preserve_identity_for_long_legal_source_names() {
    let file = domain::LedgerId::new();
    let generation = chapter_frame_generation(file, "v1", &[(0, 10)], 1);
    let video = std::path::PathBuf::from("/tmp").join(format!("{}.mkv", "界".repeat(80)));
    let artifact = ChapterFrameArtifact {
        attempt: 1,
        id: uuid::Uuid::new_v4().simple().to_string(),
    };
    let path = chapter_frame_artifact_path(&video, &generation, 0, &artifact);
    assert!(
        path.file_name().unwrap().len() <= 255,
        "legal source names must yield legal artifact names: {}",
        path.display()
    );
    let changed = chapter_frame_generation(file, "v2", &[(0, 10)], 1);
    assert_ne!(
        path,
        chapter_frame_artifact_path(&video, &changed, 0, &artifact)
    );
    assert_ne!(
        path,
        chapter_frame_artifact_path(Path::new("/tmp/other.mkv"), &generation, 0, &artifact)
    );
    for changed in [
        chapter_frame_generation(file, "v1", &[(0, 20)], 1),
        chapter_frame_generation(file, "v1", &[(0, 10)], 2),
    ] {
        assert_ne!(
            path,
            chapter_frame_artifact_path(&video, &changed, 0, &artifact)
        );
    }
}

#[test]
fn public_images_require_current_query_identity_and_canonical_target_mapping() {
    let f = Fixture::new(&[UnitKey::new(1, 1)]);
    let first = f.publish(0);
    let second = f.publish(1);
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        assert_ne!(
            f.get(&format!("/chapters/{}/0", f.files[0].file_id))
                .await
                .status(),
            StatusCode::OK
        );
        let items = f.chapters(UnitKey::new(1, 1)).await;
        for item in &items {
            let index = if item["start_ms"] == 0 { 0 } else { 1 };
            assert_eq!(item["image_url"], first[index].0);
        }
        for (url, expected, _) in first.iter().chain(second.iter()) {
            let response = f.get(url).await;
            assert_eq!(response.status(), StatusCode::OK, "{url}");
            assert_eq!(
                axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap()
                    .as_ref(),
                expected
            );
        }
        let wrong_file = first[0].0.replace(
            &f.files[0].file_id.to_string(),
            &f.files[1].file_id.to_string(),
        );
        assert_eq!(f.get(&wrong_file).await.status(), StatusCode::NOT_FOUND);
        f.store
            .put_cached_chapters(&f.files[0].file_id.to_string(), &[chapter(5_000, 15_000)])
            .unwrap();
        assert_eq!(f.get(&first[0].0).await.status(), StatusCode::NOT_FOUND);
        assert!(
            f.chapters(UnitKey::new(1, 1))
                .await
                .iter()
                .all(|item| item["image_url"].is_null())
        );
    });
}

#[test]
fn missing_or_corrupt_published_images_are_not_advertised_or_served() {
    let f = Fixture::new(&[UnitKey::new(1, 1)]);
    let frames = f.publish(0);
    std::fs::remove_file(&frames[0].2).unwrap();
    std::fs::write(&frames[1].2, b"not a JPEG").unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        assert!(
            f.chapters(UnitKey::new(1, 1))
                .await
                .iter()
                .all(|item| item["image_url"].is_null())
        );
        for (url, _, _) in frames {
            assert_eq!(f.get(&url).await.status(), StatusCode::NOT_FOUND);
        }
    });
}

#[cfg(unix)]
#[test]
fn ffmpeg_clamps_midpoint_to_registered_duration_and_reports_invalid_targets() {
    use api::chapter_frame_service::{
        ChapterFrameRequest, ChapterFrameRunner, worker::FfmpegChapterFrameRunner,
    };
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let video = temp.path().join("video.mkv");
    std::fs::write(&video, b"fixture").unwrap();
    let jpeg = temp.path().join("fixture.jpg");
    image::RgbImage::from_pixel(2, 2, image::Rgb([30, 60, 90]))
        .save_with_format(&jpeg, image::ImageFormat::Jpeg)
        .unwrap();
    let args = temp.path().join("args");
    let exe = temp.path().join("fake-ffmpeg");
    std::fs::write(&exe,format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nfor arg; do out=\"$arg\"; done\ncp '{}' \"$out\"\n",args.display(),jpeg.display())).unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let request = ChapterFrameRequest {
        file_id: domain::LedgerId::new(),
        video,
        source_version: "registered".into(),
        generation: "generation".into(),
        attempt: 1,
        targets: vec![(15_000, 25_000), (5, 5)],
        duration_ms: Some(10_000),
        pending_indices: vec![0, 1],
        candidates: (0..2)
            .map(|index| {
                (
                    index,
                    ChapterFrameArtifact {
                        attempt: 1,
                        id: uuid::Uuid::new_v4().simple().to_string(),
                    },
                )
            })
            .collect(),
    };
    let results = FfmpegChapterFrameRunner::new(exe.to_str().unwrap())
        .render(&request, &library::WorkControl::default())
        .unwrap();
    assert!(results[0].staged.is_ok());
    assert_eq!(
        results[1].staged.as_ref().unwrap_err(),
        "invalid_frame_timestamp"
    );
    let args = std::fs::read_to_string(args).unwrap();
    assert!(args.contains("-ss\n9.999\n"), "{args}");
}

#[path = "chapter_frame_identity/service.rs"]
mod service;

#[path = "chapter_frame_identity/selection.rs"]
mod selection;

#[path = "chapter_frame_identity/real.rs"]
mod real;

#[path = "chapter_frame_identity/cold.rs"]
mod cold;

#[path = "chapter_frame_identity/metadata.rs"]
mod metadata;

#[path = "chapter_frame_identity/status.rs"]
mod status;

#[path = "chapter_frame_identity/status_selection.rs"]
mod status_selection;

#[path = "chapter_frame_identity/browser.rs"]
mod browser;
