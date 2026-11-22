use std::{collections::HashMap, path::Path, sync::Arc};

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::{LedgerId, Media, MediaId, MediaKind, UnitKey, User, UserId, UserRole};
use downloader::MemoryDownloader;
use library::TransferMode;
use parking_lot::Mutex;
use serde_json::Value;
use store::library_write::WriteScope;
use store::test_support::{FixtureFile, publish_fixture_file_units};
use tower::ServiceExt;

use super::common;

pub(super) struct TestEnv {
    pub(super) _tmp: tempfile::TempDir,
    pub(super) state: ApiState,
    pub(super) app: axum::Router,
    pub(super) admin_id: UserId,
}

impl TestEnv {
    pub(super) fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let state = common::state(
            tmp.path(),
            Arc::new(common::Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        );
        let admin_id = state.store().admin_user_id().unwrap();
        let app = router(state.clone());
        Self {
            _tmp: tmp,
            state,
            app,
            admin_id,
        }
    }

    pub(super) fn root(&self) -> &Path {
        self._tmp.path()
    }

    pub(super) async fn call(
        &self,
        method: &str,
        uri: &str,
        token: &str,
        body: Value,
    ) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(common::request(method, uri, Some(token), body))
            .await
            .unwrap()
    }
}

pub(super) fn seed_member(store: &Store, login: &str) -> (UserId, String) {
    let id = UserId::new();
    store
        .insert_user_with_password(
            &User {
                id,
                login: login.into(),
                enabled: true,
                role: UserRole::Member,
            },
            "member-pass",
        )
        .unwrap();
    let token = format!("{login}-token");
    store.set_user_token(id, &token).unwrap();
    (id, token)
}

pub(super) fn seed_media(store: &Store, kind: MediaKind, title: &str) -> MediaId {
    let id = MediaId::new();
    store
        .insert_media(&Media {
            id,
            kind,
            title: title.into(),
            year: Some(2024),
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

pub(super) fn publish_file(
    store: &Store,
    media_id: MediaId,
    path: &Path,
    units: &[UnitKey],
) -> domain::LibraryFile {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"video").unwrap();
    publish_fixture_file_units(
        store,
        &FixtureFile {
            file_id: Some(LedgerId::new()),
            media_id,
            path: path.display().to_string(),
            ..Default::default()
        },
        units,
    )
    .unwrap()
}

pub(super) fn reserve_pending(
    store: &Store,
    library_id: domain::LibraryId,
    media_id: MediaId,
    path: &Path,
    units: &[UnitKey],
) -> LedgerId {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"pending").unwrap();
    let config_revision = store
        .library_config_revision(&library_id.to_string())
        .unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|r| r.library_id == library_id.to_string())
        .unwrap()
        .id;
    let scope = WriteScope {
        library_id,
        config_revision,
    };
    let file_id = LedgerId::new();
    let file = domain::LibraryFile {
        file_id,
        library_id,
        root_id,
        media_id,
        kind: if units.iter().any(|u| !u.is_whole()) {
            MediaKind::Tv
        } else {
            MediaKind::Movie
        },
        path: path.to_path_buf(),
        presence: domain::FilePresence::Present,
        publication: domain::FilePublication::Pending,
        version: domain::FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        size_bytes: Some(7),
        stat_known: true,
        created_ns: None,
        modified_ns: None,
        source_digest: None,
        source_path: None,
        transfer_mode: Some("copy".into()),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: domain::QualitySource::Probe,
        confidence: domain::Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: None,
        tracks_version: 0,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    };
    let cov = domain::FileCoverage {
        path_key: path.display().to_string(),
        file,
        units: units.to_vec(),
    };
    store
        .reserve_transfer(&scope, &cov, path, TransferMode::Copy, 1_000)
        .unwrap();
    file_id
}

pub(super) async fn assert_item_missing(resp: axum::response::Response, ctx: &str) {
    let status = resp.status();
    let err = common::json_error(resp).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{ctx}: {err}");
    assert_eq!(err["code"], "playback.item_missing", "{ctx}: {err}");
}

pub(super) fn bind_device(store: &Store, user_id: UserId, device_id: &str, token: &str) {
    store.set_user_token(user_id, token).unwrap();
    store
        .bind_playback_credential(user_id, device_id, token)
        .unwrap();
}

pub(super) async fn issue_session(env: &TestEnv, media_id: MediaId, device_id: &str) -> String {
    let r = env
        .call(
            "POST",
            "/api/v1/playback/sessions",
            "management-secret",
            serde_json::json!({"media_item_id": media_id, "device_id": device_id}),
        )
        .await;
    assert_eq!(r.status(), StatusCode::OK);
    common::json_data(r).await["play_session_id"]
        .as_str()
        .unwrap()
        .to_string()
}

pub(super) fn create_lib(
    store: &Store,
    kind: MediaKind,
    name: &str,
    dir: &Path,
    access: &str,
) -> domain::LibraryId {
    std::fs::create_dir_all(dir).unwrap();
    store
        .create_library(kind, name, &[dir.to_str().unwrap()], access, true, &[])
        .unwrap()
        .id
        .parse()
        .unwrap()
}
