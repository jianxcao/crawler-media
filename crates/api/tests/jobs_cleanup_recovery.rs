#![allow(dead_code)]
#[path = "writer_isolation/support.rs"]
mod support;
use axum::http::StatusCode;
use serde_json::Value;
use support::*;

#[test]
fn cleanup_intent_is_atomic_and_old_generation_cannot_delete_recreated_parent_jobs() {
    let fixture = Fixture::idle();
    let store = fixture.state.store();
    let template = store.get_subscribe(fixture.subscribe).unwrap().unwrap();
    let db = rusqlite::Connection::open(fixture.temp.path().join("data/subscribe.db")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_delete BEFORE DELETE ON subscribes BEGIN SELECT RAISE(FAIL,'fixture delete failure'); END;").unwrap();
    assert!(store.delete_subscribe(fixture.subscribe).is_err());
    assert!(
        store
            .subscribe_jobs_cleanup(fixture.subscribe)
            .unwrap()
            .is_none()
    );
    db.execute_batch("DROP TRIGGER reject_delete").unwrap();
    let keep = store
        .create_collection(fixture.user, "same-ID recreation fixture")
        .unwrap();
    store
        .add_collection_item(&keep, &fixture.media.to_string())
        .unwrap();
    store.delete_subscribe(fixture.subscribe).unwrap();
    let old = store
        .subscribe_jobs_cleanup(fixture.subscribe)
        .unwrap()
        .unwrap();
    store
        .acknowledge_subscribe_jobs_cleanup(fixture.subscribe, "wrong-generation")
        .unwrap();
    assert_eq!(
        store.subscribe_jobs_cleanup(fixture.subscribe).unwrap(),
        Some(old.clone())
    );
    store.insert_subscribe(&template).unwrap();
    // The newer epoch must be delivered before manual Jobs edits are protected.
    assert_eq!(
        runtime()
            .block_on(fixture.request("POST", "/api/v1/jobs/tick?now=1", Value::Null))
            .0,
        StatusCode::OK
    );
    fixture
        .state
        .jobs()
        .acquire()
        .unwrap()
        .delete_def(jobs::JobIdentity::subscribe_search(fixture.subscribe))
        .unwrap();
    let def = fixture
        .state
        .jobs()
        .acquire()
        .unwrap()
        .create_def(jobs::NewDef {
            kind: jobs::JobKind::SubscribeSearch,
            scope: jobs::JobScope::Subscribe(fixture.subscribe),
            name: "new parent".into(),
            enabled: false,
            schedule: None,
            payload: serde_json::json!({"subscribe_id":fixture.subscribe.to_string()}).to_string(),
            timeout_secs: None,
            concurrency_key: None,
        })
        .unwrap();
    let epoch = store
        .subscribe_lifecycle_epoch(fixture.subscribe, &old)
        .unwrap();
    assert!(
        !fixture
            .state
            .jobs()
            .acquire()
            .unwrap()
            .close_subscribe_epoch(fixture.subscribe, &old, epoch)
            .unwrap()
    );
    assert_eq!(
        runtime()
            .block_on(fixture.request("POST", "/api/v1/jobs/tick?now=1", Value::Null))
            .0,
        StatusCode::OK
    );
    assert!(
        store
            .subscribe_jobs_cleanup(fixture.subscribe)
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .state
            .jobs()
            .acquire()
            .unwrap()
            .def(def.id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn cleanup_recovery_window_does_not_starve_after_sixty_four_poisoned_acknowledgments() {
    let fixture = Fixture::idle();
    let store = fixture.state.store();
    let mut template = store.get_subscribe(fixture.subscribe).unwrap().unwrap();
    let mut ids = Vec::new();
    for index in 1..=65 {
        template.id = format!("00000000-0000-4000-8000-{index:012}")
            .parse()
            .unwrap();
        store.insert_subscribe(&template).unwrap();
        store.delete_subscribe(template.id).unwrap();
        ids.push(template.id);
    }
    let last = ids[64];
    let db = rusqlite::Connection::open(fixture.temp.path().join("data/subscribe.db")).unwrap();
    db.execute_batch(&format!("CREATE TRIGGER poison_cleanup BEFORE DELETE ON subscribe_jobs_cleanup WHEN OLD.id<>'{last}' BEGIN SELECT RAISE(FAIL,'fixture poisoned cleanup'); END;")).unwrap();
    for now in [1, 2] {
        assert_eq!(
            runtime()
                .block_on(fixture.request(
                    "POST",
                    &format!("/api/v1/jobs/tick?now={now}"),
                    Value::Null
                ))
                .0,
            StatusCode::OK
        );
    }
    assert!(store.subscribe_jobs_cleanup(ids[0]).unwrap().is_some());
    assert!(store.subscribe_jobs_cleanup(last).unwrap().is_none());
}

#[test]
fn postcommit_cleanup_reader_busy_reports_success_then_recovers() {
    use axum::{body::Body, http::Request};
    use std::{
        sync::{Arc, mpsc},
        time::{Duration, Instant},
    };
    use tower::ServiceExt;
    let stage = tempfile::tempdir_in("/tmp").unwrap();
    let downloader = Arc::new(downloader::MemoryDownloader::new(
        stage.path().to_path_buf(),
    ));
    let fixture = Fixture::new(downloader.clone());
    fixture.pending();
    let store = fixture.state.store();
    let jobs = fixture.state.jobs();
    jobs.acquire()
        .unwrap()
        .delete_def(jobs::JobIdentity::subscribe_search(fixture.subscribe))
        .unwrap();
    let def = jobs
        .acquire()
        .unwrap()
        .create_def(jobs::NewDef {
            kind: jobs::JobKind::SubscribeSearch,
            scope: jobs::JobScope::Subscribe(fixture.subscribe),
            name: "postcommit cleanup".into(),
            enabled: false,
            schedule: None,
            payload: serde_json::json!({"subscribe_id":fixture.subscribe.to_string()}).to_string(),
            timeout_secs: None,
            concurrency_key: None,
        })
        .unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let blocked_store = store.clone();
    let holder = std::thread::spawn(move || {
        blocked_store
            .with_writer(store::Db::App, |_| {
                ready_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(15)).unwrap();
                Ok(())
            })
            .unwrap()
    });
    ready_rx.recv().unwrap();
    let state = fixture.state.clone();
    let id = fixture.subscribe;
    let deletion = std::thread::spawn(move || {
        runtime()
            .block_on(
                api::router(state).oneshot(
                    Request::delete(format!("/api/v1/subscriptions/{id}?delete_torrents=true"))
                        .header("authorization", format!("Bearer {TOKEN}"))
                        .body(Body::empty())
                        .unwrap(),
                ),
            )
            .unwrap()
    });
    let db = rusqlite::Connection::open(fixture.temp.path().join("data/subscribe.db")).unwrap();
    let started = Instant::now();
    loop {
        let count: i64 = db
            .query_row(
                "SELECT count(*) FROM subscribes WHERE id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        if count == 0 {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "deletion must commit before App cleanup"
        );
        std::thread::yield_now();
    }
    let pool = store.reader_pool(store::Db::Subscribe);
    let first = pool.try_checkout().unwrap();
    let second = pool.try_checkout().unwrap();
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    let response = deletion.join().unwrap();
    drop((first, second));
    assert!(!downloader.removed().is_empty());
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "committed deletion must survive postcommit reader admission failure"
    );
    assert!(store.subscribe_jobs_cleanup(id).unwrap().is_some());
    assert_eq!(
        runtime()
            .block_on(fixture.request("POST", "/api/v1/jobs/tick?now=1", Value::Null))
            .0,
        StatusCode::OK
    );
    assert!(store.subscribe_jobs_cleanup(id).unwrap().is_none());
    assert!(jobs.acquire().unwrap().def(def.id).unwrap().is_none());
}
