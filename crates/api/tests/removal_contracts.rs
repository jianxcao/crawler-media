use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::Request;
use domain::{User, UserId, UserRole};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::{Value, json};
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

fn state(tmp: &tempfile::TempDir) -> ApiState {
    ApiState::new(
        Store::open(tmp.path().join("data")).unwrap(),
        "admin-token".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    )
    .unwrap()
}

#[test]
fn constructing_api_state_does_not_create_an_unused_library_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let _state = state(&tmp);
    assert!(!tmp.path().join("unused-default-library").exists());
}

async fn create_owned_subscribe(state: &ApiState) -> (User, String) {
    let member = User {
        id: UserId::new(),
        login: "member".into(),
        enabled: true,
        role: UserRole::Member,
    };
    let store = state.store();
    store
        .insert_user_with_password(&member, "member-password")
        .unwrap();
    store.set_user_token(member.id, "member-token").unwrap();
    let response = router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/subscriptions")
                .header("content-type", "application/json")
                .header("authorization", "Bearer member-token")
                .body(Body::from(
                    json!({
                        "media": {"kind": "movie", "title": "Fixture"},
                        "coverage": {"kind": "movie"},
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::CREATED);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    (member, body["data"]["id"].as_str().unwrap().to_owned())
}

async fn assert_recovery_version(version: Option<u32>) {
    let tmp = tempfile::tempdir().unwrap();
    let state = state(&tmp);
    let (member, subscribe) = create_owned_subscribe(&state).await;
    let store = state.store();
    let key = format!("user_removal.{}", member.id);
    let mut record = json!({
        "operation_id": "version-contract", "user_id": member.id.to_string(),
        "phase": "disabling", "cursor": null, "error": null, "updated_at": 1,
    });
    if let Some(version) = version {
        record["version"] = json!(version);
    }
    let raw = record.to_string();
    store.put_setting(&key, &raw).unwrap();
    let recovered = api::user_removal::UserRemovalService::new(Arc::new(state.clone()))
        .recover_pending_removals();
    if version == Some(1) {
        recovered.unwrap();
        assert!(store.get_user(member.id).unwrap().is_none());
        assert!(store.user_by_token("member-token").unwrap().is_none());
        assert!(store.get_setting(&key).unwrap().is_none());
        assert_eq!(
            store
                .get_subscribe(subscribe.parse().unwrap())
                .unwrap()
                .unwrap()
                .user_id,
            store.admin_user_id().unwrap()
        );
        return;
    }
    assert!(
        recovered.is_err(),
        "unsupported {version:?} record must fail closed"
    );
    assert_eq!(store.get_user(member.id).unwrap(), Some(member.clone()));
    assert_eq!(
        store.user_by_token("member-token").unwrap(),
        Some(member.clone())
    );
    assert!(store.login_credential(&member.login).unwrap().is_some());
    assert_eq!(
        store
            .get_subscribe(subscribe.parse().unwrap())
            .unwrap()
            .unwrap()
            .user_id,
        member.id
    );
    assert_eq!(
        store.get_setting(&key).unwrap().as_deref(),
        Some(raw.as_str())
    );
    assert!(
        store
            .get_setting("user_removal.result.version-contract")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_setting(&format!("user_removal.done.{}", member.id))
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn user_recovery_rejects_missing_record_version() {
    assert_recovery_version(None).await;
}

#[tokio::test]
async fn user_recovery_rejects_zero_record_version() {
    assert_recovery_version(Some(0)).await;
}

#[tokio::test]
async fn user_recovery_rejects_unknown_record_version() {
    assert_recovery_version(Some(2)).await;
}

#[tokio::test]
async fn user_recovery_resumes_supported_record_version() {
    assert_recovery_version(Some(1)).await;
}
