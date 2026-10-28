use api::{ApiState, Store};
use domain::UserRole;
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media_server::provider::MediaServerProvider;
use std::sync::Arc;
use std::time::{Duration, Instant};
struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

// No runtime exists during construction, so unrelated background drivers stay off.
fn state_with_admin(root: &std::path::Path) -> (ApiState, String) {
    let state = ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "executor-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap();
    let user = domain::User {
        id: domain::UserId::new(),
        login: "artwork-admin".into(),
        enabled: true,
        role: UserRole::Admin,
    };
    let token = "artwork-admin-session".to_string();
    {
        let handle = state.store();
        let store = handle.lock();
        store
            .insert_user_with_password(&user, "admin-pass")
            .unwrap();
        store.set_user_token(user.id, &token).unwrap();
    }
    (state, token)
}

fn contended(kind: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let (state, token) = state_with_admin(tmp.path());
    let user = state
        .store()
        .lock()
        .user_by_token(&token)
        .unwrap()
        .unwrap()
        .id;
    let provider = api::media_server_provider::ApiServerProvider::new(state.clone());
    let store = state.store();
    let (tx, rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = store.lock();
        tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(500));
    });
    rx.recv().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let elapsed = runtime.block_on(async {
        let start = Instant::now();
        let (_, elapsed) = tokio::join!(
            async {
                exercise(&provider, user, kind).await;
            },
            async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                start.elapsed()
            }
        );
        elapsed
    });
    holder.join().unwrap();
    assert!(
        elapsed < Duration::from_millis(200),
        "{kind} parked executor for {elapsed:?}"
    );
}
#[test]
fn user_lookup_keeps_executor_free() {
    contended("user")
}
#[test]
fn device_lookup_keeps_executor_free() {
    contended("device")
}
#[test]
fn cover_lookup_keeps_executor_free() {
    contended("cover")
}

async fn exercise(
    provider: &api::media_server_provider::ApiServerProvider,
    user: domain::UserId,
    kind: &str,
) {
    let id = domain::TypedItemId::Work(domain::MediaId::new()).to_wire_string();
    match kind {
        "user" => assert!(provider.get_user(user).await.unwrap().is_some()),
        "device" => assert!(!provider.is_device_revoked(user, "device").await.unwrap()),
        "credential" => provider
            .bind_playback_credential(user, "device", "credential", None)
            .await
            .unwrap(),
        "cover" => assert!(
            provider
                .resolve_cover_bytes(Some(user), "missing")
                .await
                .unwrap()
                .is_none()
        ),
        "marks" => assert!(
            !provider
                .set_user_marks(user, &id, Some(true), None)
                .await
                .unwrap()
        ),
        "session" => assert!(
            provider
                .issue_play_session(user, "device", &id)
                .await
                .is_err()
        ),
        _ => unreachable!(),
    }
}
#[test]
fn marks_keep_executor_free() {
    contended("marks")
}
#[test]
fn session_keeps_executor_free() {
    contended("session")
}

#[test]
fn cancelled_read_holds_capacity_until_worker_finishes() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, token) = state_with_admin(tmp.path());
    let user = state
        .store()
        .lock()
        .user_by_token(&token)
        .unwrap()
        .unwrap()
        .id;
    let budget = Arc::new(api::blocking_budget::BlockingBudget::new(1));
    let state = state.with_http_work(budget.clone());
    let store = state.store();
    let (tx, rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = store.lock();
        tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(500));
    });
    rx.recv().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let provider = Arc::new(api::media_server_provider::ApiServerProvider::new(state));
        let task = tokio::spawn({
            let provider = provider.clone();
            async move { provider.get_user(user).await }
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(budget.available_permits(), 0);
        assert!(provider.get_user(user).await.is_err());
        task.abort();
        assert_eq!(budget.available_permits(), 0);
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(budget.available_permits(), 1);
        assert!(provider.get_user(user).await.unwrap().is_some());
    });
    holder.join().unwrap();
}

#[test]
fn credential_binding_keeps_executor_free() {
    contended("credential")
}

#[test]
fn saturated_marks_report_unavailable() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, _) = state_with_admin(tmp.path());
    let budget = Arc::new(api::blocking_budget::BlockingBudget::new(1));
    let _reservation = budget.try_reserve().unwrap();
    let provider = api::media_server_provider::ApiServerProvider::new(state.with_http_work(budget));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let error = runtime
        .block_on(provider.set_user_marks(domain::UserId::new(), "missing", Some(true), None))
        .unwrap_err();
    assert_eq!(
        error.status_code(),
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
}
