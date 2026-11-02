#![allow(dead_code)]
#[path = "writer_isolation/support.rs"]
mod support;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};
use support::*;
use tower::ServiceExt;

#[test]
fn jobs_writer_and_reservation_contention_reject_subscribe_mutations_before_store_effects() {
    for reservation_only in [false, true] {
        let fixture = Fixture::idle();
        let before = fixture
            .state
            .store()
            .get_subscribe(fixture.subscribe)
            .unwrap();
        let media_count = fixture.state.store().list_all_subscribes().unwrap().len();
        let jobs = fixture.state.jobs();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let holder = std::thread::spawn(move || {
            if reservation_only {
                let _held = jobs.reserve().unwrap();
                ready_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            } else {
                let _held = jobs.acquire().unwrap();
                ready_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            }
        });
        ready_rx.recv().unwrap();
        runtime().block_on(async {
            for (method, path, body) in [
                ("PATCH", format!("/api/v1/subscriptions/{}",fixture.subscribe), json!({"tracking_state":"paused"})),
                ("POST", "/api/v1/subscriptions".into(), json!({"media":{"kind":"movie","title":"Busy new Media"},"coverage":{"kind":"movie"}})),
            ] {
                let started = Instant::now();
                let response = api::router(fixture.state.clone()).oneshot(Request::builder()
                    .method(method).uri(path).header("authorization", format!("Bearer {TOKEN}"))
                    .header("content-type","application/json").body(Body::from(body.to_string())).unwrap()).await.unwrap();
                assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
                assert_eq!(response.headers().get("retry-after").unwrap(), "1");
                assert!(started.elapsed() < Duration::from_millis(1800));
            }
            assert_eq!(tokio::time::timeout(SLO, fixture.request("GET","/api/v1/jobs",Value::Null)).await.unwrap().0,StatusCode::OK);
        });
        fixture
            .state
            .store()
            .put_setting("jobs.contention.independent", "ok")
            .unwrap();
        assert_eq!(
            fixture
                .state
                .store()
                .get_subscribe(fixture.subscribe)
                .unwrap(),
            before
        );
        assert_eq!(
            fixture.state.store().list_all_subscribes().unwrap().len(),
            media_count
        );
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        assert_eq!(
            runtime()
                .block_on(fixture.request(
                    "PATCH",
                    &format!("/api/v1/subscriptions/{}", fixture.subscribe),
                    json!({"tracking_state":"paused"})
                ))
                .0,
            StatusCode::OK
        );
    }
}

#[test]
fn reservation_reentry_and_database_writer_entry_are_structured_immediate_errors() {
    let fixture = Fixture::idle();
    let jobs = fixture.state.jobs();
    let reservation = jobs.reserve().unwrap();
    let started = Instant::now();
    assert!(matches!(
        jobs.reserve(),
        Err(store::StoreError::LockOrder { .. })
    ));
    assert!(started.elapsed() < Duration::from_millis(100));
    fixture
        .state
        .store()
        .with_writer(store::Db::App, |_| {
            assert!(matches!(
                reservation.acquire(),
                Err(store::StoreError::LockOrder { .. })
            ));
            Ok(())
        })
        .unwrap();
    drop(reservation);
    fixture
        .state
        .store()
        .with_writer(store::Db::App, |_| {
            assert!(matches!(
                jobs.acquire(),
                Err(store::StoreError::LockOrder { .. })
            ));
            Ok(())
        })
        .unwrap();
    jobs.acquire().unwrap();
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _reservation = jobs.reserve().unwrap();
        panic!("fixture reservation unwind");
    }));
    assert!(unwind.is_err());
    jobs.acquire().unwrap();
}

#[test]
fn failed_jobs_sql_and_failed_subscribe_compensation_recover_from_durable_dirty_revision() {
    let fixture = Fixture::idle();
    let store = fixture.state.store();
    let initial = store.get_subscribe(fixture.subscribe).unwrap().unwrap();
    let mut dirty = initial.clone();
    dirty.search_interval_secs = 1900;
    store.update_subscribe(&dirty).unwrap();
    store.update_subscribe(&initial).unwrap();
    let intent = store.subscribe_job_sync(initial.id).unwrap().unwrap();
    store
        .acknowledge_subscribe_job_sync(initial.id, &intent.generation, intent.revision)
        .unwrap();
    let jobs_db = rusqlite::Connection::open(fixture.temp.path().join("data/jobs.db")).unwrap();
    jobs_db.execute_batch("CREATE TRIGGER reject_schedule BEFORE UPDATE OF schedule ON job_defs WHEN NEW.kind='subscribe_search' BEGIN SELECT RAISE(FAIL,'fixture Jobs SQL failure'); END;").unwrap();
    let subscribe_db =
        rusqlite::Connection::open(fixture.temp.path().join("data/subscribe.db")).unwrap();
    subscribe_db.execute_batch("CREATE TRIGGER reject_compensation BEFORE UPDATE ON subscribes WHEN OLD.tracking_state='paused' AND NEW.tracking_state='active' BEGIN SELECT RAISE(FAIL,'fixture compensation failure'); END;").unwrap();
    let (status, body) = runtime().block_on(fixture.request(
        "PATCH",
        &format!("/api/v1/subscriptions/{}", initial.id),
        json!({"tracking_state":"paused","search_interval_secs":900}),
    ));
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(body.to_string().contains("compensation failed"));
    let payload = serde_json::json!({"subscribe_id":initial.id}).to_string();
    let def = fixture
        .state
        .jobs()
        .acquire()
        .unwrap()
        .def_for_identity(subscribe_identity(&payload))
        .unwrap()
        .unwrap();
    assert!(
        def.enabled,
        "atomic Queue sync must roll back the earlier enabled update"
    );
    assert_eq!(def.schedule, Some(jobs::Schedule::Interval { secs: 1800 }));
    let current = store.get_subscribe(initial.id).unwrap().unwrap();
    assert_eq!(current.tracking_state, "paused");
    let intent = store.subscribe_job_sync(initial.id).unwrap().unwrap();
    assert!(
        !store
            .acknowledge_subscribe_job_sync(initial.id, &intent.generation, intent.revision - 1)
            .unwrap()
    );
    jobs_db
        .execute_batch("DROP TRIGGER reject_schedule")
        .unwrap();
    subscribe_db
        .execute_batch("DROP TRIGGER reject_compensation")
        .unwrap();
    let reopened = api::ApiState::new(
        api::Store::open(fixture.temp.path().join("data")).unwrap(),
        TOKEN.into(),
        indexer::ProfileSet::load(None).unwrap(),
        std::sync::Arc::new(Offline),
        std::sync::Arc::new(downloader::MemoryDownloader::new(
            fixture.temp.path().join("stage"),
        )),
        fixture.temp.path().join("library"),
    )
    .unwrap()
    .with_catalog(std::sync::Arc::new(Offline));
    let response = runtime()
        .block_on(
            api::router(reopened.clone()).oneshot(
                Request::post("/api/v1/jobs/tick?now=1")
                    .header("authorization", format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        reopened
            .store()
            .subscribe_job_sync(initial.id)
            .unwrap()
            .is_none()
    );
    let queue = reopened.jobs();
    let queue = queue.acquire().unwrap();
    let def = queue
        .def_for_identity(subscribe_identity(&payload))
        .unwrap()
        .unwrap();
    assert!(!def.enabled);
    assert_eq!(def.schedule, Some(jobs::Schedule::Interval { secs: 900 }));
}

#[test]
fn healthy_jobs_manual_config_survives_unrelated_subscribe_updates_and_tick() {
    let fixture = Fixture::idle();
    let mut subscribe = fixture
        .state
        .store()
        .get_subscribe(fixture.subscribe)
        .unwrap()
        .unwrap();
    let payload = json!({"subscribe_id":subscribe.id}).to_string();
    {
        let jobs = fixture.state.jobs();
        let queue = jobs.acquire().unwrap();
        queue
            .set_def_schedule(
                subscribe_identity(&payload),
                jobs::Schedule::Interval { secs: 700 },
            )
            .unwrap();
        queue
            .set_def_enabled(subscribe_identity(&payload), false)
            .unwrap();
    }
    subscribe.follow_future = true;
    subscribe.tracking_state = "completed".into();
    fixture.state.store().update_subscribe(&subscribe).unwrap();
    assert!(
        fixture
            .state
            .store()
            .subscribe_job_sync(subscribe.id)
            .unwrap()
            .is_none()
    );
    let response =
        runtime().block_on(fixture.request("POST", "/api/v1/jobs/tick?now=1", Value::Null));
    assert_eq!(response.0, StatusCode::OK);
    let def = fixture
        .state
        .jobs()
        .acquire()
        .unwrap()
        .def_for_identity(subscribe_identity(&payload))
        .unwrap()
        .unwrap();
    assert!(!def.enabled);
    assert_eq!(def.schedule, Some(jobs::Schedule::Interval { secs: 700 }));
}

#[test]
fn committed_sync_receipt_preserves_later_manual_job_edit_after_ack_failure_and_reopen() {
    let fixture = Fixture::idle();
    let subscribe_db =
        rusqlite::Connection::open(fixture.temp.path().join("data/subscribe.db")).unwrap();
    subscribe_db.execute_batch("CREATE TRIGGER reject_sync_ack BEFORE UPDATE OF jobs_sync_pending ON subscribes WHEN NEW.jobs_sync_pending=0 BEGIN SELECT RAISE(FAIL,'fixture ack failure'); END;").unwrap();
    let path = format!("/api/v1/subscriptions/{}", fixture.subscribe);
    assert_eq!(
        runtime()
            .block_on(fixture.request(
                "PATCH",
                &path,
                json!({"tracking_state":"paused","search_interval_secs":900})
            ))
            .0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    let store = fixture.state.store();
    let intent = store
        .subscribe_job_sync(fixture.subscribe)
        .unwrap()
        .unwrap();
    let payload = json!({"subscribe_id":fixture.subscribe}).to_string();
    let def = fixture
        .state
        .jobs()
        .acquire()
        .unwrap()
        .def_for_identity(subscribe_identity(&payload))
        .unwrap()
        .unwrap();
    assert!(!def.enabled);
    assert_eq!(
        runtime()
            .block_on(fixture.request(
                "PATCH",
                &format!("/api/v1/jobs/{}", def.id),
                json!({"enabled":true})
            ))
            .0,
        StatusCode::OK
    );
    assert_eq!(
        runtime()
            .block_on(fixture.request(
                "PUT",
                &format!("/api/v1/jobs/{}/schedule", def.id),
                json!({"interval_secs":700})
            ))
            .0,
        StatusCode::OK
    );
    subscribe_db
        .execute_batch("DROP TRIGGER reject_sync_ack")
        .unwrap();
    let reopened = api::ApiState::new(
        api::Store::open(fixture.temp.path().join("data")).unwrap(),
        TOKEN.into(),
        indexer::ProfileSet::load(None).unwrap(),
        std::sync::Arc::new(Offline),
        std::sync::Arc::new(downloader::MemoryDownloader::new(
            fixture.temp.path().join("stage"),
        )),
        fixture.temp.path().join("library"),
    )
    .unwrap()
    .with_catalog(std::sync::Arc::new(Offline));
    let response = runtime()
        .block_on(
            api::router(reopened.clone()).oneshot(
                Request::post("/api/v1/jobs/tick?now=1")
                    .header("authorization", format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        reopened
            .store()
            .subscribe_job_sync(fixture.subscribe)
            .unwrap()
            .is_none()
    );
    let current = reopened
        .jobs()
        .acquire()
        .unwrap()
        .def_for_identity(subscribe_identity(&payload))
        .unwrap()
        .unwrap();
    assert!(
        current.enabled,
        "receipt must acknowledge, not replay over manual enabled change"
    );
    assert_eq!(
        current.schedule,
        Some(jobs::Schedule::Interval { secs: 700 })
    );
    let mut latest = store.get_subscribe(fixture.subscribe).unwrap().unwrap();
    latest.search_interval_secs = 1000;
    store.update_subscribe(&latest).unwrap();
    assert!(
        !store
            .acknowledge_subscribe_job_sync(latest.id, &intent.generation, intent.revision)
            .unwrap()
    );
    assert!(store.subscribe_job_sync(latest.id).unwrap().is_some());
}

#[test]
fn failed_create_compensation_keeps_a_recoverable_subscribe_and_atomic_jobs_seed() {
    let fixture = Fixture::idle();
    let jobs_db = rusqlite::Connection::open(fixture.temp.path().join("data/jobs.db")).unwrap();
    jobs_db.execute_batch("CREATE TRIGGER reject_catalog BEFORE INSERT ON job_defs WHEN NEW.kind='catalog_refresh' BEGIN SELECT RAISE(FAIL,'fixture catalog failure'); END;").unwrap();
    let subscribe_db =
        rusqlite::Connection::open(fixture.temp.path().join("data/subscribe.db")).unwrap();
    subscribe_db.execute_batch("CREATE TRIGGER reject_subscribe_delete BEFORE DELETE ON subscribes BEGIN SELECT RAISE(FAIL,'fixture delete failure'); END;").unwrap();
    let before = fixture
        .state
        .jobs()
        .acquire()
        .unwrap()
        .list_defs()
        .unwrap()
        .len();
    let response = runtime().block_on(fixture.request("POST","/api/v1/subscriptions",json!({"media":{"kind":"movie","title":"Recover failed create"},"coverage":{"kind":"movie"}})));
    assert_eq!(response.0, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(response.1.to_string().contains("compensation incomplete"));
    assert_eq!(
        fixture
            .state
            .jobs()
            .acquire()
            .unwrap()
            .list_defs()
            .unwrap()
            .len(),
        before,
        "Search seed must roll back with failed Catalog seed"
    );
    let pending = fixture
        .state
        .store()
        .pending_subscribe_job_sync(None, 64)
        .unwrap();
    assert_eq!(pending.len(), 1);
    let intent = fixture
        .state
        .store()
        .subscribe_job_sync(pending[0])
        .unwrap()
        .unwrap();
    assert!(
        fixture
            .state
            .store()
            .get_media(intent.subscribe.media_id)
            .unwrap()
            .is_some()
    );
    jobs_db
        .execute_batch("DROP TRIGGER reject_catalog")
        .unwrap();
    subscribe_db
        .execute_batch("DROP TRIGGER reject_subscribe_delete")
        .unwrap();
    assert_eq!(
        runtime()
            .block_on(fixture.request("POST", "/api/v1/jobs/tick?now=1", Value::Null))
            .0,
        StatusCode::OK
    );
    assert!(
        fixture
            .state
            .store()
            .subscribe_job_sync(pending[0])
            .unwrap()
            .is_none()
    );
    let queue = fixture.state.jobs();
    let queue = queue.acquire().unwrap();
    assert_eq!(
        queue
            .def_for_identity(jobs::JobIdentity::subscribe_search(pending[0]))
            .unwrap()
            .is_some() as usize,
        1
    );
    assert_eq!(
        queue
            .def_for_identity(jobs::JobIdentity::catalog_refresh(
                intent.subscribe.media_id
            ))
            .unwrap()
            .is_some() as usize,
        1
    );
}

#[test]
fn jobs_mutation_and_maintenance_endpoints_return_retryable_busy_without_stalling_reads() {
    let fixture = Fixture::idle();
    let defs = fixture.state.jobs().acquire().unwrap().list_defs().unwrap();
    let ordinary = defs
        .iter()
        .find(|def| def.kind == jobs::JobKind::SubscribeSearch)
        .unwrap();
    let maintenance = defs.iter().find(|def| def.kind.is_maintenance()).unwrap();
    let requests = vec![
        (
            "POST",
            format!("/api/v1/jobs/{}/run", ordinary.id),
            Value::Null,
        ),
        (
            "PATCH",
            format!("/api/v1/jobs/{}", ordinary.id),
            json!({"enabled":true}),
        ),
        (
            "PUT",
            format!("/api/v1/jobs/{}/schedule", ordinary.id),
            json!({"interval_secs":900}),
        ),
        (
            "POST",
            format!("/api/v1/jobs/{}/cancel", ordinary.id),
            Value::Null,
        ),
        (
            "PUT",
            format!("/api/v1/jobs/{}/maintenance-config", maintenance.id),
            json!({"expected_payload":serde_json::from_str::<Value>(&maintenance.payload).unwrap(),"payload":serde_json::from_str::<Value>(&maintenance.payload).unwrap()}),
        ),
    ];
    let jobs = fixture.state.jobs();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = jobs.acquire().unwrap();
        ready_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    ready_rx.recv().unwrap();
    runtime().block_on(async {
        for (method, path, body) in &requests {
            let started = Instant::now();
            let response = api::router(fixture.state.clone())
                .oneshot(
                    Request::builder()
                        .method(*method)
                        .uri(path)
                        .header("authorization", format!("Bearer {TOKEN}"))
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
            assert_eq!(response.headers().get("retry-after").unwrap(), "1");
            assert!(started.elapsed() < Duration::from_millis(1800));
        }
        assert_eq!(
            tokio::time::timeout(SLO, fixture.request("GET", "/api/v1/health", Value::Null))
                .await
                .unwrap()
                .0,
            StatusCode::OK
        );
        assert_eq!(
            tokio::time::timeout(SLO, fixture.request("GET", "/api/v1/jobs", Value::Null))
                .await
                .unwrap()
                .0,
            StatusCode::OK
        );
    });
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    runtime().block_on(async {
        for (method, path, body) in requests {
            assert_eq!(
                fixture.request(method, &path, body).await.0,
                StatusCode::OK,
                "{path}"
            );
        }
    });
}

#[test]
fn poisoned_first_recovery_window_does_not_starve_a_later_dirty_subscribe() {
    let fixture = Fixture::idle();
    let store = fixture.state.store();
    let mut template = store.get_subscribe(fixture.subscribe).unwrap().unwrap();
    let mut ids = Vec::new();
    for index in 1..=65 {
        template.id = format!("00000000-0000-4000-8000-{index:012}")
            .parse()
            .unwrap();
        store.insert_subscribe(&template).unwrap();
        template.search_interval_secs = 901;
        store.update_subscribe(&template).unwrap();
        template.search_interval_secs = 1800;
        ids.push(template.id);
    }
    let last = ids[64];
    let jobs_db = rusqlite::Connection::open(fixture.temp.path().join("data/jobs.db")).unwrap();
    jobs_db.execute_batch(&format!("CREATE TRIGGER poison_first_window BEFORE UPDATE OF schedule ON job_defs WHEN NEW.kind='subscribe_search' AND json_extract(NEW.payload,'$.subscribe_id')<>'{last}' BEGIN SELECT RAISE(FAIL,'fixture poisoned intent'); END;")).unwrap();
    assert_eq!(
        runtime()
            .block_on(fixture.request("POST", "/api/v1/jobs/tick?now=1", Value::Null))
            .0,
        StatusCode::OK
    );
    assert!(store.subscribe_job_sync(last).unwrap().is_some());
    assert_eq!(
        runtime()
            .block_on(fixture.request("POST", "/api/v1/jobs/tick?now=2", Value::Null))
            .0,
        StatusCode::OK
    );
    assert!(store.subscribe_job_sync(last).unwrap().is_none());
    assert!(store.subscribe_job_sync(ids[0]).unwrap().is_some());
}

#[test]
fn deletion_jobs_busy_reports_external_cleanup_and_recovers_disabled_definition() {
    let stage = tempfile::tempdir_in("/tmp").unwrap();
    let downloader = std::sync::Arc::new(downloader::MemoryDownloader::new(
        stage.path().to_path_buf(),
    ));
    let fixture = Fixture::new(downloader.clone());
    fixture.pending();
    let payload = json!({"subscribe_id":fixture.subscribe.to_string()}).to_string();
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
            name: "disabled deletion fixture".into(),
            enabled: false,
            schedule: None,
            payload: payload.clone(),
            timeout_secs: None,
            concurrency_key: None,
        })
        .unwrap();
    let jobs = fixture.state.jobs();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _held = jobs.acquire().unwrap();
        ready_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    ready_rx.recv().unwrap();
    let path = format!(
        "/api/v1/subscriptions/{}?delete_torrents=true",
        fixture.subscribe
    );
    let response = runtime().block_on(fixture.request("DELETE", &path, Value::Null));
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    assert!(
        !downloader.removed().is_empty(),
        "fixture external torrent cleanup must have happened"
    );
    assert_eq!(
        response.0,
        StatusCode::OK,
        "completed external cleanup and parent deletion must report success"
    );
    assert!(
        fixture
            .state
            .store()
            .get_subscribe(fixture.subscribe)
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .state
            .store()
            .subscribe_jobs_cleanup(fixture.subscribe)
            .unwrap()
            .is_some()
    );
    let reopened = api::ApiState::new(
        api::Store::open(fixture.temp.path().join("data")).unwrap(),
        TOKEN.into(),
        indexer::ProfileSet::load(None).unwrap(),
        std::sync::Arc::new(Offline),
        downloader,
        fixture.temp.path().join("library"),
    )
    .unwrap()
    .with_catalog(std::sync::Arc::new(Offline));
    let response = runtime()
        .block_on(
            api::router(reopened.clone()).oneshot(
                Request::post("/api/v1/jobs/tick?now=1")
                    .header("authorization", format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        reopened
            .store()
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
            .is_none()
    );
}

fn subscribe_identity(payload: &str) -> jobs::JobIdentity {
    let value: serde_json::Value = serde_json::from_str(payload).unwrap();
    jobs::JobIdentity::subscribe_search(value["subscribe_id"].as_str().unwrap().parse().unwrap())
}

#[test]
fn subscribe_activity_includes_defless_search_with_extra_arguments() {
    let fixture = Fixture::idle();
    let payload =
        json!({"exclude_enclosure":"replacement","subscribe_id":fixture.subscribe,"extra":true})
            .to_string();
    let job = fixture
        .state
        .jobs()
        .acquire()
        .unwrap()
        .enqueue(
            jobs::JobIdentity::subscribe_search(fixture.subscribe),
            &payload,
            0,
        )
        .unwrap();
    let (status, body) = runtime().block_on(fixture.request(
        "GET",
        &format!("/api/v1/subscriptions/{}/activities", fixture.subscribe),
        Value::Null,
    ));
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == job.id.to_string()),
        "{body}"
    );
}
