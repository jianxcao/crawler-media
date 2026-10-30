//! Task 22 slice 2: the drivers `ApiState::new` starts must not hold the executor.
//!
//! Request handlers and background drivers (`chapter_frame_service`, probe workers,
//! obligation dispatcher) must move blocking database work off the tokio executor.
//! A driver waiting for a Library writer on the executor would delay `/health` and
//! unrelated timers while a scan is writing.
//!
//! The measurement is bounded on purpose: an executor parked by a driver would
//! otherwise hang the test, so the request runs on the runtime thread while the
//! test thread waits on a channel with a deadline.
mod writer_hold {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/writer_hold.rs"
    ));
}

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use api::probe_manager::policy::ProbeRequestOrigin;
use api::{ApiState, Store, router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use store::test_support::{FixtureFile, publish_fixture_file};
use tower::ServiceExt;

struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

fn request(method: &str, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

/// Boot `ApiState` **inside** a current-thread runtime, so its drivers are tasks on
/// that single executor thread — the strictest version of the production shape.
fn boot_inside_runtime(root: &std::path::Path) -> (tokio::runtime::Runtime, ApiState) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let state = runtime.block_on(async {
        ApiState::new(
            Store::open(root.join("data")).unwrap(),
            "drivers-secret".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(root.join("stage"))),
            root.join("library"),
        )
        .unwrap()
    });
    (runtime, state)
}

/// A ledgered file whose probe facts are missing: a probe request therefore wakes
/// the metadata worker, which reads the store from its async loop.
fn seed_probe_candidate(state: &ApiState) -> domain::FileCoverage {
    let store = state.store();
    let guard = store.as_ref();
    let root = guard
        .list_libraries()
        .unwrap()
        .into_iter()
        .find(|library| library.kind == MediaKind::Movie)
        .and_then(|library| library.root_paths.first().cloned())
        .expect("seeded movie Library root");
    std::fs::create_dir_all(&root).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Drivers".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    guard.insert_media(&media).unwrap();
    let path = root.join("drivers.mkv");
    std::fs::write(&path, b"video-bytes").unwrap();
    let file = publish_fixture_file(
        guard,
        &FixtureFile {
            file_id: Some(LedgerId::new()),
            media_id: media.id,
            path: path.display().to_string(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::Low,
            filter_score: None,
            ..Default::default()
        },
    )
    .unwrap();
    domain::FileCoverage {
        path_key: String::new(),
        file,
        units: vec![domain::UnitKey::WHOLE],
    }
}

#[test]
fn health_answers_while_a_driver_polls_the_store_behind_a_write() {
    let tmp = tempfile::tempdir().unwrap();
    let (runtime, state) = boot_inside_runtime(tmp.path());
    let coverage = seed_probe_candidate(&state);
    // Wake a driver: the probe workers read the ledger from their async loop.
    let queued = state
        .probe
        .request_probe(&coverage, ProbeRequestOrigin::Detail)
        .expect("probe request");
    let job_id = match queued {
        api::probe_manager::policy::ProbeRequestResult::Queued { job_id } => job_id,
        other => panic!("the fixture must actually wake a driver, got {other:?}"),
    };

    let app = router(state.clone());
    // A scan-sized write in flight: another thread holds the process-wide Store.
    let held = state.store();
    let holder = std::thread::spawn(move || {
        let _guard = crate::writer_hold::WriterHold::acquire(held.clone(), store::Db::Library);
        std::thread::sleep(Duration::from_millis(2000));
    });
    std::thread::sleep(Duration::from_millis(50));

    // Drive the runtime on its own thread for long enough that the woken driver is
    // polled while the lock is held. The block_on has to yield (the sleep) so the
    // drivers — not just the request — get scheduled; a request that completes
    // before any driver is polled proves nothing.
    let (tx, rx) = std::sync::mpsc::channel();
    let measured = std::thread::spawn(move || {
        let outcome = runtime.block_on(async move {
            let request = tokio::spawn(async move {
                let started = Instant::now();
                let status = app
                    .oneshot(request("GET", "/api/v1/health"))
                    .await
                    .unwrap()
                    .status();
                (status, started.elapsed())
            });
            tokio::time::sleep(Duration::from_millis(1500)).await;
            let answered = request.await;
            // Keep the runtime driving after the batch ends: the woken worker needs
            // to finish its unit so the engagement check below is meaningful.
            for _ in 0..40 {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            answered
        });
        let _ = tx.send(outcome);
    });
    // Bounded: a parked executor must fail the test, never hang it.
    let outcome = rx.recv_timeout(Duration::from_secs(8));
    holder.join().unwrap();
    let outcome = outcome
        .expect("a driver held the executor: the runtime never reached the end of the window");
    measured.join().unwrap();
    let (status, elapsed) = outcome.expect("/health task failed");
    assert_eq!(status, StatusCode::OK, "/health must stay healthy");
    assert!(
        elapsed < Duration::from_millis(800),
        "/health took {elapsed:?}: a driver parked the executor behind the Store lock"
    );

    // Engagement proof. A driver that never woke cannot park anything, so the
    // measurement above only means something if the probe worker really ran this
    // unit: its job must have left `queued`.
    let store_handle = state.store();
    let status = {
        let guard = store_handle.as_ref();
        guard
            .get_probe_job(&job_id)
            .expect("probe job read")
            .map(|job| job.status)
    };
    assert!(
        status.as_deref().is_some_and(|status| status != "queued"),
        "the probe worker never ran the unit (job status {status:?}): the /health \
         measurement above proved nothing about the driver"
    );
}

#[test]
fn probe_resumes_after_store_contention_without_cancelling_the_job() {
    let tmp = tempfile::tempdir().unwrap();
    let (runtime, state) = boot_inside_runtime(tmp.path());
    let coverage = seed_probe_candidate(&state);
    let result = state
        .probe
        .request_probe(&coverage, ProbeRequestOrigin::Detail)
        .unwrap();
    let job_id = match result {
        api::probe_manager::policy::ProbeRequestResult::Queued { job_id } => job_id,
        other => panic!("expected a queued probe, got {other:?}"),
    };
    let held = state.store();
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = crate::writer_hold::WriterHold::acquire(held.clone(), store::Db::Library);
        locked_tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(2500));
    });
    locked_rx.recv().unwrap();
    let store = state.store();
    let status = runtime.block_on(async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let store = store.clone();
            let job_id = job_id.clone();
            let status = tokio::task::spawn_blocking(move || {
                store
                    .as_ref()
                    .get_probe_job(&job_id)
                    .unwrap()
                    .unwrap()
                    .status
            })
            .await
            .unwrap();
            if !matches!(status.as_str(), "queued" | "running") {
                break status;
            }
            if tokio::time::Instant::now() >= deadline {
                break status;
            }
        }
    });
    holder.join().unwrap();
    assert_ne!(
        status, "cancelled",
        "Store contention is not a cancellation"
    );
    assert!(
        !matches!(status.as_str(), "queued" | "running"),
        "probe did not resume after Store contention: {status}"
    );
}

/// A runner that must never be reached: the assertion is about the driver's own
/// scheduling, not about rendering.
struct NeverRuns;

impl api::chapter_frame_service::ChapterFrameRunner for NeverRuns {
    fn render(
        &self,
        _request: &api::chapter_frame_service::ChapterFrameRequest,
        _control: &library::WorkControl,
    ) -> Result<Vec<api::chapter_frame_service::ChapterFrameStaged>, String> {
        Err("test runner: no rendering in this test".into())
    }
}

/// The chapter-frame worker is the driver that wakes on every new obligation, so it
/// is the one that most often touches the `Store` while a scan-sized write runs.
/// Everything it does on wake (`fill_window`, `resolve_target`, `finish_task`) must
/// happen off the executor.
#[test]
fn chapter_frame_worker_does_not_park_the_executor() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (service, store) = runtime.block_on(async {
        let store = Arc::new(Store::open(tmp.path().join("data")).unwrap());
        let registry = Arc::new(api::library_lifecycle::LibraryWorkRegistry::default());
        let service = Arc::new(
            api::chapter_frame_service::ChapterFrameService::with_runner(
                store.clone(),
                registry,
                Arc::new(NeverRuns),
            ),
        );
        service.start();
        (service, store)
    });
    let file_id = {
        let guard = store.as_ref();
        let root = guard
            .list_libraries()
            .unwrap()
            .into_iter()
            .find(|library| library.kind == MediaKind::Movie)
            .and_then(|library| library.root_paths.first().cloned())
            .expect("seeded movie Library root");
        std::fs::create_dir_all(&root).unwrap();
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: "Chapters".into(),
            year: Some(2024),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        guard.insert_media(&media).unwrap();
        let path = root.join("chapters.mkv");
        std::fs::write(&path, b"video-bytes").unwrap();
        publish_fixture_file(
            guard,
            &FixtureFile {
                file_id: Some(LedgerId::new()),
                media_id: media.id,
                path: path.display().to_string(),
                season: None,
                episode: None,
                resolution: Some("1080p".into()),
                codec: Some("h264".into()),
                hdr: None,
                quality_source: QualitySource::Probe,
                confidence: Confidence::High,
                filter_score: None,
                ..Default::default()
            },
        )
        .unwrap()
        .file_id
    };
    assert!(
        service
            .enqueue_ledger(&file_id.to_string(), vec![(0, 1_000)])
            .unwrap(),
        "the obligation must be accepted, or no worker is woken"
    );

    let held = store.clone();
    let holder = std::thread::spawn(move || {
        let _guard = crate::writer_hold::WriterHold::acquire(held.clone(), store::Db::Library);
        std::thread::sleep(Duration::from_millis(1500));
    });
    std::thread::sleep(Duration::from_millis(50));

    let (tx, rx) = std::sync::mpsc::channel();
    let measured = std::thread::spawn(move || {
        let ticks = runtime.block_on(async move {
            let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let ticker = {
                let counter = counter.clone();
                tokio::spawn(async move {
                    for _ in 0..20 {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                })
            };
            let _ = tokio::time::timeout(Duration::from_millis(1300), ticker).await;
            counter.load(std::sync::atomic::Ordering::SeqCst)
        });
        let _ = tx.send(ticks);
    });
    // Bounded: a parked executor must fail the test, never hang it.
    let ticks = rx
        .recv_timeout(Duration::from_secs(8))
        .expect("the chapter-frame worker held the executor: the ticker never ran");
    holder.join().unwrap();
    measured.join().unwrap();
    assert!(
        ticks >= 15,
        "only {ticks}/20 ticks fired in 1.3 s: the chapter-frame worker parked the executor on the Store lock"
    );
}
/// A `TracksProbe` that parks inside the metadata stage until the test releases it.
///
/// This is the instrument slice 2b lacked: the stage's store windows (the cache
/// lookup before it, the publish after it) are reached at a moment the test
/// controls, instead of hoping the worker happens to be inside the hold. Bounded on
/// purpose — the park gives up after `PARK_BUDGET` and fails the stage, so a broken
/// test fails instead of hanging.
struct GatedTracksProbe {
    entered: Arc<std::sync::atomic::AtomicUsize>,
    released: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

const PARK_BUDGET: Duration = Duration::from_secs(10);

impl GatedTracksProbe {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            released: Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new())),
        })
    }

    fn entered(&self) -> bool {
        self.entered.load(std::sync::atomic::Ordering::SeqCst) > 0
    }

    fn release(&self) {
        let (lock, cv) = &*self.released;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }
}

impl api::probe_manager::TracksProbe for GatedTracksProbe {
    fn probe(
        &self,
        _path: &Path,
        _control: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        self.entered
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (lock, cv) = &*self.released;
        let released = lock.lock().unwrap();
        let (released, _) = cv
            .wait_timeout_while(released, PARK_BUDGET, |released| !*released)
            .expect("probe gate mutex");
        if !*released {
            return Err(library::LibraryError::Probe(
                "test gate was never released".into(),
            ));
        }
        Ok((
            library::Tracks {
                video: Some(library::VideoTrack {
                    stream_index: Some(0),
                    codec: Some("h264".into()),
                    profile: None,
                    width: Some(1920),
                    height: Some(1080),
                    is_default: true,
                    frame_rate: Some(24.0),
                    average_frame_rate: None,
                    bit_rate: Some(4_000_000),
                    duration_secs: Some(1.0),
                    aspect_ratio: None,
                    sample_aspect_ratio: None,
                    pixel_format: Some("yuv420p".into()),
                    bit_depth: Some(8),
                    ..Default::default()
                }),
                audio: Vec::new(),
                subtitles: Vec::new(),
            },
            Some(1_000),
        ))
    }
}

/// The instrument slice 2b needed, kept because it is where the measurement
/// dead-ends — not because it passes.
///
/// What it does: a gated `TracksProbe` parks the worker *inside* the metadata stage,
/// so the stage's store window is reached at a moment the test controls instead of
/// hoped for. The test then holds the `Store` from another thread, releases the gate,
/// and counts an unrelated ticker.
///
/// Why it is `#[ignore]`d: with the 2 s hold used here, the post-2b publish step
/// gives up before the ticker starts. `publish_probe_tracks` is now reached through
/// `store_wait` with `STORE_WRITE_WAIT` (1 s), so a 2 s hold expires the budget and
/// the stage returns `Err` **by design** — the ticker assertion then passes for the
/// wrong reason, and the explicit vacuity assertion below catches that. That is the
/// diagnostic answer of task 32: neither `publish_probe_tracks` nor
/// `observe_source`/`ensure_source_recorded` short-circuits; the bounded write wait
/// does, because the instrument's hold is longer than the budget it is testing.
///
/// To un-ignore: hold the `Store` for **less than `STORE_WRITE_WAIT`** (e.g. 700 ms)
/// and start the tick window after the gate release, so the post-fix path completes
/// within budget while an unbounded writer wait on the executor would still park it
/// for the whole hold. Then this test should be red on the pre-fix code and green on
/// the current code, and the vacuity assertion below proves the stage really
/// published.
#[ignore = "measurement dead-end: a 2 s hold exceeds STORE_WRITE_WAIT, so the stage gives up before publishing; see the doc comment for the fix"]
#[test]
fn probe_metadata_publish_window_is_unmeasurable_with_a_hold_longer_than_the_write_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let store = Arc::new(Store::open(tmp.path().join("data")).unwrap());
    let coverage = seed_probe_candidate_in(&store);
    let probe = GatedTracksProbe::new();
    let manager = Arc::new(
        api::probe_manager::ProbeManager::new(store.clone())
            .unwrap()
            .with_tracks_probe(probe.clone() as Arc<dyn api::probe_manager::TracksProbe>),
    );
    assert!(
        runtime.block_on(async { manager.try_start_workers() }),
        "probe workers must start on this runtime"
    );
    assert!(
        matches!(
            manager.request_probe(&coverage, ProbeRequestOrigin::Detail),
            Ok(api::probe_manager::policy::ProbeRequestResult::Queued { .. })
        ),
        "the fixture must actually enqueue a probe unit"
    );

    // 1. Drive the runtime until the worker is parked inside the stage.
    let parked = runtime.block_on(async {
        for _ in 0..500 {
            if probe.entered() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    });
    assert!(
        parked,
        "the probe worker never reached the tracks stage: the measurement would prove nothing"
    );

    // 2. Hold the Store, then release the gate: the publish now runs with it held.
    let held = store.clone();
    let holder = std::thread::spawn(move || {
        let _guard = crate::writer_hold::WriterHold::acquire(held.clone(), store::Db::Library);
        std::thread::sleep(Duration::from_millis(2000));
    });
    std::thread::sleep(Duration::from_millis(50));
    probe.release();

    // 3. Count ticks on the runtime while the stage finishes its publish.
    let (tx, rx) = std::sync::mpsc::channel();
    let measured = std::thread::spawn(move || {
        let ticks = runtime.block_on(async {
            let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let ticker = {
                let counter = counter.clone();
                tokio::spawn(async move {
                    for _ in 0..20 {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                })
            };
            let _ = tokio::time::timeout(Duration::from_millis(1300), ticker).await;
            counter.load(std::sync::atomic::Ordering::SeqCst)
        });
        let _ = tx.send(ticks);
    });
    let ticks = rx
        .recv_timeout(Duration::from_secs(12))
        .expect("the probe stage held the executor: the ticker never finished");
    holder.join().unwrap();
    measured.join().unwrap();
    // Engagement proof for *this* window: the publish really ran (the cache row and
    // its version are written by that step), so "no park" is not "nothing happened".
    let ledger_id = coverage.file.file_id.to_string();
    let published = {
        let guard = store.as_ref();
        guard
            .get_media_info_cache_version(&ledger_id)
            .ok()
            .flatten()
            .is_some()
    };
    assert!(
        published,
        "the stage never reached its publish step: the tick count below proves nothing"
    );
    assert!(
        ticks >= 15,
        "only {ticks}/20 ticks fired in 1.3 s: the probe stage parked the executor on the Store lock"
    );
}

/// Publish one ledgered file under the default movie Library root, for a store the
/// test owns directly (no `ApiState`).
fn seed_probe_candidate_in(store: &Arc<Store>) -> domain::FileCoverage {
    let guard = store.as_ref();
    let root = guard
        .list_libraries()
        .unwrap()
        .into_iter()
        .find(|library| library.kind == MediaKind::Movie)
        .and_then(|library| library.root_paths.first().cloned())
        .expect("seeded movie Library root");
    std::fs::create_dir_all(&root).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Stage".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    guard.insert_media(&media).unwrap();
    let path = root.join(format!("stage-{}.mkv", media.id));
    std::fs::write(&path, b"video-bytes").unwrap();
    let file = publish_fixture_file(
        guard,
        &FixtureFile {
            file_id: Some(LedgerId::new()),
            media_id: media.id,
            path: path.display().to_string(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::Low,
            filter_score: None,
            ..Default::default()
        },
    )
    .unwrap();
    domain::FileCoverage {
        path_key: String::new(),
        file,
        units: vec![domain::UnitKey::WHOLE],
    }
}

#[path = "driver_executor_bounds/storage_review.rs"]
mod storage_review;
