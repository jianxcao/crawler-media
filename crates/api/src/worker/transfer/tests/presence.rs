use super::*;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

#[test]
fn verify_does_not_recover_an_active_transfer_before_its_completion() {
    let (tmp, state, media, subscribe) = fixture();
    let source = tmp.path().join("source.mkv");
    std::fs::write(&source, b"completed Transfer bytes").unwrap();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let (placed_tx, placed_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    std::thread::scope(|threads| {
        let worker_state = &state;
        let worker_plan = &plan;
        let worker_subscribe = &subscribe;
        let worker = threads.spawn(move || {
            let lease = worker_state
                .acquire_library_work(&worker_plan.library_id)
                .unwrap();
            let recorder = recorder(worker_state, worker_subscribe, &lease, worker_plan);
            let candidate = candidate(&media, worker_plan, &source, library::TransferMode::Copy);
            let mut reserved = subscribe::TransferRecorder::prepare(
                &recorder,
                &candidate,
                &source,
                library::TransferMode::Copy,
            )
            .unwrap();
            library::transfer_file(&source, &reserved.file.path, library::TransferMode::Copy)
                .unwrap();
            placed_tx.send(reserved.clone()).unwrap();
            release_rx.recv().unwrap();
            reserved.file.publication = domain::FilePublication::Published;
            subscribe::TransferRecorder::published(&recorder, &reserved)
        });
        let reserved = placed_rx.recv().unwrap();
        let status = verify_through_http(&state, &plan.library_id);
        let after_verify = state
            .store
            .file_coverage_by_id(reserved.file.file_id)
            .unwrap()
            .unwrap();
        // Always release the worker before assertions, including the red case.
        release_tx.send(()).unwrap();
        let completion = worker.join().unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            after_verify.file.publication,
            domain::FilePublication::Pending,
            "Verify must leave an active Transfer's reservation alone"
        );
        assert_eq!(after_verify.file.version, reserved.file.version);
        assert!(completion.is_ok(), "active Transfer failed: {completion:?}");
        let published = state
            .store
            .file_coverage_by_id(reserved.file.file_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            published.file.publication,
            domain::FilePublication::Published
        );
        assert_eq!(
            std::fs::read(&published.file.path).unwrap(),
            b"completed Transfer bytes"
        );
    });
}

#[test]
fn verify_recovers_real_source_after_transfer_recorder_is_dropped() {
    let (tmp, state, media, subscribe) = fixture();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let lease = state.acquire_library_work(&plan.library_id).unwrap();
    let source = tmp.path().join("source.mkv");
    std::fs::write(&source, b"interrupted Transfer bytes").unwrap();
    let recorder = recorder(&state, &subscribe, &lease, &plan);
    let candidate = candidate(&media, &plan, &source, library::TransferMode::Copy);
    let reserved = subscribe::TransferRecorder::prepare(
        &recorder,
        &candidate,
        &source,
        library::TransferMode::Copy,
    )
    .unwrap();
    library::transfer_file(&source, &reserved.file.path, library::TransferMode::Copy).unwrap();
    let observed = library::observe_source(&reserved.file.path).unwrap();
    drop(recorder);
    assert_eq!(
        verify_through_http(&state, &plan.library_id),
        StatusCode::OK
    );
    let recovered = state
        .store
        .file_coverage_by_id(reserved.file.file_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        recovered.file.publication,
        domain::FilePublication::Published
    );
    assert_eq!(
        recovered.file.version.revision,
        reserved.file.version.revision + 1
    );
    assert_eq!(
        recovered.file.version.source_version,
        observed.source_version
    );
    assert_eq!(recovered.file.size_bytes, observed.size_bytes);
    assert_eq!(state.store.scan_probe_obligations(10).unwrap().len(), 1);
}

#[test]
fn verify_recovers_pending_source_after_transfer_completion_is_rejected() {
    let (tmp, state, media, subscribe) = fixture();
    let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
    let lease = state.acquire_library_work(&plan.library_id).unwrap();
    let source = tmp.path().join("source.mkv");
    std::fs::write(&source, b"placed Transfer bytes").unwrap();
    let recorder = recorder(&state, &subscribe, &lease, &plan);
    let candidate = candidate(&media, &plan, &source, library::TransferMode::Copy);
    let mut reserved = subscribe::TransferRecorder::prepare(
        &recorder,
        &candidate,
        &source,
        library::TransferMode::Copy,
    )
    .unwrap();
    library::transfer_file(&source, &reserved.file.path, library::TransferMode::Copy).unwrap();
    // A configuration change rejects normal completion; the recorder stays alive.
    state.store.save_raw_scrape_settings("{}").unwrap();
    reserved.file.publication = domain::FilePublication::Published;
    assert!(subscribe::TransferRecorder::published(&recorder, &reserved).is_err());
    assert_eq!(
        verify_through_http(&state, &plan.library_id),
        StatusCode::OK
    );
    let recovered = state
        .store
        .file_coverage_by_id(reserved.file.file_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        recovered.file.publication,
        domain::FilePublication::Published
    );
    assert_eq!(
        recovered.file.version.source_version,
        library::observe_source(&reserved.file.path)
            .unwrap()
            .source_version
    );
}

fn verify_through_http(state: &ApiState, library: &str) -> StatusCode {
    let app = crate::management::router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/libraries/{library}/verify"))
        .header("authorization", "Bearer test-token")
        .body(Body::empty())
        .unwrap();
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(app.oneshot(request))
        .unwrap()
        .status()
}
