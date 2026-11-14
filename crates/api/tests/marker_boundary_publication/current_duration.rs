use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

struct DurationChanged {
    store: Arc<store::Store>,
    rows: Vec<domain::FileCoverage>,
    changed: AtomicBool,
    absence: bool,
    duration_ms: Option<i64>,
}
impl FingerprintEngine for DurationChanged {
    fn compare_fingerprints(
        &self,
        first: &[u32],
        second: &[u32],
        min: f32,
        max: f32,
    ) -> marker::PairFingerprintDecision {
        let candidate = self.find_common_segment(first, second, min, max);
        if self.absence && (matches!(first[0], 1 | 101) || matches!(second[0], 1 | 101)) {
            marker::PairFingerprintDecision::NoMatch
        } else {
            marker::PairFingerprintDecision::Candidates(candidate.into_iter().collect())
        }
    }
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        unreachable!("captures are retained")
    }
    fn find_common_segment(
        &self,
        first: &[u32],
        _: &[u32],
        _: f32,
        _: f32,
    ) -> Option<CommonSegment> {
        if !self.changed.swap(true, Ordering::SeqCst) {
            for row in &self.rows {
                let id = row.file.file_id.to_string();
                let tracks = self.store.get_file_meta(&id).unwrap().unwrap();
                self.store
                    .put_file_meta_versioned(
                        &id,
                        &tracks,
                        Some(&row.file.version.source_version),
                        self.duration_ms,
                    )
                    .unwrap();
            }
        }
        let (start, end) = if first[0] < 100 {
            (30.0, 90.0)
        } else {
            (120.0, 180.0)
        };
        Some(CommonSegment {
            start1_sec: start,
            end1_sec: end,
            start2_sec: start,
            end2_sec: end,
            duration_sec: 60.0,
            score: 0.0,
        })
    }
}

async fn corrected_metadata(absence: bool, duration_ms: Option<i64>) {
    let mut s = scenario().await;
    s.manager.shutdown_workers().await.unwrap();
    let old = store::StoredFileMarker {
        intro_start_ms: Some(120_000),
        intro_end_ms: Some(180_000),
        outro_start_ms: Some(2_330_000),
        outro_end_ms: Some(2_390_000),
        source: "fingerprint".into(),
        locked: false,
        updated_at: 1,
        ..store::test_support::fixture_marker(&s.store, s.media_id, domain::UnitKey::new(1, 1))
    };
    s.store.put_fixture_marker(&old).unwrap();
    let old = s
        .store
        .get_fixture_marker(s.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    for (index, row) in s.ledgers.iter().enumerate() {
        let id = row.file.file_id.to_string();
        let mut cache = s.store.get_fingerprint_cache(&id).unwrap().unwrap();
        cache.intro.words = vec![index as u32 + 1, 10];
        cache.outro.as_mut().unwrap().words = vec![index as u32 + 101, 110];
        s.store.put_fingerprint_cache(&id, &cache).unwrap();
    }
    let before = s
        .ledgers
        .iter()
        .map(|row| {
            s.store
                .get_fingerprint_cache(&row.file.file_id.to_string())
                .unwrap()
        })
        .collect::<Vec<_>>();
    let engine = Arc::new(DurationChanged {
        store: s.store.clone(),
        rows: s.ledgers.clone(),
        changed: AtomicBool::new(false),
        absence,
        duration_ms,
    });
    s.manager = Arc::new(
        ProbeManager::with_engines(s.store.clone(), engine.clone(), Arc::new(FixedCandidate))
            .unwrap(),
    );
    s.manager.set_clock(s.clock.clone());
    assert!(s.manager.try_start_workers());
    probe(&s).await;
    assert!(engine.changed.load(Ordering::SeqCst));
    for (row, cache) in s.ledgers.iter().zip(before) {
        assert_eq!(
            s.store
                .get_fingerprint_cache(&row.file.file_id.to_string())
                .unwrap(),
            cache
        );
    }
    let outcome = latest_outcome(&s);
    if duration_ms == Some(2_400_000) && absence {
        assert_eq!(outcome["status"], "verified", "{outcome}");
        assert!(
            s.store
                .get_fixture_marker(s.media_id, Some(1), Some(1))
                .unwrap()
                .is_none()
        );
    } else {
        assert_eq!(outcome["status"], "partial", "{outcome}");
        assert_eq!(outcome["kinds"]["outro"]["status"], "uncertain");
        let after = s
            .store
            .get_fixture_marker(s.media_id, Some(1), Some(1))
            .unwrap()
            .unwrap();
        assert_eq!(
            (after.outro_start_ms, after.outro_end_ms),
            (old.outro_start_ms, old.outro_end_ms),
            "obsolete tail cannot publish or clear current Outro"
        );
        assert_eq!(
            (after.intro_start_ms, after.intro_end_ms),
            if absence {
                (None, None)
            } else {
                (Some(30_000), Some(90_000))
            }
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrected_duration_only_rejects_old_tail_as_media_edge() {
    corrected_metadata(false, Some(2_410_000)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrected_duration_only_rejects_old_tail_as_complete_absence() {
    corrected_metadata(true, Some(2_410_000)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_current_duration_cannot_borrow_cached_tail_edge() {
    corrected_metadata(true, None).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn current_complete_tail_can_still_clear_proven_absence() {
    corrected_metadata(true, Some(2_400_000)).await;
}
