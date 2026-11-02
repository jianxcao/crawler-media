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
fn jobs_writer_rejects_reverse_database_acquisition() {
    let fixture = Fixture::idle();
    let jobs = fixture.state.jobs();
    let _jobs = jobs.acquire().unwrap();
    let result = fixture
        .state
        .store()
        .with_writer(store::Db::Cache, |_| Ok(()));
    assert!(
        matches!(result, Err(store::StoreError::LockOrder { held, requested: store::WriterRank::Cache }) if held == [store::WriterRank::Jobs])
    );
}
