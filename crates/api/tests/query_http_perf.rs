//! Release HTTP read-model measurement over real self and Jellyfin routes.
//! cargo test --release -p api --test query_http_perf -- --ignored --nocapture
use api::{ApiState, Store, router};
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use domain::{LibraryId, MediaKind};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use std::sync::Arc;
use std::time::Instant;
use tower::ServiceExt;

#[path = "../../store/tests/query_work/support.rs"]
mod support;

const TOKEN: &str = "management-secret";
struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}
fn fixed_library(store: &Store) -> LibraryId {
    let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"query-library-movie").to_string();
    let root_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"query-root-movie").to_string();
    let db = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
    db.execute("INSERT INTO libraries(library_id,kind,name,is_default,access_mode,admin_visible) VALUES(?1,'movie','Work',0,'everyone',1)",[&id]).unwrap();
    db.execute("INSERT INTO library_roots(root_id,library_id,path_key,display_path) VALUES(?1,?2,'/query-work/movie','/query-work/movie')",rusqlite::params![root_id,id]).unwrap();
    id.parse().unwrap()
}
fn fixture(size: usize) -> (tempfile::TempDir, ApiState, axum::Router, LibraryId) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let lib = fixed_library(&store);
    let start = Instant::now();
    support::seed_into(&store, MediaKind::Movie, 0, size, lib, "/query-work/movie");
    let db = store.data_dir().join("library.db");
    println!(
        "http_fixture size={size} repair_ms={:.3} db_bytes={} wal_bytes={:?}",
        start.elapsed().as_secs_f64() * 1000.,
        std::fs::metadata(&db).unwrap().len(),
        std::fs::metadata(format!("{}-wal", db.display()))
            .map(|v| v.len())
            .ok()
    );
    let state = ApiState::new(
        store,
        TOKEN.into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap();
    let app = router(state.clone());
    (dir, state, app, lib)
}
struct Sample {
    status: u16,
    bytes: usize,
    elapsed_ms: f64,
    phases_ms: [f64; 7],
}

async fn call(app: &axum::Router, state: Option<&ApiState>, uri: &str) -> Sample {
    let req = Request::builder()
        .uri(uri)
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let before = state.map(ApiState::query_phase_snapshot);
    let start = Instant::now();
    let response = app.clone().oneshot(req).await.unwrap();
    let status = response.status().as_u16();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.;
    let phases_ms = if let (Some(before), Some(state)) = (before, state) {
        let after = state.query_phase_snapshot();
        [
            after.lock_wait_ns.saturating_sub(before.lock_wait_ns),
            after.selection_ns.saturating_sub(before.selection_ns),
            after.store_select_ns.saturating_sub(before.store_select_ns),
            after
                .selected_facts_ns
                .saturating_sub(before.selected_facts_ns),
            after.cold_details_ns.saturating_sub(before.cold_details_ns),
            after.dto_ns.saturating_sub(before.dto_ns),
            after
                .serialization_ns
                .saturating_sub(before.serialization_ns),
        ]
        .map(|ns| ns as f64 / 1_000_000.)
    } else {
        [0.; 7]
    };
    Sample {
        status,
        bytes: body.len(),
        elapsed_ms,
        phases_ms,
    }
}
async fn measure(
    app: &axum::Router,
    state: Option<&ApiState>,
    uri: &str,
    label: &str,
    expected_status: u16,
) {
    let first = call(app, state, uri).await;
    assert_eq!(first.status, expected_status, "{label} first");
    for _ in 0..5 {
        assert_eq!(
            call(app, state, uri).await.status,
            expected_status,
            "{label} warmup"
        );
    }
    let mut times = Vec::with_capacity(50);
    let mut phase_times: [Vec<f64>; 7] = std::array::from_fn(|_| Vec::with_capacity(50));
    let bytes = first.bytes;
    for _ in 0..50 {
        let sample = call(app, state, uri).await;
        assert_eq!(sample.status, expected_status, "{label}");
        assert_eq!(sample.bytes, bytes, "{label} body size drift");
        times.push(sample.elapsed_ms);
        for (bucket, elapsed) in phase_times.iter_mut().zip(sample.phases_ms) {
            bucket.push(elapsed);
        }
    }
    times.sort_by(f64::total_cmp);
    println!(
        "{label}\tstatus={expected_status}\tbytes={bytes}\tfirst_ms={:.3}\tp50_ms={:.3}\tp95_ms={:.3}\tmax_ms={:.3}",
        first.elapsed_ms, times[24], times[47], times[49]
    );
    if state.is_some() {
        for (index, (name, mut values)) in [
            "lock_wait",
            "selection",
            "store_select",
            "selected_facts",
            "cold_details",
            "dto",
            "serialization",
        ]
        .into_iter()
        .zip(phase_times)
        .enumerate()
        {
            values.sort_by(f64::total_cmp);
            println!(
                "{label}.{name}\tfirst_ms={:.3}\tp50_ms={:.3}\tp95_ms={:.3}\tmax_ms={:.3}",
                first.phases_ms[index], values[24], values[47], values[49]
            );
        }
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "manual release measurement"]
async fn http_matrix() {
    for size in std::env::var("PERF_SIZES")
        .unwrap_or_else(|_| "100000".into())
        .split(',')
        .map(|v| v.parse::<usize>().unwrap())
    {
        let (_dir, state, app, lib) = fixture(size);
        measure(
            &app,
            None,
            &format!("/api/v1/libraries/{lib}/items?limit=100"),
            &format!("self_items_{size}"),
            200,
        )
        .await;
        measure(
            &app,
            None,
            &format!("/api/v1/libraries/{lib}/facets"),
            &format!("self_facets_{size}"),
            200,
        )
        .await;
        let before = state
            .jellyfin_hydrated_rows()
            .load(std::sync::atomic::Ordering::SeqCst);
        measure(
            &app,
            Some(&state),
            &format!("/Items?ParentId={lib}&Recursive=true&IncludeItemTypes=Movie&Limit=100"),
            &format!("jellyfin_items_{size}"),
            200,
        )
        .await;
        let after = state
            .jellyfin_hydrated_rows()
            .load(std::sync::atomic::Ordering::SeqCst);
        println!(
            "jellyfin_hydrated_rows_per_request={}",
            (after - before) / 56
        );
    }
}
#[tokio::test]
async fn http_fixture_serves_self_and_protocol_pages() {
    let (_dir, _state, app, lib) = fixture(24);
    for uri in [
        format!("/api/v1/libraries/{lib}/items?limit=100"),
        format!("/api/v1/libraries/{lib}/facets"),
        format!("/Items?ParentId={lib}&Recursive=true&IncludeItemTypes=Movie&Limit=100"),
    ] {
        assert_eq!(call(&app, None, &uri).await.status, 200, "{uri}");
    }
}

#[tokio::test]
async fn real_jellyfin_page_records_each_phase_boundary() {
    let (_dir, state, app, lib) = fixture(24);
    let before = state.query_phase_snapshot();
    let response = call(
        &app,
        Some(&state),
        &format!("/Items?ParentId={lib}&Recursive=true&IncludeItemTypes=Movie&Limit=100"),
    )
    .await;
    assert_eq!(response.status, 200);
    let after = state.query_phase_snapshot();
    assert!(after.lock_wait_ns > before.lock_wait_ns);
    assert!(after.selection_ns > before.selection_ns);
    assert!(after.store_select_ns > before.store_select_ns);
    assert!(after.selected_facts_ns > before.selected_facts_ns);
    assert!(after.cold_details_ns > before.cold_details_ns);
    assert!(after.dto_ns > before.dto_ns);
    assert!(after.serialization_ns > before.serialization_ns);
    assert_eq!(
        state
            .jellyfin_hydrated_rows()
            .load(std::sync::atomic::Ordering::SeqCst),
        24 * 3
    );
}
