use api::{management::ApiState, router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
use std::sync::Arc;
use tower::ServiceExt;

struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}

fn request(method: &str, path: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", "Bearer secret")
        .body(Body::empty())
        .unwrap()
}

fn json_request(method: &str, path: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", "Bearer secret")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

fn stage(temp: &tempfile::TempDir) -> ApiState {
    ApiState::new(
        store::Store::open(temp.path().join("data")).unwrap(),
        "secret".into(),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(temp.path().join("stage"))),
    )
    .unwrap()
}

#[tokio::test]
async fn strict_language_contract_and_legacy_credentials_are_atomic() {
    let temp = tempfile::tempdir().unwrap();
    let state = stage(&temp);
    let app = router(state.clone());
    let old = app
        .clone()
        .oneshot(json_request(
            "PUT",
            "/api/v1/settings/metadata",
            serde_json::json!({"language":null,"tmdb_api_key":"do-not-write"}),
        ))
        .await
        .unwrap();
    assert_eq!(old.status(), StatusCode::BAD_REQUEST);
    assert!(state.store().get_setting("tmdb.api_key").unwrap().is_none());
    for body in [
        serde_json::json!({}),
        serde_json::json!({"language":null,"expected_revision":0}),
        serde_json::json!({"language":"ja","expected_revision":0,"extra":true}),
    ] {
        let res = app
            .clone()
            .oneshot(json_request(
                "PATCH",
                "/api/v1/settings/metadata-language",
                body,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }
    let stale = app
        .clone()
        .oneshot(json_request(
            "PATCH",
            "/api/v1/settings/metadata-language",
            serde_json::json!({"language":"en-US","expected_revision":9}),
        ))
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    let changed = app
        .clone()
        .oneshot(json_request(
            "PATCH",
            "/api/v1/settings/metadata-language",
            serde_json::json!({"language":"en-US","expected_revision":0}),
        ))
        .await
        .unwrap();
    assert_eq!(changed.status(), StatusCode::ACCEPTED);
    let data = body_json(changed).await;
    assert_eq!(data["data"]["revision"], 1);
    let get = app
        .clone()
        .oneshot(request("GET", "/api/v1/settings/metadata-language"))
        .await
        .unwrap();
    assert_eq!(body_json(get).await["data"]["language"], "en-US");
}

#[tokio::test]
async fn unchanged_language_preserves_admitted_work() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let state = stage(&temp);
    let app = router(state.clone());
    let first = app
        .clone()
        .oneshot(json_request(
            "PATCH",
            "/api/v1/settings/metadata-language",
            serde_json::json!({"expected_revision":0,"language":"en-US"}),
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let changed = body_json(first).await;
    for operation in changed["data"]["operations"].as_array().unwrap() {
        let poll = format!(
            "/api/v1/libraries/{}/operations/{}",
            operation["library_id"].as_str().unwrap(),
            operation["operation_id"].as_str().unwrap()
        );
        wait_for_operation(&app, &poll).await;
    }
    let completed = app
        .clone()
        .oneshot(request("GET", "/api/v1/settings/metadata-language"))
        .await
        .unwrap();
    let baseline = body_json(completed).await;
    let library = state
        .store()
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .unwrap();
    let work = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Ok(work) = state.acquire_library_work(&library.id) {
                break work;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let response = app
        .clone()
        .oneshot(json_request(
            "PATCH",
            "/api/v1/settings/metadata-language",
            serde_json::json!({"expected_revision":1,"language":" en-US "}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let current = body_json(response).await;
    assert_eq!(current["data"]["revision"], 1);
    assert_eq!(
        current["data"]["operations"],
        baseline["data"]["operations"]
    );
    assert!(!work.control().is_cancelled());
}

#[tokio::test]
async fn unchanged_language_keeps_pending_and_failed_operations_untouched() {
    for failed in [false, true] {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let state = stage(&temp);
        let library = state
            .store()
            .default_library(domain::MediaKind::Movie)
            .unwrap()
            .unwrap();
        let work = state.acquire_library_work(&library.id).unwrap();
        let changed = state
            .store()
            .change_metadata_language(0, "en-US", "same", 1)
            .unwrap();
        if failed {
            for operation in &changed.operations {
                state
                    .store()
                    .fail_library_operation_if_owned(&operation.operation_id, "fixture failure", 2)
                    .unwrap();
            }
        }
        let app = router(state.clone());
        let baseline = body_json(
            app.clone()
                .oneshot(request("GET", "/api/v1/settings/metadata-language"))
                .await
                .unwrap(),
        )
        .await;
        let response = app
            .oneshot(json_request(
                "PATCH",
                "/api/v1/settings/metadata-language",
                serde_json::json!({"expected_revision":1,"language":"en-US"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await["data"], baseline["data"]);
        assert!(!work.control().is_cancelled());
    }
}

#[tokio::test]
async fn accepted_language_operations_wait_for_capacity() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let state = stage(&temp);
    for index in 0..14 {
        let root = temp.path().join(format!("movie-{index}"));
        std::fs::create_dir_all(&root).unwrap();
        state
            .store()
            .create_library(
                domain::MediaKind::Movie,
                &format!("Movie {index}"),
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
    }
    let reservations = [
        state.fs_work.try_reserve().unwrap(),
        state.fs_work.try_reserve().unwrap(),
    ];
    let app = router(state.clone());
    let response = app
        .clone()
        .oneshot(json_request(
            "PATCH",
            "/api/v1/settings/metadata-language",
            serde_json::json!({"expected_revision":0,"language":"en-US"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = body_json(response).await;
    let operations = body["data"]["operations"].as_array().unwrap();
    assert_eq!(operations.len(), 16);
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    drop(reservations);
    for operation in operations {
        let library = operation["library_id"].as_str().unwrap();
        let id = operation["operation_id"].as_str().unwrap();
        let poll = format!("/api/v1/libraries/{library}/operations/{id}");
        wait_for_operation(&app, &poll).await;
        assert_eq!(
            state.store().library_lifecycle_state(library).unwrap(),
            "ready"
        );
    }
}

#[tokio::test]
async fn accepted_language_operations_compete_for_two_capacity_slots() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let state = stage(&temp);
    for index in 0..14 {
        let root = temp.path().join(format!("movie-{index}"));
        std::fs::create_dir_all(&root).unwrap();
        state
            .store()
            .create_library(
                domain::MediaKind::Movie,
                &format!("Movie {index}"),
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
    }
    let app = router(state.clone());
    let response = app
        .clone()
        .oneshot(json_request(
            "PATCH",
            "/api/v1/settings/metadata-language",
            serde_json::json!({"expected_revision":0,"language":"en-US"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = body_json(response).await;
    let operations = body["data"]["operations"].as_array().unwrap();
    assert_eq!(operations.len(), 16);
    for operation in operations {
        let library = operation["library_id"].as_str().unwrap();
        let id = operation["operation_id"].as_str().unwrap();
        wait_for_operation(
            &app,
            &format!("/api/v1/libraries/{library}/operations/{id}"),
        )
        .await;
        assert_eq!(
            state.store().library_lifecycle_state(library).unwrap(),
            "ready"
        );
    }
}

#[tokio::test]
async fn startup_recovery_waits_for_capacity_for_sixteen_operations() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let state = stage(&temp);
    for index in 0..14 {
        let root = temp.path().join(format!("movie-{index}"));
        std::fs::create_dir_all(&root).unwrap();
        state
            .store()
            .create_library(
                domain::MediaKind::Movie,
                &format!("Movie {index}"),
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
    }
    let changed = state
        .store()
        .change_metadata_language(0, "en-US", "recovery", 1)
        .unwrap();
    assert_eq!(changed.operations.len(), 16);
    let operations = changed
        .operations
        .into_iter()
        .map(|operation| (operation.library_id.to_string(), operation.operation_id))
        .collect::<Vec<_>>();
    drop(state);

    let restarted = stage(&temp);
    let reservations = [
        restarted.fs_work.try_reserve().unwrap(),
        restarted.fs_work.try_reserve().unwrap(),
    ];
    api::storage_operations::resume_library_reconfigurations(&restarted).unwrap();
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    drop(reservations);
    let app = router(restarted.clone());
    for (library, operation) in operations {
        wait_for_operation(
            &app,
            &format!("/api/v1/libraries/{library}/operations/{operation}"),
        )
        .await;
        assert_eq!(
            restarted.store().library_lifecycle_state(&library).unwrap(),
            "ready"
        );
    }
}

#[test]
fn driver_failure_cas_persists_only_for_the_current_frozen_owner() {
    let temp = tempfile::tempdir().unwrap();
    let state = stage(&temp);
    let changed = state
        .store()
        .change_metadata_language(0, "ja", "failure-cas", 0)
        .unwrap();
    let operation = &changed.operations[0];
    assert!(
        state
            .store()
            .fail_library_operation_if_owned(&operation.operation_id, "driver stopped", 20)
            .unwrap()
    );
    let failed = state
        .store()
        .library_operation_view(&operation.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(failed.state, "failed");
    assert_eq!(failed.error.as_deref(), Some("driver stopped"));
    assert_eq!(
        state
            .store()
            .library_lifecycle_state(&operation.library_id.to_string())
            .unwrap(),
        "reindexing"
    );
    assert!(
        !state
            .store()
            .fail_library_operation_if_owned(&operation.operation_id, "overwrite", 30)
            .unwrap()
    );
    assert_eq!(
        state
            .store()
            .library_operation_view(&operation.operation_id)
            .unwrap()
            .unwrap()
            .error
            .as_deref(),
        Some("driver stopped")
    );
}

#[tokio::test]
async fn retry_validates_library_owner_and_failed_state() {
    let temp = tempfile::tempdir().unwrap();
    let state = stage(&temp);
    let library = state
        .store()
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .unwrap();
    // Hold admitted work before freezing: retry must drain this lease before it
    // can finish and reopen ordinary configuration writes.
    let work = state.acquire_library_work(&library.id).unwrap();
    let library_id = library.id.parse::<domain::LibraryId>().unwrap();
    let changed = state
        .store()
        .change_metadata_language(0, "ja", "change", 0)
        .unwrap();
    let operation = changed
        .operations
        .iter()
        .find(|operation| operation.library_id == library_id)
        .unwrap();
    let app = router(state.clone());
    let route = format!(
        "/api/v1/libraries/{}/operations/{}/retry",
        operation.library_id, operation.operation_id
    );
    let res = app.clone().oneshot(request("POST", &route)).await.unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_foreign_retry_missing(&app, &changed, library_id, &operation.operation_id).await;
    let db = rusqlite::Connection::open(temp.path().join("data/library.db")).unwrap();
    db.execute(
        "UPDATE library_operations SET state='failed',error='fault' WHERE operation_id=?1",
        [&operation.operation_id],
    )
    .unwrap();
    let res = app.clone().oneshot(request("POST", &route)).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    assert_eq!(
        body_json(res).await["data"]["operation_id"],
        operation.operation_id
    );
    let running = app.clone().oneshot(request("POST", &route)).await.unwrap();
    assert_eq!(running.status(), StatusCode::CONFLICT);
    assert_eq!(
        body_json(running).await["error"]["code"],
        "library.operation_running"
    );
    let arbitrary = format!("/api/v1/libraries/{}", operation.library_id);
    // The narrow retry exception does not admit normal writes while draining.
    let res = app
        .clone()
        .oneshot(json_request(
            "PATCH",
            &arbitrary,
            serde_json::json!({"name":"should not apply"}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_eq!(
        state
            .store()
            .get_library(&library.id)
            .unwrap()
            .unwrap()
            .name,
        library.name
    );

    drop(work);
    wait_for_operation(
        &app,
        &format!(
            "/api/v1/libraries/{}/operations/{}",
            operation.library_id, operation.operation_id
        ),
    )
    .await;
    assert!(
        !state
            .store()
            .fail_library_operation_if_owned(&operation.operation_id, "late failure", 2)
            .unwrap()
    );
    let finished = state
        .store()
        .library_operation_view(&operation.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(finished.state, "succeeded");
    let res = app
        .oneshot(json_request(
            "PATCH",
            &arbitrary,
            serde_json::json!({"name":"after retry"}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        state
            .store()
            .get_library(&library.id)
            .unwrap()
            .unwrap()
            .name,
        "after retry"
    );
}

async fn assert_foreign_retry_missing(
    app: &axum::Router,
    changed: &store::MetadataLanguageState,
    library: domain::LibraryId,
    operation: &str,
) {
    let other = changed
        .operations
        .iter()
        .find(|other| other.library_id != library)
        .unwrap();
    let route = format!(
        "/api/v1/libraries/{}/operations/{operation}/retry",
        other.library_id
    );
    assert_eq!(
        app.clone()
            .oneshot(request("POST", &route))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
}

async fn wait_for_operation(app: &axum::Router, poll: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let response = app.clone().oneshot(request("GET", poll)).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = body_json(response).await;
            if body["data"]["state"] == "succeeded" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("retry must complete after admitted work drains");
}

#[tokio::test]
async fn language_freeze_is_unavailable_for_library_and_root_reads() {
    let temp = tempfile::tempdir().unwrap();
    let state = stage(&temp);
    let changed = state
        .store()
        .change_metadata_language(0, "ja", "change", 0)
        .unwrap();
    let app = router(state.clone());
    let root = app
        .clone()
        .oneshot(request(
            "GET",
            "/Items?Recursive=true&IncludeItemTypes=Movie",
        ))
        .await
        .unwrap();
    assert_eq!(root.status(), StatusCode::SERVICE_UNAVAILABLE);
    for op in changed.operations {
        let protocol = format!("/Items?ParentId={}&Recursive=true", op.library_id);
        assert_eq!(
            app.clone()
                .oneshot(request("GET", &protocol))
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        let path = format!("/api/v1/libraries/{}/items", op.library_id);
        assert_eq!(
            app.clone()
                .oneshot(request("GET", &path))
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}

struct OperationClock(std::sync::atomic::AtomicI64);
impl api::playback_events::clock::PlaybackClock for OperationClock {
    fn now_ms(&self) -> i64 {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[tokio::test]
async fn completed_language_operations_obey_thirty_day_retention() {
    const NOW: i64 = 1_800_000_000_000;
    const WINDOW: i64 = 30 * 86_400_000;
    let temp = tempfile::tempdir().unwrap();
    let clock = Arc::new(OperationClock(std::sync::atomic::AtomicI64::new(
        NOW - WINDOW - 1,
    )));
    let state = stage(&temp).with_playback_clock(clock.clone());
    let app = router(state.clone());
    let mut old_ids = Vec::new();
    let mut new_ids = Vec::new();
    for (revision, language, ids) in [(0, "en-US", &mut old_ids), (1, "ja", &mut new_ids)] {
        let response = app
            .clone()
            .oneshot(json_request(
                "PATCH",
                "/api/v1/settings/metadata-language",
                serde_json::json!({"expected_revision":revision,"language":language}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = body_json(response).await;
        for operation in body["data"]["operations"].as_array().unwrap() {
            let id = operation["operation_id"].as_str().unwrap().to_string();
            let library = operation["library_id"].as_str().unwrap();
            wait_for_operation(
                &app,
                &format!("/api/v1/libraries/{library}/operations/{id}"),
            )
            .await;
            ids.push(id);
        }
        clock.0.store(NOW, std::sync::atomic::Ordering::SeqCst);
    }
    let handle = state.store();
    let store = handle.as_ref();
    store
        .prune_terminal_operations(NOW - WINDOW, 100, false)
        .unwrap();
    for id in old_ids {
        assert!(store.library_operation_view(&id).unwrap().is_none());
    }
    for id in new_ids {
        let view = store
            .library_operation_view(&id)
            .unwrap()
            .expect("today's operation must survive retention");
        assert_eq!(view.updated_at_ms, NOW);
    }
}

#[path = "metadata_language_operation/driver_failure.rs"]
mod driver_failure;
