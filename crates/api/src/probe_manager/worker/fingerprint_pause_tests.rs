use crate::probe_manager::{ProbeManager, ProbeUnit, probe::FingerprintWork};
use domain::{Media, MediaId, MediaKind};
use marker::{
    CaptureFailure, CaptureFailureKind, CaptureMetrics, CapturedFingerprint,
    FingerprintCaptureEngine, FingerprintEngine,
};
use std::sync::Arc;
use tokio::sync::mpsc;

struct Matcher;

impl FingerprintEngine for Matcher {
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        unreachable!("capture engine supplies fingerprints")
    }

    fn find_common_segment(
        &self,
        _: &[u32],
        _: &[u32],
        _: f32,
        _: f32,
    ) -> Option<marker::CommonSegment> {
        None
    }
}

struct ObserveAdmission(mpsc::UnboundedSender<std::path::PathBuf>);

struct ResetRemoteGate;

impl Drop for ResetRemoteGate {
    fn drop(&mut self) {
        marker::media_read::set_remote_reads_paused(false);
        marker::media_read::configure_remote_reads(
            marker::media_read::DEFAULT_REMOTE_READ_LIMIT,
            marker::media_read::DEFAULT_REMOTE_READ_COOLDOWN,
        );
    }
}

impl FingerprintCaptureEngine for ObserveAdmission {
    fn capture_window(
        &self,
        request: &marker::CaptureRequest,
    ) -> Result<CapturedFingerprint, CaptureFailure> {
        self.capture_window_controlled(request, &marker::WorkControl::default())
    }

    fn capture_window_controlled(
        &self,
        request: &marker::CaptureRequest,
        control: &marker::WorkControl,
    ) -> Result<CapturedFingerprint, CaptureFailure> {
        let _ = self.0.send(request.path.clone());
        let _permit =
            marker::media_read::acquire_media_read(&request.path, control).map_err(|message| {
                CaptureFailure {
                    kind: CaptureFailureKind::Cancelled,
                    message,
                    metrics: CaptureMetrics::default(),
                }
            })?;
        Ok(CapturedFingerprint {
            window: request.window.clone(),
            words: vec![1, 2, 3, 4],
            pcm_duration_ms: Some(request.window.duration_ms()),
            metrics: CaptureMetrics::default(),
        })
    }
}

fn probe_unit(row: domain::FileCoverage, job_id: String) -> ProbeUnit {
    ProbeUnit {
        kind: MediaKind::Tv,
        row,
        force_fingerprint: true,
        reuse_fingerprint_cache: false,
        overwrite_markers: false,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: Some(job_id),
    }
}

fn persist_fingerprint_job(store: &store::Store, unit: &ProbeUnit, job_id: &str) {
    let ledger_id = unit.row.file.file_id.to_string();
    assert!(
        store
            .create_probe_job(
                job_id,
                "fingerprint_probe",
                &unit.row.file.media_id.to_string(),
                Some(1),
                job_id,
                &[store::ProbeJobUnitSpec {
                    ledger_id: &ledger_id,
                    kind: "tv",
                    force_fingerprint: true,
                    reuse_fingerprint_cache: false,
                    overwrite_markers: false,
                    reuse_media_info_cache: true,
                }],
            )
            .unwrap()
    );
}

fn enqueue_backlog(
    manager: &ProbeManager,
    store: &crate::Store,
    root: &std::path::Path,
    media_id: MediaId,
) -> (Vec<ProbeUnit>, std::path::PathBuf) {
    let mut remote_units = Vec::new();
    for index in 0..32 {
        let path = root.join(format!("S01E{index:02}.strm"));
        std::fs::write(&path, "https://example.invalid/episode.mkv\n").unwrap();
        let mut coverage = crate::probe_manager::test_support::coverage(
            media_id,
            path.to_str().unwrap(),
            Some(1),
            Some(index),
        );
        coverage.file.kind = MediaKind::Tv;
        let row = crate::probe_manager::test_support::publish(store, &coverage);
        let job_id = format!("remote-{index:02}");
        let unit = probe_unit(row, job_id.clone());
        persist_fingerprint_job(store, &unit, &job_id);
        let source_version = unit.row.file.version.source_version.clone();
        if index < crate::probe_manager::dispatch::FINGERPRINT_WINDOW as u32 {
            let ledger_id = unit.row.file.file_id.to_string();
            manager.seen.lock().insert(ledger_id.clone());
            manager
                .fingerprint_tx
                .try_send((
                    store::ProbeDispatchId {
                        job_id: job_id.clone(),
                        ledger_id,
                        stage: store::ProbeDispatchStage::Fingerprint,
                    },
                    FingerprintWork {
                        unit: unit.clone(),
                        media_duration_ms: Some(2_400_000),
                        source_version,
                        duration_secs: 180,
                        start_job: true,
                    },
                ))
                .unwrap();
        }
        remote_units.push(unit);
    }

    let local_path = root.join("S01E99.mkv");
    std::fs::write(&local_path, b"local fixture").unwrap();
    let mut local_coverage = crate::probe_manager::test_support::coverage(
        media_id,
        local_path.to_str().unwrap(),
        Some(1),
        Some(99),
    );
    local_coverage.file.kind = MediaKind::Tv;
    let local_row = crate::probe_manager::test_support::publish(store, &local_coverage);
    let local_job_id = "zz-local".to_string();
    let local_unit = probe_unit(local_row, local_job_id.clone());
    persist_fingerprint_job(store, &local_unit, &local_job_id);
    (remote_units, local_path)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paused_fingerprint_backlog_releases_worker_for_a_local_file() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(crate::Store::open(directory.path().join("data")).unwrap());
    let root = directory.path().join("shows");
    std::fs::create_dir_all(&root).unwrap();
    let library = store
        .create_library(
            MediaKind::Tv,
            "Shows",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store
        .set_library_intro_settings(&library.id, true, true)
        .unwrap();
    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "Paused reads".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();

    let (capture_tx, mut capture_rx) = mpsc::unbounded_channel();
    let _reset_gate = ResetRemoteGate;
    let manager = Arc::new(
        ProbeManager::with_engines(
            store.clone(),
            Arc::new(Matcher),
            Arc::new(ObserveAdmission(capture_tx)),
        )
        .unwrap(),
    );
    let busy = root.join("busy.strm");
    std::fs::write(&busy, "https://example.invalid/busy.mkv\n").unwrap();
    marker::media_read::configure_remote_reads(1, std::time::Duration::ZERO);
    marker::media_read::set_remote_reads_paused(false);
    let occupied =
        marker::media_read::acquire_media_read(&busy, &marker::WorkControl::default()).unwrap();
    let (remote_units, local_path) = enqueue_backlog(&manager, &store, &root, media_id);

    assert!(manager.try_start_workers());
    let first = tokio::time::timeout(std::time::Duration::from_secs(2), capture_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        first
            .extension()
            .is_some_and(|extension| extension == "strm")
    );
    manager.apply_remote_reads_pause(true);

    let local_started = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if capture_rx.recv().await == Some(local_path.clone()) {
                break;
            }
        }
    })
    .await;
    drop(occupied);
    assert!(
        local_started.is_ok(),
        "local work after the first queued remote window must run while reads are paused"
    );
    let paused_remote = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let row = &remote_units[0].row.file;
            if store
                .active_probe_unit_for_ledger(&row.file_id.to_string())
                .unwrap()
                .is_some_and(|unit| unit.status == "queued")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(
        paused_remote.is_ok(),
        "remote fingerprint work stays queued"
    );

    manager.apply_remote_reads_pause(false);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while let Some(resumed_read) = capture_rx.recv().await {
            if resumed_read
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("strm"))
            {
                return;
            }
        }
        panic!("remote fingerprint work is dispatched again after resume");
    })
    .await
    .unwrap();
    manager.shutdown_workers().await.unwrap();
}
