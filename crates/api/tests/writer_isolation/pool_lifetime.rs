use super::support::*;
use axum::http::StatusCode;
use serde_json::Value;
use std::num::NonZeroUsize;

#[test]
fn resizing_the_shared_reader_pool_keeps_library_requests_available() {
    let mut fixture = Fixture::idle();
    fixture.state = fixture
        .state
        .with_library_read_pool_size(NonZeroUsize::new(1).unwrap())
        .unwrap();
    let library = fixture
        .state
        .store()
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .unwrap();
    runtime().block_on(async {
        let (status, body) = fixture
            .request(
                "GET",
                &format!("/api/v1/libraries/{}/items", library.id),
                Value::Null,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    });
}

#[test]
#[cfg(debug_assertions)]
fn jobs_writer_rejects_reverse_database_acquisition() {
    let fixture = Fixture::idle();
    let jobs = fixture.state.jobs();
    let _jobs = jobs.lock();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        fixture
            .state
            .store()
            .with_writer(store::Db::Cache, |_| Ok(()))
    }));
    assert!(
        result.is_err(),
        "jobs → cache violates the database writer order"
    );
}
