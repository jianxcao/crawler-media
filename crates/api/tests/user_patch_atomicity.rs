//! User patches must apply to the current row after acquiring the App writer.
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

use api::{ApiState, Store, router};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use domain::{User, UserId, UserRole};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::{Value, json};
use tower::ServiceExt;

const ADMIN_TOKEN: &str = "user-patch-admin";

struct Offline;
impl Fetcher for Offline {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    state: ApiState,
    member: UserId,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let state = ApiState::new(
            Store::open(temp.path().join("data")).unwrap(),
            ADMIN_TOKEN.into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(Offline),
            Arc::new(MemoryDownloader::new(temp.path().join("stage"))),
        )
        .unwrap();
        let member = User {
            id: UserId::new(),
            login: "member".into(),
            enabled: true,
            role: UserRole::Member,
        };
        state
            .store()
            .insert_user_with_password(&member, "original-password")
            .unwrap();
        state
            .store()
            .set_user_token(member.id, "member-session")
            .unwrap();
        Self {
            _temp: temp,
            state,
            member: member.id,
        }
    }

    async fn patch(&self, body: Value) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("PATCH")
            .uri(format!("/api/v1/users/{}", self.member))
            .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = router(self.state.clone()).oneshot(request).await.unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    async fn patch_after_concurrent_disable(&self, body: Value) -> (StatusCode, Value) {
        let store = self.state.store();
        let member = self.member;
        let (ready_tx, ready_rx) = mpsc::channel();
        let (disable_tx, disable_rx) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            store
                .writers()
                .with_app(|app| {
                    ready_tx.send(()).unwrap();
                    disable_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    let mut current = app.get_user(member)?.unwrap();
                    current.enabled = false;
                    app.save_user(&current)
                })
                .unwrap();
        });
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        let pool = self.state.store().reader_pool(store::Db::App);
        let reads = Arc::new(AtomicUsize::new(0));
        let observed = reads.clone();
        pool.set_query_progress_observer(Some(Arc::new(move || {
            observed.fetch_add(1, Ordering::SeqCst);
        })));
        let release_writer = async {
            // Authentication and role checks use two App read stages. A broken
            // patch also reads the target before waiting for the held writer.
            // Let that read finish, so the writer commits after its snapshot.
            // A fixed patch waits for the writer without the extra reader.
            let _ = tokio::time::timeout(Duration::from_millis(500), async {
                while reads.load(Ordering::SeqCst) < 3 || pool.available() != 4 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await;
            pool.set_query_progress_observer(None);
            disable_tx.send(()).unwrap();
            writer.join().unwrap();
        };
        let (response, ()) = tokio::join!(self.patch(body), release_writer);
        response
    }
}

#[test]
fn rename_patch_preserves_concurrent_member_disable() {
    assert_patch_preserves_disable(json!({"login": "renamed"}));
}

#[test]
fn password_patch_preserves_concurrent_member_disable() {
    assert_patch_preserves_disable(json!({"login": "renamed", "password": "new-password"}));
}

fn assert_patch_preserves_disable(patch: Value) {
    let fixture = Fixture::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let (status, body) = fixture.patch_after_concurrent_disable(patch).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["login"], "renamed");
        assert_eq!(
            body["data"]["enabled"], false,
            "patch must preserve the completed disable"
        );
        let store = fixture.state.store();
        let current = store.get_user(fixture.member).unwrap().unwrap();
        assert_eq!(current.login, "renamed");
        assert!(
            !current.enabled,
            "an unrelated patch must not reactivate a member"
        );
        assert!(store.user_by_token("member-session").unwrap().is_none());
    });
}
