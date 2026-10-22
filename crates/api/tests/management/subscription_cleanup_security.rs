//! Regression coverage for destructive subscription cleanup authorization and retry behavior.

use std::collections::HashMap;
use std::sync::Arc;

use api::{Store, router};
use axum::http::StatusCode;
use domain::{Confidence, LedgerId, MediaId, QualitySource};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::{Fixtures, json_body, request, state};
use store::test_support::{publish_fixture_file, FixtureFile};

fn app(tmp: &tempfile::TempDir) -> axum::Router {
    router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ))
}

async fn member_token(app: &axum::Router) -> String {
    app.clone()
        .oneshot(request(
            "POST",
            "/api/v1/users",
            Some("management-secret"),
            json!({ "login": "cleanup-member", "password": "member-pass" }),
        ))
        .await
        .unwrap();
    let login = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            json!({ "username": "cleanup-member", "password": "member-pass" }),
        ))
        .await
        .unwrap();
    json_body(login).await["data"]["token"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn create_subscription(app: &axum::Router, token: &str, title: &str) -> Value {
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some(token),
            json!({
                "media": { "kind": "movie", "title": title },
                "coverage": { "kind": "movie" }
            }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    json_body(created).await["data"].clone()
}

#[tokio::test]
async fn member_cleanup_flags_are_forbidden_without_deleting_subscription() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let member = member_token(&app).await;
    let created = create_subscription(&app, &member, "Keep Me").await;
    let id = created["id"].as_str().unwrap();
    let denied = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/subscriptions/{id}?delete_torrents=true"),
            Some(&member),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    let retained = app
        .oneshot(request(
            "GET",
            &format!("/api/v1/subscriptions/{id}"),
            Some(&member),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(retained.status(), StatusCode::OK);
}

#[tokio::test]
async fn failed_file_cleanup_keeps_subscription_for_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let created = create_subscription(&app, "management-secret", "Retry Me").await;
    let id = created["id"].as_str().unwrap();
    let media_id: MediaId = created["media"]["id"].as_str().unwrap().parse().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = store.library_root(domain::MediaKind::Movie).unwrap();
    let blocked = root.join("directory-cannot-be-removed-as-file");
    std::fs::create_dir_all(&blocked).unwrap();
    publish_fixture_file(
        &store,
        &FixtureFile {
            file_id: Some(LedgerId::new()),
            media_id,
            path: blocked.display().to_string(),
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

    let failed = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/subscriptions/{id}?delete_library_files=true"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(failed.status(), StatusCode::CONFLICT);
    let retained = app
        .oneshot(request(
            "GET",
            &format!("/api/v1/subscriptions/{id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(retained.status(), StatusCode::OK);
}

#[tokio::test]
async fn cleanup_does_not_delete_a_file_outside_every_library_root() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let created = create_subscription(&app, "management-secret", "Outside").await;
    let id = created["id"].as_str().unwrap();
    let media_id: MediaId = created["media"]["id"].as_str().unwrap().parse().unwrap();
    let outside = tmp.path().join("not-a-library/secret.mkv");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, b"keep-me").unwrap();
    // 旧库可能遗留库外台账行；publication API 现在拒绝这种行，用原始 SQL 复现
    // 该不一致状态，断言清理路径仍然拒绝删除库外文件。
    let store = Store::open(tmp.path().join("data")).unwrap();
    let library = store
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .expect("default movie library");
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|root| root.library_id.as_deref() == Some(library.id.as_str()))
        .expect("default movie root")
        .id;
    let file_id = LedgerId::new();
    drop(store);
    rusqlite::Connection::open(tmp.path().join("data/library.db"))
        .unwrap()
        .execute(
            "INSERT INTO library_files
                (file_id, library_id, root_id, media_id, kind, path_key, display_path,
                 presence_state, publication_state, file_revision, source_version,
                 quality_source, confidence)
             VALUES (?1, ?2, ?3, ?4, 'movie', ?5, ?5, 'present', 'published', 1, 'legacy',
                     'release', 'high')",
            rusqlite::params![
                file_id.to_string(),
                library.id,
                root_id,
                media_id.to_string(),
                outside.display().to_string(),
            ],
        )
        .unwrap();

    let deleted = app
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/subscriptions/{id}?delete_library_files=true"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    assert_eq!(std::fs::read(&outside).unwrap(), b"keep-me");
}
