use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use axum::{body::Body, http::Request};
use tower::ServiceExt;

use super::{ProbeManager, ProbeUnit, TracksProbe};

type Gate = Arc<(Mutex<bool>, Condvar)>;

struct PendingTracks {
    entered: Mutex<Option<tokio::sync::oneshot::Sender<library::WorkControl>>>,
    released: Gate,
}

impl TracksProbe for PendingTracks {
    fn probe(
        &self,
        _: &Path,
        control: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        if let Some(entered) = self.entered.lock().unwrap().take() {
            let _ = entered.send(control.clone());
        }
        let (released, changed) = self.released.as_ref();
        let (released, _) = changed
            .wait_timeout_while(released.lock().unwrap(), Duration::from_secs(5), |value| {
                !*value
            })
            .unwrap();
        if !*released {
            return Err(library::LibraryError::Probe(
                "probe gate not released".into(),
            ));
        }
        // A completed read may arrive even after cancellation. The real pipeline
        // must refuse publication instead of relying on the injected IO to do it.
        Ok((
            library::Tracks {
                video: Some(library::VideoTrack {
                    codec: Some("h264".into()),
                    width: Some(1920),
                    height: Some(1080),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Some(120_000),
        ))
    }
}

struct ReleaseTracks(Gate);
impl Drop for ReleaseTracks {
    fn drop(&mut self) {
        let (released, changed) = self.0.as_ref();
        *released.lock().unwrap() = true;
        changed.notify_all();
    }
}

struct Offline;
impl indexer::Fetcher for Offline {
    fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        Err(indexer::IndexerError::Fetch(
            "offline removal fixture".into(),
        ))
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    state: crate::ApiState,
    unit: ProbeUnit,
    entered: tokio::sync::oneshot::Receiver<library::WorkControl>,
    release: ReleaseTracks,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(store::Store::open(temp.path().join("data")).unwrap());
        let root = temp.path().join("movies");
        std::fs::create_dir_all(&root).unwrap();
        store
            .create_library(
                domain::MediaKind::Movie,
                "Race movie",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let media = domain::Media {
            id: domain::MediaId::new(),
            kind: domain::MediaKind::Movie,
            title: "Race movie".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&media).unwrap();
        let source = root.join("movie.strm");
        std::fs::write(&source, b"https://example.invalid/movie.mkv\n").unwrap();
        std::fs::write(source.with_extension("nfo"), ORIGINAL_NFO).unwrap();
        let row = super::test_support::coverage(media.id, source.to_str().unwrap(), None, None);
        let row = super::test_support::publish(&store, &row);
        let mut state = crate::ApiState::new_arc(
            store.clone(),
            "removal-secret".into(),
            indexer::ProfileSet::load(None).unwrap(),
            Arc::new(Offline),
            Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
        )
        .unwrap();
        let (entered, receiver) = tokio::sync::oneshot::channel();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let manager = Arc::new(
            ProbeManager::new(store)
                .unwrap()
                .with_tracks_probe(Arc::new(PendingTracks {
                    entered: Mutex::new(Some(entered)),
                    released: gate.clone(),
                })),
        );
        manager.set_library_registry(state.library_work());
        state.probe = manager;
        Self {
            _temp: temp,
            state,
            unit: ProbeUnit {
                row,
                kind: domain::MediaKind::Movie,
                force_fingerprint: false,
                reuse_fingerprint_cache: false,
                overwrite_markers: false,
                reuse_media_info_cache: false,
                marker_refresh_id: None,
                job_id: None,
            },
            entered: receiver,
            release: ReleaseTracks(gate),
        }
    }
}

const ORIGINAL_NFO: &[u8] = b"<movie><title>Original</title></movie>";

async fn delete_and_wait(state: &crate::ApiState, library: &str) {
    let app = crate::router(state.clone());
    let response = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/v1/libraries/{library}"))
                .header("Authorization", "Bearer removal-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::ACCEPTED);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let operation = body["data"]["operation_id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let response = app
                .clone()
                .oneshot(
                    Request::get(format!("/api/v1/library-removals/{operation}"))
                        .header("Authorization", "Bearer removal-secret")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            if body["data"]["state"] == "succeeded" {
                break;
            }
            assert_ne!(body["data"]["state"], "failed", "{body}");
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("removal must finish before the pending probe is released");
}

/// Characterization of the public removal boundary. The pre-fix RED is the
/// independent real STRM E2E; this test does not arrange HashMap iteration.
#[test]
fn removal_refuses_late_tracks_sidecar_and_file_facts() {
    let fixture = Fixture::new(); // Construct before starting a runtime/worker.
    let Fixture {
        _temp,
        state,
        unit,
        entered,
        release,
    } = fixture;
    let library = unit.row.file.library_id.to_string();
    let other_work = state.acquire_library_work(&library).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        assert!(state.probe.enqueue_force(unit.clone()));
        assert!(state.probe.try_start_workers());
        let control = tokio::time::timeout(Duration::from_secs(2), entered)
            .await
            .unwrap()
            .unwrap();
        drop(other_work);
        delete_and_wait(&state, &library).await;
        let cancelled = control.is_cancelled();
        drop(release);
        state.probe.shutdown_workers().await.unwrap();
        assert!(cancelled, "removal must cancel the actual stream reader");
        assert_eq!(
            std::fs::read(unit.row.file.path.with_extension("nfo")).unwrap(),
            ORIGINAL_NFO
        );
        assert_eq!(
            std::fs::read(&unit.row.file.path).unwrap(),
            b"https://example.invalid/movie.mkv\n"
        );
        let store = state.store();
        assert!(
            store
                .file_coverage_by_id(unit.row.file.file_id)
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get_file_meta(&unit.row.file.file_id.to_string())
                .unwrap()
                .is_none()
        );
    });
}

#[test]
fn finishing_unrelated_work_keeps_the_tracks_probe_alive() {
    let Fixture {
        _temp,
        state,
        unit,
        entered,
        release,
    } = Fixture::new();
    let other_work = state
        .acquire_library_work(&unit.row.file.library_id.to_string())
        .unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        assert!(state.probe.enqueue_force(unit.clone()));
        assert!(state.probe.try_start_workers());
        let control = tokio::time::timeout(Duration::from_secs(2), entered)
            .await
            .unwrap()
            .unwrap();
        drop(other_work);
        let alive = !control.is_cancelled();
        drop(release);
        state.probe.shutdown_workers().await.unwrap();
        assert!(alive, "ending an unrelated scan must not cancel stream IO");
        let body = std::fs::read_to_string(unit.row.file.path.with_extension("nfo")).unwrap();
        assert!(body.contains("<streamdetails>"));
        let details = state
            .store()
            .get_file_meta(&unit.row.file.file_id.to_string())
            .unwrap()
            .expect("the admitted probe must publish its file facts");
        assert_eq!(details.video.unwrap().width, Some(1920));
    });
}
