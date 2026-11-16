//! Library media information refresh selects files without altering visible facts.
use std::collections::HashMap;
use std::sync::Arc;

use api::{ApiState, router};
use axum::http::StatusCode;
use domain::{LibraryFile, Media, MediaId, MediaKind};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use store::test_support::{FixtureFile, publish_fixture_file};
use tower::ServiceExt;

#[path = "management/common.rs"]
mod common;
use common::{Fixtures, json_body, request};

struct Fixture {
    _tmp: tempfile::TempDir,
    state: ApiState,
    library: String,
    media: MediaId,
}

impl Fixture {
    // Construct outside Tokio to inspect durable admission without a worker race.
    fn new() -> Self {
        let tmp = tempfile::tempdir_in("/tmp").unwrap();
        let state = common::state(
            tmp.path(),
            Arc::new(Fixtures {
                requests: Mutex::new(vec![]),
                bodies: HashMap::new(),
            }),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        );
        let store = state.store();
        let library = store.default_library(MediaKind::Tv).unwrap().unwrap();
        std::fs::create_dir_all(&library.root_paths[0]).unwrap();
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "Media information".into(),
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
        Self {
            _tmp: tmp,
            state,
            library: library.id,
            media: media.id,
        }
    }

    fn file(&self, episode: u32) -> LibraryFile {
        let store = self.state.store();
        let library = store.get_library(&self.library).unwrap().unwrap();
        let path = library.root_paths[0].join(format!("Episode.{episode}.mkv"));
        std::fs::write(&path, b"fixture video").unwrap();
        publish_fixture_file(
            &store,
            &FixtureFile {
                media_id: self.media,
                path: path.display().to_string(),
                season: Some(1),
                episode: Some(episode),
                ..Default::default()
            },
        )
        .unwrap()
    }

    fn cache(&self, file: &LibraryFile, duration: Option<i64>) {
        self.state
            .store()
            .put_file_meta_versioned(
                &file.file_id.to_string(),
                &library::Tracks {
                    video: Some(library::VideoTrack::default()),
                    ..Default::default()
                },
                Some(&file.version.source_version),
                duration,
            )
            .unwrap();
    }

    fn refresh(&self, mut body: Value) -> (StatusCode, Value) {
        if body.get("request_id").is_none() {
            body["request_id"] = json!(uuid::Uuid::new_v4().to_string());
        }
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let app = router(self.state.clone());
            let mut totals = [0usize; 3];
            loop {
                let response = app
                    .clone()
                    .oneshot(request(
                        "POST",
                        &format!("/api/v1/libraries/{}/media-info/refresh", self.library),
                        Some("management-secret"),
                        body.clone(),
                    ))
                    .await
                    .unwrap();
                let status = response.status();
                let mut result = json_body(response).await;
                if status != StatusCode::OK {
                    return (status, result);
                }
                assert!(result["data"]["total"].as_u64().unwrap() <= 50);
                for (i, key) in ["total", "queued", "skipped"].into_iter().enumerate() {
                    totals[i] += result["data"][key].as_u64().unwrap() as usize;
                }
                match result["data"]
                    .as_object_mut()
                    .unwrap()
                    .remove("next_cursor")
                {
                    Some(Value::String(cursor)) => body["after"] = json!(cursor),
                    Some(Value::Null) => {
                        for (i, key) in ["total", "queued", "skipped"].into_iter().enumerate() {
                            result["data"][key] = json!(totals[i]);
                        }
                        return (status, result);
                    }
                    _ => panic!("bounded admission must return a continuation cursor"),
                }
            }
        })
    }
}

#[test]
fn missing_refresh_skips_valid_information_and_queues_absent_or_incomplete_information() {
    let fixture = Fixture::new();
    let valid = fixture.file(1);
    fixture.cache(&valid, Some(120_000));
    let absent = fixture.file(2);
    let incomplete = fixture.file(3);
    fixture.cache(&incomplete, None);
    let (status, body) = fixture.refresh(json!({ "mode": "missing" }));
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"],
        json!({ "mode": "missing", "total": 3, "queued": 2, "skipped": 1 })
    );
    let store = fixture.state.store();
    assert!(
        store
            .latest_probe_job_for_ledger(&valid.file_id.to_string())
            .unwrap()
            .is_none()
    );
    for file in [absent, incomplete] {
        let job = store
            .latest_probe_job_for_ledger(&file.file_id.to_string())
            .unwrap()
            .unwrap();
        assert_eq!(
            store.get_probe_job(&job).unwrap().unwrap().kind,
            "media_probe"
        );
    }
}

#[test]
fn force_refresh_includes_cached_files_preserves_results_and_deduplicates_active_work() {
    let fixture = Fixture::new();
    let file = fixture.file(1);
    fixture.cache(&file, Some(120_000));
    let store = fixture.state.store();
    let id = file.file_id.to_string();
    let before = store.get_file_meta(&id).unwrap();
    let (status, body) = fixture.refresh(json!({ "mode": "force" }));
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["queued"], 1);
    let job = store.latest_probe_job_for_ledger(&id).unwrap().unwrap();
    assert_eq!(
        store.get_probe_job(&job).unwrap().unwrap().kind,
        "media_probe",
        "disabled intro detection must not become a marker refresh"
    );
    let units = store.probe_job_units(&job).unwrap();
    assert!(!units[0].reuse_media_info_cache);
    assert!(!units[0].overwrite_markers);
    assert_eq!(store.get_file_meta(&id).unwrap(), before);
    let (status, body) = fixture.refresh(json!({ "mode": "force" }));
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["queued"], 0);
    assert_eq!(body["data"]["skipped"], 1);
    assert_eq!(
        store.latest_probe_job_for_ledger(&id).unwrap().unwrap(),
        job
    );
}

#[test]
fn refresh_rejects_invalid_modes_and_read_only_instances() {
    let fixture = Fixture::new();
    let file = fixture.file(1);
    for body in [
        json!({}),
        json!({ "mode": "all" }),
        json!({ "mode": "force", "extra": true }),
    ] {
        let (status, _) = fixture.refresh(body);
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    fixture.state.clone().with_instance_writable(false);
    let (status, body) = fixture.refresh(json!({ "mode": "force" }));
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "instance.read_only");
    assert!(
        fixture
            .state
            .store()
            .latest_probe_job_for_ledger(&file.file_id.to_string())
            .unwrap()
            .is_none()
    );
}

#[test]
fn library_refresh_visits_all_pages_and_keeps_other_libraries_and_missing_files_untouched() {
    let fixture = Fixture::new();
    let files = (1..=205)
        .map(|episode| fixture.file(episode))
        .collect::<Vec<_>>();
    let store = fixture.state.store();
    let missing = store
        .file_coverage_by_id(files[0].file_id)
        .unwrap()
        .unwrap();
    store
        .apply_ledger_presence(
            &[store::LedgerPresenceObservation {
                path_key: missing.path_key.clone(),
                file: missing,
                missing_at: None,
                presence: domain::FilePresence::Missing,
            }],
            store::LedgerPresenceMode::Verify,
            1,
        )
        .unwrap();
    let other_root = fixture._tmp.path().join("other-tv");
    std::fs::create_dir_all(&other_root).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Other",
            &[other_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let other_path = other_root.join("Same.Media.S01E01.mkv");
    std::fs::write(&other_path, b"other file").unwrap();
    let other = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: fixture.media,
            path: other_path.display().to_string(),
            season: Some(1),
            episode: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    let (status, body) = fixture.refresh(json!({ "mode": "force" }));
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"],
        json!({ "mode": "force", "total": 205, "queued": 204, "skipped": 1 })
    );
    assert!(
        store
            .latest_probe_job_for_ledger(&other.file_id.to_string())
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .latest_probe_job_for_ledger(&files[0].file_id.to_string())
            .unwrap()
            .is_none()
    );
    for file in &files[1..] {
        assert!(
            store
                .latest_probe_job_for_ledger(&file.file_id.to_string())
                .unwrap()
                .is_some()
        );
    }
}

#[test]
fn media_info_refresh_requires_admin_and_rejects_unknown_library() {
    let fixture = Fixture::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let app = router(fixture.state.clone());
        let (_, member) = common::create_member(&app, "viewer").await;
        for (id, token, expected) in [
            (fixture.library.clone(), None, StatusCode::UNAUTHORIZED),
            (
                fixture.library.clone(),
                Some(member.as_str()),
                StatusCode::FORBIDDEN,
            ),
            (
                domain::LibraryId::new().to_string(),
                Some("management-secret"),
                StatusCode::NOT_FOUND,
            ),
        ] {
            let response = app
                .clone()
                .oneshot(request(
                    "POST",
                    &format!("/api/v1/libraries/{id}/media-info/refresh"),
                    token,
                    json!({ "mode": "force", "request_id": uuid::Uuid::new_v4().to_string() }),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }
    });
}

#[test]
fn retrying_the_same_force_request_does_not_reprobe_a_completed_file() {
    let fixture = Fixture::new();
    let file = fixture.file(1);
    let body = json!({ "mode": "force", "request_id": uuid::Uuid::new_v4().to_string() });
    let (status, receipt) = fixture.refresh(body.clone());
    assert_eq!(status, StatusCode::OK, "{receipt}");
    let store = fixture.state.store();
    let id = file.file_id.to_string();
    let job = store.latest_probe_job_for_ledger(&id).unwrap().unwrap();
    assert_eq!(
        store
            .active_probe_unit_for_ledger(&id)
            .unwrap()
            .unwrap()
            .job_id,
        job
    );
    assert_eq!(
        store
            .active_probe_job_for_scope(&format!("ledger:{id}"))
            .unwrap()
            .unwrap()
            .id,
        job
    );
    store.start_probe_unit(&job, &id).unwrap();
    store.finish_probe_unit(&job, &id, true, None).unwrap();
    store.finish_probe_job(&job, true, None).unwrap();
    let (status, replay) = fixture.refresh(body);
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay, receipt);
    assert_eq!(
        store.latest_probe_job_for_ledger(&id).unwrap().unwrap(),
        job
    );
    assert_eq!(
        store.get_probe_job(&job).unwrap().unwrap().status,
        "succeeded"
    );
}

#[test]
fn changed_sources_require_a_scan_instead_of_queuing_an_obsolete_version() {
    for mode in ["missing", "force"] {
        let fixture = Fixture::new();
        let file = fixture.file(1);
        std::fs::write(&file.path, b"replacement content of a different size").unwrap();
        let (status, body) = fixture.refresh(json!({ "mode": mode }));
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"]["code"], "library.probe_source_changed");
        assert!(
            fixture
                .state
                .store()
                .latest_probe_job_for_ledger(&file.file_id.to_string())
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn replay_keeps_files_skipped_for_active_work_skipped_after_that_work_finishes() {
    let fixture = Fixture::new();
    let file = fixture.file(1);
    assert_eq!(
        fixture.refresh(json!({ "mode": "force" })).0,
        StatusCode::OK
    );
    let body = json!({ "mode": "force", "request_id": uuid::Uuid::new_v4().to_string() });
    let (status, receipt) = fixture.refresh(body.clone());
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(receipt["data"]["skipped"], 1);
    let store = fixture.state.store();
    let id = file.file_id.to_string();
    let job = store.latest_probe_job_for_ledger(&id).unwrap().unwrap();
    assert_eq!(
        store
            .active_probe_unit_for_ledger(&id)
            .unwrap()
            .unwrap()
            .job_id,
        job
    );
    assert_eq!(
        store
            .active_probe_job_for_scope(&format!("ledger:{id}"))
            .unwrap()
            .unwrap()
            .id,
        job
    );
    store.start_probe_unit(&job, &id).unwrap();
    store.finish_probe_unit(&job, &id, true, None).unwrap();
    store.finish_probe_job(&job, true, None).unwrap();
    let (status, replay) = fixture.refresh(body);
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay, receipt, "replay must preserve the skip decision");
    assert_eq!(
        store.latest_probe_job_for_ledger(&id).unwrap().unwrap(),
        job
    );
    store.reset_running_probe_units().unwrap();
    assert!(
        store
            .active_probe_jobs_by_kind("media_probe")
            .unwrap()
            .is_empty()
    );
}

#[path = "library_media_info_refresh/worker.rs"]
mod worker;
