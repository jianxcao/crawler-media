//! Concurrent callers share finite upstream work and observe the same real result.
use domain::MediaKind;
use media::{CatalogGet, Tmdb, TmdbError};
use std::sync::{
    Arc, Barrier,
    atomic::{AtomicBool, Ordering},
};
struct FiniteOrigin(AtomicBool);
impl CatalogGet for FiniteOrigin {
    fn get(&self, _: &str) -> Result<String, TmdbError> {
        if self.0.swap(false, Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(100));
            Ok(include_str!("fixtures/rich_movie_details.json").into())
        } else {
            Err(TmdbError::Http(
                "finite origin already serving one response".into(),
            ))
        }
    }
}
#[test]
fn simultaneous_cold_reads_share_a_complete_catalog_result() {
    let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let tmdb = Arc::new(
        Tmdb::new_at(
            FiniteOrigin(AtomicBool::new(true)),
            &dir.path().join("cache.db"),
            1_000_000,
        )
        .unwrap(),
    );
    let ready = Arc::new(Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let tmdb = tmdb.clone();
            let ready = ready.clone();
            std::thread::spawn(move || {
                ready.wait();
                tmdb.details_with_meta(MediaKind::Movie, "603")
            })
        })
        .collect();
    for worker in workers {
        let metadata=worker.join().unwrap().expect("coalesced callers must receive complete result instead of competing with finite origin");
        assert!(metadata.overview.is_some());
        assert!(!metadata.cast.is_empty());
    }
}

struct BoundedOrigin {
    active: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
}
impl CatalogGet for BoundedOrigin {
    fn get(&self, _: &str) -> Result<String, TmdbError> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(100));
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(include_str!("fixtures/rich_movie_details.json").into())
    }
}
#[derive(Clone)]
struct SharedBoundedOrigin(Arc<BoundedOrigin>);
impl CatalogGet for SharedBoundedOrigin {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
        self.0.get(path)
    }
}
#[test]
fn unrelated_cold_keys_keep_complete_results_under_a_fixed_upstream_bound() {
    // Candidate fixed source budget=8, with enough bounded pending capacity for16.
    // Parent may choose a smaller fixed bound; raising this assertion is not a fix.
    let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let origin = Arc::new(BoundedOrigin {
        active: std::sync::atomic::AtomicUsize::new(0),
        peak: std::sync::atomic::AtomicUsize::new(0),
    });
    let tmdb = Arc::new(
        Tmdb::new_at(
            SharedBoundedOrigin(origin.clone()),
            &dir.path().join("cache.db"),
            1_000_000,
        )
        .unwrap(),
    );
    let ready = Arc::new(Barrier::new(16));
    let workers: Vec<_> = (0..16)
        .map(|key| {
            let tmdb = tmdb.clone();
            let ready = ready.clone();
            std::thread::spawn(move || {
                ready.wait();
                tmdb.details_with_meta(MediaKind::Movie, &key.to_string())
            })
        })
        .collect();
    for worker in workers {
        let metadata = worker
            .join()
            .unwrap()
            .expect("bounded queued cold reads retain complete results");
        assert!(metadata.overview.is_some());
        assert!(!metadata.cast.is_empty());
    }
    assert!(
        origin.peak.load(Ordering::SeqCst) <= 8,
        "different cold keys must not bypass the shared upstream capacity"
    );
}
