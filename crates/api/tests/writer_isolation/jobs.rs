use super::support::*;
use axum::http::StatusCode;
use serde_json::Value;
use std::time::Duration;

#[test]
fn job_claim_and_job_list_do_not_serialize() {
    let fixture = Fixture::idle();
    let baseline = fixture.state.jobs().lock().list_defs().unwrap();
    assert!(!baseline.is_empty());
    let mut jobs = rusqlite::Connection::open(fixture.temp.path().join("data/jobs.db")).unwrap();
    let blocked = jobs
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let shared = fixture.state.jobs();
    let (entered, wait) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let queue = shared.lock();
        entered.send(()).unwrap();
        queue.enqueue(
            jobs::JobKind::SubscribeSearch,
            r#"{"writer_isolation":true}"#,
            0,
        )
    });
    wait.recv_timeout(Duration::from_secs(1)).unwrap();
    let runtime = runtime();
    let result = runtime.block_on(async {
        tokio::time::timeout(SLO, fixture.request("GET", "/api/v1/jobs", Value::Null)).await
    });
    blocked.commit().unwrap();
    let enqueued = writer.join().unwrap().unwrap();
    let (status, body) =
        result.expect("jobs writer must not delay the committed HTTP list beyond 200ms");
    assert_eq!(status, StatusCode::OK, "{body}");
    let listed = body["data"].as_array().unwrap();
    for def in baseline {
        assert!(
            listed.iter().any(|item| item["id"] == def.id.to_string()),
            "committed def {} absent: {body}",
            def.id
        );
    }
    assert_eq!(
        fixture
            .state
            .jobs()
            .lock()
            .get(enqueued.id)
            .unwrap()
            .unwrap()
            .id,
        enqueued.id
    );
}

#[test]
fn jobs_readers_are_bounded_and_reject_writes_on_both_connections() {
    let fixture = Fixture::idle();
    let readers = fixture.state.jobs_readers();
    let mut first = readers.try_checkout().unwrap();
    let mut second = readers.try_checkout().unwrap();
    assert!(matches!(
        readers.try_checkout(),
        Err(store::LibraryReadError::Busy)
    ));
    for lease in [&mut first, &mut second] {
        let budget = store::QueryBudget {
            deadline: std::time::Instant::now() + Duration::from_secs(1),
            cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        lease
            .with_connection(&budget, |connection| {
                assert!(connection.is_readonly("main")?);
                let query_only: bool =
                    connection.pragma_query_value(None, "query_only", |row| row.get(0))?;
                assert!(query_only);
                assert!(
                    connection
                        .execute("UPDATE job_defs SET name=name", [])
                        .is_err()
                );
                assert!(
                    !jobs::QueueRead::new(connection)
                        .list_defs()
                        .unwrap()
                        .is_empty()
                );
                Ok(())
            })
            .unwrap();
    }
}
