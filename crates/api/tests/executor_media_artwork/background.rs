use super::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};

#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    fn wait(&self) {
        let (open, _) = self
            .changed
            .wait_timeout_while(self.open.lock().unwrap(), Duration::from_secs(5), |open| {
                !*open
            })
            .unwrap();
        let released = *open;
        drop(open);
        assert!(released, "fixture gate was never released");
    }

    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }
}

struct ReleaseOnDrop(Arc<Gate>);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

struct ArtworkCatalog {
    entered: tokio::sync::mpsc::UnboundedSender<String>,
    gate: Arc<Gate>,
    calls: Mutex<HashMap<String, usize>>,
    panic_first: AtomicBool,
}

impl api::catalog::Catalog for ArtworkCatalog {
    fn search_movie(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn search_tv(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn details(&self, _: MediaKind, _: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
    fn image_candidates(
        &self,
        _: MediaKind,
        id: &str,
    ) -> Result<api::catalog::ArtworkCandidates, String> {
        *self.calls.lock().unwrap().entry(id.into()).or_default() += 1;
        let _ = self.entered.send(id.into());
        assert!(
            !self.panic_first.swap(false, Ordering::SeqCst),
            "fixture catalog panic"
        );
        self.gate.wait();
        Ok(api::catalog::ArtworkCandidates {
            posters: vec![],
            backdrops: vec![api::catalog::ArtworkCandidate {
                file_path: "/fixture.jpg".into(),
                width: 1920,
                height: 1080,
                language: None,
            }],
        })
    }
}

struct FixturePoster;

impl api::PosterFetch for FixturePoster {
    fn get(&self, _: &str) -> Result<Vec<u8>, String> {
        Ok(b"fixture image".to_vec())
    }
}

fn artwork_state(
    root: &Path,
    panic_first: bool,
) -> (
    ApiState,
    String,
    Arc<ArtworkCatalog>,
    tokio::sync::mpsc::UnboundedReceiver<String>,
) {
    let (state, token) = state_with_admin(root);
    let (entered, starts) = tokio::sync::mpsc::unbounded_channel();
    let catalog = Arc::new(ArtworkCatalog {
        entered,
        gate: Arc::new(Gate::default()),
        calls: Mutex::default(),
        panic_first: AtomicBool::new(panic_first),
    });
    let state = state
        .with_catalog(catalog.clone())
        .with_poster_fetch(Arc::new(FixturePoster));
    (state, token, catalog, starts)
}

async fn get_up_next(app: &axum::Router, token: &str) {
    let response = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/playback/up-next",
            token,
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

async fn next_start(starts: &mut tokio::sync::mpsc::UnboundedReceiver<String>) -> String {
    tokio::time::timeout(Duration::from_secs(2), starts.recv())
        .await
        .unwrap()
        .unwrap()
}

async fn wait_for_artwork(paths: &[PathBuf]) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while paths.iter().any(|path| !path.is_file()) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("background work must publish the fixture artwork");
}

#[test]
fn artwork_fanout_is_bounded_deduplicated_and_retries_skipped_targets() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, token, catalog, mut starts) = artwork_state(tmp.path(), false);
    let paths: Vec<_> = (0..3)
        .map(|n| publish_in_progress_movie(&state, &token, &format!("bounded-{n}")))
        .collect();
    let app = router(state.clone());
    let runtime = runtime();
    let _release = ReleaseOnDrop(catalog.gate.clone());
    runtime.block_on(async {
        get_up_next(&app, &token).await;
        let first = next_start(&mut starts).await;
        let second = next_start(&mut starts).await;
        assert_ne!(first, second);
        for _ in 0..3 {
            get_up_next(&app, &token).await;
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(50), starts.recv())
                .await
                .is_err(),
            "repeated requests must neither duplicate targets nor queue over capacity"
        );
        assert_eq!(
            state.http_work.available_permits(),
            8,
            "background artwork has its own capacity"
        );
        catalog.gate.release();
        let admitted: Vec<_> = [first, second]
            .iter()
            .map(|name| {
                paths
                    .iter()
                    .find(|path| path.parent().unwrap().file_name().unwrap().to_str() == Some(name))
                    .unwrap()
                    .clone()
            })
            .collect();
        wait_for_artwork(&admitted).await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while paths.iter().any(|path| !path.is_file()) {
                get_up_next(&app, &token).await;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            catalog
                .calls
                .lock()
                .unwrap()
                .values()
                .all(|count| *count == 1),
            "each target must issue only one background Catalog request"
        );
        for path in &paths {
            assert_eq!(std::fs::read(path).unwrap(), b"fixture image");
        }
    });
}

#[test]
fn artwork_panic_releases_capacity_and_target_for_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, token, catalog, mut starts) = artwork_state(tmp.path(), true);
    let path = publish_in_progress_movie(&state, &token, "panic-retry");
    let app = router(state);
    let runtime = runtime();
    let _release = ReleaseOnDrop(catalog.gate.clone());
    runtime.block_on(async {
        get_up_next(&app, &token).await;
        assert_eq!(next_start(&mut starts).await, "panic-retry");
        catalog.gate.release();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !path.is_file() {
                get_up_next(&app, &token).await;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"fixture image");
        assert_eq!(catalog.calls.lock().unwrap()["panic-retry"], 2);
    });
}

struct PauseStillObservation(Arc<Gate>);

impl api::file_stat_read::FileStatRead for PauseStillObservation {
    fn read_nfo(&self, _: &Path) -> Option<library::NfoMeta> {
        None
    }
    fn metadata(&self, path: &Path) -> std::io::Result<std::fs::Metadata> {
        std::fs::metadata(path)
    }
    fn is_file(&self, path: &Path) -> bool {
        if path.to_string_lossy().ends_with("-thumb.jpg") {
            self.0.wait();
        }
        path.is_file()
    }
}

#[test]
fn cancelled_response_keeps_background_target_reserved_until_worker_finishes() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, token, catalog, mut starts) = artwork_state(tmp.path(), false);
    let path = publish_in_progress_movie(&state, &token, "cancel-response");
    let render_gate = Arc::new(Gate::default());
    let paused_app = router(
        state
            .clone()
            .with_file_stat_read(Arc::new(PauseStillObservation(render_gate.clone()))),
    );
    let app = router(state);
    let runtime = runtime();
    let _release_catalog = ReleaseOnDrop(catalog.gate.clone());
    let _release_render = ReleaseOnDrop(render_gate.clone());
    runtime.block_on(async {
        let request_token = token.clone();
        let pending = tokio::spawn(async move { get_up_next(&paused_app, &request_token).await });
        assert_eq!(next_start(&mut starts).await, "cancel-response");
        assert!(
            !pending.is_finished(),
            "render gate must leave the response pending"
        );
        pending.abort();
        assert!(pending.await.unwrap_err().is_cancelled());
        get_up_next(&app, &token).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(50), starts.recv())
                .await
                .is_err(),
            "cancelling the requester must retain the worker's target reservation"
        );
        render_gate.release();
        catalog.gate.release();
        wait_for_artwork(&[path]).await;
        assert_eq!(catalog.calls.lock().unwrap()["cancel-response"], 1);
    });
}
