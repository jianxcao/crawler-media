#![cfg(unix)]

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use api::{ApiState, Store, router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::{Media, MediaId, MediaKind};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

struct ClaimFixture {
    _tmp: tempfile::TempDir,
    state: ApiState,
    media: Media,
    library_id: String,
    path: PathBuf,
}

fn fixture() -> ClaimFixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("Claim.2024.mkv");
    std::fs::write(&path, b"original file").unwrap();
    let state = ApiState::new(
        Store::open(tmp.path().join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        root.clone(),
    )
    .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Claim".into(),
        year: Some(2024),
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let library_id = {
        let shared = state.store();
        let store = shared.lock();
        store.insert_media(&media).unwrap();
        store
            .create_library(
                media.kind,
                "Movies",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
            .id
    };
    state
        .store()
        .lock()
        .insert_unidentified(path.display().to_string(), domain::Confidence::Low)
        .unwrap();
    ClaimFixture {
        _tmp: tmp,
        state,
        media,
        library_id,
        path,
    }
}

/// A real NFO read blocks at the FIFO until its writer publishes the fixture.
struct NfoGate {
    path: PathBuf,
    writer: Option<File>,
}

impl NfoGate {
    fn new(path: &Path) -> Self {
        let path = path.with_extension("nfo");
        let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // CString owns a valid NUL-terminated path for the duration of this call.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        Self { path, writer: None }
    }

    fn wait_for_reader(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&self.path)
            {
                Ok(writer) => {
                    self.writer = Some(writer);
                    return;
                }
                Err(error)
                    if error.raw_os_error() == Some(libc::ENXIO) && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("claim did not enter the NFO read: {error}"),
            }
        }
    }

    fn release(&mut self) -> std::io::Result<()> {
        if let Some(mut writer) = self.writer.take() {
            writer.write_all(b"<movie><plot>Observed claim plot</plot></movie>")?;
        }
        Ok(())
    }
}

impl Drop for NfoGate {
    fn drop(&mut self) {
        let _ = self.release();
        let _ = std::fs::remove_file(&self.path);
    }
}

fn start_claim(fixture: &ClaimFixture) -> mpsc::Receiver<StatusCode> {
    let app = router(fixture.state.clone());
    let body = serde_json::json!({
        "path": fixture.path,
        "action": {"type": "bind_existing", "media_id": fixture.media.id.to_string()},
    });
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/unidentified/claim")
        .header("Authorization", "Bearer management-secret")
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let response = runtime.block_on(app.oneshot(request)).unwrap();
        let _ = sender.send(response.status());
    });
    receiver
}

#[test]
fn blocking_claim_nfo_read_keeps_store_available() {
    let fixture = fixture();
    let mut gate = NfoGate::new(&fixture.path);
    let result = start_claim(&fixture);
    gate.wait_for_reader();
    let shared = fixture.state.store();
    let readable = shared
        .try_lock_for(Duration::from_millis(250))
        .is_some_and(|store| store.get_media(fixture.media.id).unwrap().is_some());
    gate.release().unwrap();
    let status = result.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(readable, "NFO IO must not hold the shared Store mutex");
    assert_eq!(status, StatusCode::OK);
    assert!(
        shared
            .lock()
            .file_coverage_by_path(fixture.path.to_str().unwrap())
            .unwrap()
            .is_some()
    );
}

#[test]
fn changed_source_during_claim_nfo_read_is_not_published() {
    let fixture = fixture();
    let mut gate = NfoGate::new(&fixture.path);
    let result = start_claim(&fixture);
    gate.wait_for_reader();
    std::fs::write(&fixture.path, b"replacement bytes have another size").unwrap();
    gate.release().unwrap();
    let status = result.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_ne!(
        status,
        StatusCode::OK,
        "claim must reject a stale source observation"
    );
    assert!(
        fixture
            .state
            .store()
            .lock()
            .file_coverage_by_path(fixture.path.to_str().unwrap())
            .unwrap()
            .is_none()
    );
}

#[test]
fn reconfigured_library_during_claim_nfo_read_rejects_the_original_context() {
    let fixture = fixture();
    let mut gate = NfoGate::new(&fixture.path);
    let result = start_claim(&fixture);
    gate.wait_for_reader();
    let shared = fixture.state.store();
    let reconfigured = if let Some(store) = shared.try_lock_for(Duration::from_millis(250)) {
        let revision = store.library_config_revision(&fixture.library_id).unwrap();
        store
            .begin_library_reconfiguration(
                &store::library_operations::LibraryReconfiguration {
                    operation_id: "claim-observation-race".into(),
                    library_id: fixture.library_id.parse().unwrap(),
                    expected_revision: revision,
                    root_paths: None,
                    display_locale: Some("zh-CN".into()),
                },
                1_000,
            )
            .unwrap();
        for _ in 0..10 {
            let status = store
                .advance_library_operation("claim-observation-race", 1_100, 100)
                .unwrap();
            if status.state == "succeeded" {
                break;
            }
        }
        assert_eq!(
            store.library_lifecycle_state(&fixture.library_id).unwrap(),
            "ready"
        );
        assert_eq!(
            store.library_config_revision(&fixture.library_id).unwrap(),
            revision + 1
        );
        true
    } else {
        false
    };
    gate.release().unwrap();
    let status = result.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        reconfigured,
        "claim observation must release the Store mutex"
    );
    assert_ne!(
        status,
        StatusCode::OK,
        "claim must retain its original config revision"
    );
    assert!(
        shared
            .lock()
            .file_coverage_by_path(fixture.path.to_str().unwrap())
            .unwrap()
            .is_none()
    );
}

#[test]
fn updated_parked_identity_during_claim_keeps_new_record_and_rejects_file() {
    let fixture = fixture();
    let mut gate = NfoGate::new(&fixture.path);
    let result = start_claim(&fixture);
    gate.wait_for_reader();
    let shared = fixture.state.store();
    let changed = if let Some(store) = shared.try_lock_for(Duration::from_millis(250)) {
        store
            .insert_unidentified(fixture.path.display().to_string(), domain::Confidence::High)
            .unwrap();
        true
    } else {
        false
    };
    gate.release().unwrap();
    assert!(changed, "Claim observation must release Store");
    assert_eq!(
        result.recv_timeout(Duration::from_secs(5)).unwrap(),
        StatusCode::CONFLICT
    );
    let store = shared.lock();
    assert_eq!(
        store
            .get_unidentified(fixture.path.to_str().unwrap())
            .unwrap()
            .unwrap()
            .confidence,
        "high"
    );
    assert!(
        store
            .file_coverage_by_path(fixture.path.to_str().unwrap())
            .unwrap()
            .is_none()
    );
}

#[test]
fn slow_claim_nfo_keeps_same_runtime_timer_and_store_responsive() {
    let fixture = fixture();
    let mut gate = NfoGate::new(&fixture.path);
    let shared = fixture.state.store();
    let release = std::thread::spawn(move || {
        gate.wait_for_reader();
        let readable = shared.try_lock_for(Duration::from_millis(100)).is_some();
        std::thread::sleep(Duration::from_millis(500));
        gate.release().unwrap();
        readable
    });
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/unidentified/claim")
        .header("Authorization", "Bearer management-secret")
        .header("Content-Type", "application/json")
        .body(Body::from(
            serde_json::json!({"path":fixture.path,"action":{
                "type":"bind_existing","media_id":fixture.media.id
            }})
            .to_string(),
        ))
        .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (response, elapsed) = runtime.block_on(async {
        let started = Instant::now();
        tokio::join!(router(fixture.state).oneshot(request), async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            started.elapsed()
        })
    });
    assert!(release.join().unwrap(), "slow NFO must not hold Store");
    assert_eq!(response.unwrap().status(), StatusCode::OK);
    assert!(
        elapsed < Duration::from_millis(200),
        "NFO blocked runtime: {elapsed:?}"
    );
}

#[test]
fn claim_rechecks_maintenance_after_nfo_read_before_publication() {
    let fixture = fixture();
    let mut nfo = NfoGate::new(&fixture.path);
    let result = start_claim(&fixture);
    nfo.wait_for_reader();
    let gate = fixture.state.write_gate();
    let _closed = gate.close();
    nfo.release().unwrap();
    assert_eq!(
        result.recv_timeout(Duration::from_secs(5)).unwrap(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    let handle = fixture.state.store();
    let store = handle.lock();
    assert!(
        store
            .file_coverage_by_path(fixture.path.to_str().unwrap())
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_unidentified(fixture.path.to_str().unwrap())
            .unwrap()
            .is_some()
    );
    assert!(fixture.path.is_file());
}
