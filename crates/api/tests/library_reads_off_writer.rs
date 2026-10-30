//! Task 3 RED draft: writer-independent reads and safe selected-source races.
mod writer_hold {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/writer_hold.rs"
    ));
}
#[path = "library_reads_off_writer/probe_admission.rs"]
mod probe_admission;
#[path = "library_reads_off_writer/support.rs"]
mod support;

use axum::http::StatusCode;
use domain::TypedItemId;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use support::{Fixture, HeldWriter, get_json};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jellyfin_items_page_is_served_while_writer_is_held() {
    let fixture = Fixture::new(60);
    let user = fixture.user();
    let app = api::router(fixture.state.clone());
    let page_uri = format!(
        "/Users/{user}/Items?ParentId={}&Recursive=true&Limit=50",
        fixture.library
    );
    let latest_uri = format!(
        "/Users/{user}/Items/Latest?ParentId={}&Limit=50",
        fixture.library
    );
    let writer = HeldWriter::acquire(&fixture.state);
    let result = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(get_json(app.clone(), &page_uri), get_json(app, &latest_uri))
    })
    .await;
    // Release before assertions, including the timeout path. The writer stayed
    // held until both response bodies completed, not merely until dispatch.
    drop(writer);
    let ((page_status, page), (latest_status, latest)) =
        result.expect("Items and Latest must finish while the writer stays locked");
    assert_eq!(page_status, StatusCode::OK, "{page}");
    assert_eq!(latest_status, StatusCode::OK, "{latest}");
    assert_eq!(page["Items"].as_array().unwrap().len(), 50);
    assert_eq!(latest.as_array().unwrap().len(), 50);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn items_page_drops_file_deleted_between_prepare_and_validate() {
    let mut fixture = Fixture::new(3);
    let observed = Arc::new(Mutex::new(Vec::new()));
    let deleted = observed.clone();
    let writer = fixture.state.store();
    fixture.state = fixture
        .state
        .clone()
        .with_protocol_selection_barrier(Arc::new(move |page| {
            let mut deleted = deleted.lock().unwrap();
            if !deleted.is_empty() {
                return;
            }
            let selected = page.items.first().expect("fixture page contains items");
            let source = selected.preferred_source.as_ref().unwrap();
            assert_eq!(
                writer
                    .as_ref()
                    .delete_files(selected.library_id, &[source.file_id])
                    .unwrap(),
                1
            );
            deleted.push(selected.item.to_wire_string());
        }));
    let (status, body) = get_json(api::router(fixture.state.clone()), &fixture.items_uri()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let deleted = observed.lock().unwrap();
    assert_eq!(deleted.len(), 1);
    let returned = support::item_ids(&body);
    assert_eq!(returned.len(), 2);
    assert!(!returned.contains(&deleted[0]));
    let expected: BTreeSet<_> = fixture
        .files
        .iter()
        .map(|file| TypedItemId::Work(file.media_id).to_wire_string())
        .filter(|id| !deleted.contains(id))
        .collect();
    assert_eq!(returned, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_consecutive_source_changes_return_safe_200_without_third_prepare() {
    let mut fixture = Fixture::new(6);
    let attempts = Arc::new(AtomicUsize::new(0));
    let entered = attempts.clone();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let deleted = observed.clone();
    let writer = fixture.state.store();
    fixture.state = fixture
        .state
        .clone()
        .with_protocol_selection_barrier(Arc::new(move |page| {
            let attempt = entered.fetch_add(1, Ordering::SeqCst);
            if attempt >= 2 {
                return;
            }
            let selected = page.items.first().expect("fixture page contains items");
            let source = selected.preferred_source.as_ref().unwrap();
            assert_eq!(
                writer
                    .as_ref()
                    .delete_files(selected.library_id, &[source.file_id])
                    .unwrap(),
                1
            );
            deleted.lock().unwrap().push(selected.item.to_wire_string());
        }));
    let response = tokio::time::timeout(
        Duration::from_secs(2),
        get_json(api::router(fixture.state.clone()), &fixture.items_uri()),
    )
    .await
    .expect("source changes may retry only once");
    let (status, body) = response;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    let deleted = observed.lock().unwrap();
    assert_eq!(deleted.len(), 2);
    let returned = support::item_ids(&body);
    let expected: BTreeSet<_> = fixture
        .files
        .iter()
        .map(|file| TypedItemId::Work(file.media_id).to_wire_string())
        .filter(|id| !deleted.contains(id))
        .collect();
    assert_eq!(returned, expected);
    assert_eq!(returned.len(), 4);
    assert!(body["TotalRecordCount"].as_u64().unwrap() >= returned.len() as u64);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observed_session_device_remains_revocable_and_bound_reads_skip_writer() {
    use media_server::provider::MediaServerProvider;
    let fixture = Fixture::new(3);
    let user = fixture.user();
    let uri = fixture.items_uri();
    let token = "task3-ordinary-session";
    fixture.state.store().set_user_token(user, token).unwrap();
    let provider = api::media_server_provider::ApiServerProvider::new(fixture.state.clone());
    provider
        .bind_playback_credential(user, "task3-reader", token, None)
        .await
        .unwrap();
    assert!(
        fixture
            .state
            .store()
            .device_is_revocable(user, "task3-reader")
            .unwrap()
    );
    let writer = HeldWriter::acquire(&fixture.state);
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        support::get_json_with_token(api::router(fixture.state.clone()), &uri, token),
    )
    .await;
    drop(writer);
    let (status, body) = result.expect("observed device listing must not wait for writer");
    assert_eq!(status, StatusCode::OK, "{body}");
    fixture
        .state
        .store()
        .revoke_device(user, "task3-reader")
        .unwrap();
    assert!(
        provider
            .user_id_by_token(token, None)
            .await
            .unwrap()
            .is_none()
    );
}

struct PausedReaderRelease(Arc<(Mutex<bool>, std::sync::Condvar)>);
impl Drop for PausedReaderRelease {
    fn drop(&mut self) {
        let (released, changed) = self.0.as_ref();
        *released.lock().unwrap() = true;
        changed.notify_all();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exhausted_reader_pool_obeys_deadline_without_starting_a_late_worker() {
    use axum::body::Body;
    use axum::http::Request;
    use media_server::provider::QueryTiming;
    use tower::ServiceExt;
    let mut fixture = Fixture::new(1);
    let release = PausedReaderRelease(Arc::new((Mutex::new(false), std::sync::Condvar::new())));
    let gate = release.0.clone();
    let entered = Arc::new(AtomicUsize::new(0));
    let observed = entered.clone();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    fixture.state = fixture
        .state
        .clone()
        .with_library_read_stage_barrier(Arc::new(move |stage| {
            if stage != "prepare" {
                return;
            }
            observed.fetch_add(1, Ordering::SeqCst);
            tx.send(()).unwrap();
            let (released, changed) = gate.as_ref();
            let guard = released.lock().unwrap();
            let (_guard, wait) = changed
                .wait_timeout_while(guard, Duration::from_secs(5), |flag| !*flag)
                .unwrap();
            assert!(!wait.timed_out(), "reader holders were not released");
        }));
    let app = api::router(fixture.state.clone());
    let uri = format!("/api/v1/libraries/{}/facets", fixture.library);
    let mut holders = Vec::new();
    for _ in 0..8 {
        let request = Request::get(&uri)
            .header("Authorization", "Bearer management-secret")
            .body(Body::empty())
            .unwrap();
        holders.push(tokio::spawn(app.clone().oneshot(request)));
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
    }
    let started = std::time::Instant::now();
    let mut request = Request::get(&uri)
        .header("Authorization", "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(QueryTiming {
        started,
        deadline: started + Duration::from_millis(100),
    });
    let response = tokio::time::timeout(Duration::from_secs(1), app.oneshot(request))
        .await
        .unwrap()
        .unwrap();
    let status = response.status();
    drop(release);
    for holder in holders {
        assert_eq!(holder.await.unwrap().unwrap().status(), StatusCode::OK);
    }
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        entered.load(Ordering::SeqCst),
        8,
        "the expired reader waiter must never start SQL work"
    );
}
