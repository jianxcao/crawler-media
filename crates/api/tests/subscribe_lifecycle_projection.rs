#![allow(dead_code)]
#[path = "writer_isolation/support.rs"]
mod support;
use axum::http::StatusCode;
use serde_json::Value;
use support::*;
#[test]
fn standalone_recreation_allows_predelivery_old_policy_then_fences_old_publish_delete() {
    let fixture = Fixture::idle();
    let store = fixture.state.store();
    let original = store.get_subscribe(fixture.subscribe).unwrap().unwrap();
    let old = store
        .subscribe_job_sync(fixture.subscribe)
        .unwrap()
        .expect("unchecked Store insert emits activation");
    assert_eq!(
        runtime()
            .block_on(fixture.request("POST", "/api/v1/jobs/tick?now=1", Value::Null))
            .0,
        StatusCode::OK
    );
    let identity = jobs::JobIdentity::subscribe_search(fixture.subscribe);
    fixture
        .state
        .jobs()
        .acquire()
        .unwrap()
        .set_def_enabled(identity, false)
        .unwrap();
    let collection = store
        .create_collection(fixture.user, "keep during recreation")
        .unwrap();
    store
        .add_collection_item(&collection, &fixture.media.to_string())
        .unwrap();
    let peer = api::Store::open(fixture.temp.path().join("data")).unwrap();
    peer.delete_subscribe(fixture.subscribe).unwrap();
    let mut next = original.clone();
    next.search_interval_secs = 777;
    peer.insert_subscribe(&next).unwrap();
    let new = peer.subscribe_job_sync(next.id).unwrap().unwrap();
    assert!(new.epoch > old.epoch);
    assert_ne!(new.generation, old.generation);
    assert!(
        !fixture
            .state
            .jobs()
            .acquire()
            .unwrap()
            .def_for_identity(identity)
            .unwrap()
            .unwrap()
            .enabled,
        "optionB explicitly allows old policy before activation delivery"
    );
    assert!(
        !peer
            .acknowledge_subscribe_job_sync(next.id, &old.generation, old.revision)
            .unwrap()
    );
    assert!(peer.subscribe_job_sync(next.id).unwrap().is_some());
    assert_eq!(
        runtime()
            .block_on(fixture.request("POST", "/api/v1/jobs/tick?now=2", Value::Null))
            .0,
        StatusCode::OK
    );
    let jobs = fixture.state.jobs();
    let queue = jobs.acquire().unwrap();
    let definition = queue.def_for_identity(identity).unwrap().unwrap();
    assert!(definition.enabled);
    assert_eq!(
        definition.schedule,
        Some(jobs::Schedule::Interval { secs: 777 })
    );
    assert!(
        !queue
            .apply_subscribe_config(
                next.id,
                &old.generation,
                old.epoch,
                old.revision + 100,
                |_| panic!("stale publication reached effect")
            )
            .unwrap()
    );
    assert!(
        !queue
            .close_subscribe_epoch(next.id, &old.generation, old.epoch)
            .unwrap()
    );
    assert_eq!(
        queue.def_for_identity(identity).unwrap().unwrap().id,
        definition.id
    );
}
#[test]
fn aborted_standalone_creation_closes_jobs_epoch_without_seeding_jobs_or_replaying_hooks() {
    let fixture = Fixture::idle();
    let store = fixture.state.store();
    let mut candidate = store.get_subscribe(fixture.subscribe).unwrap().unwrap();
    candidate.id = domain::SubscribeId::new();
    let op = store.begin_subscribe_creation(&candidate).unwrap();
    store.retain_subscribe_creation(&op).unwrap();
    store.abort_subscribe_creation(&op).unwrap();
    let generation = store.subscribe_jobs_cleanup(candidate.id).unwrap().unwrap();
    let epoch = store
        .subscribe_lifecycle_epoch(candidate.id, &generation)
        .unwrap();
    assert_eq!(
        runtime()
            .block_on(fixture.request("POST", "/api/v1/jobs/tick?now=1", Value::Null))
            .0,
        StatusCode::OK
    );
    let jobs = fixture.state.jobs();
    let queue = jobs.acquire().unwrap();
    assert!(
        queue
            .def_for_identity(jobs::JobIdentity::subscribe_search(candidate.id))
            .unwrap()
            .is_none()
    );
    assert!(
        !queue
            .apply_subscribe_config(candidate.id, &generation, epoch, 1, |_| panic!(
                "aborted epoch replayed effect"
            ))
            .unwrap()
    );
    assert!(store.get_subscribe(candidate.id).unwrap().is_none());
}
