#[path = "support/mod.rs"]
mod support;

use axum::http::StatusCode;
use serde_json::json;
use std::{path::Path, sync::Arc, time::Duration};
use store::test_support::{FixtureFile, publish_fixture_file};
use support::library_presence::PresenceFixture;

struct FixtureTracks {
    fail: bool,
}
impl api::probe_manager::TracksProbe for FixtureTracks {
    fn probe(
        &self,
        _: &Path,
        _: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        if self.fail {
            return Err(library::LibraryError::Probe("fixture probe failure".into()));
        }
        Ok((
            library::Tracks {
                video: Some(library::VideoTrack::default()),
                ..Default::default()
            },
            Some(120_000),
        ))
    }
}

fn fixture(fail_probe: bool) -> PresenceFixture {
    // Install fake IO before any background worker can probe the scanned file.
    let mut fix = std::thread::spawn(PresenceFixture::new).join().unwrap();
    fix.state.probe = Arc::new(
        api::probe_manager::ProbeManager::new(fix.state.store())
            .with_tracks_probe(Arc::new(FixtureTracks { fail: fail_probe })),
    );
    fix
}

#[tokio::test]
async fn full_lifecycle_playback_and_scan_correctness_acceptance() {
    let fix = fixture(false);
    let row = scan_published_movie(&fix).await;
    let store_arc = fix.state.store();
    let job = store_arc
        .lock()
        .latest_probe_job_for_scope(&format!("ledger:{}", row.file.file_id))
        .unwrap()
        .expect("scan admission must persist probe work");
    assert!(fix.state.probe.try_start_workers());
    wait_for_probe(&fix, &job.id, "succeeded").await;

    let user_id = store_arc
        .lock()
        .user_by_token(&fix.admin_token)
        .unwrap()
        .unwrap()
        .id;
    let session_id_1 = support::session::issue_session(
        &store_arc.lock(),
        user_id,
        "device_acc",
        row.file.file_id,
        1000,
    );
    heartbeat(&fix, &row, &session_id_1, "start", 0).await;
    heartbeat(&fix, &row, &session_id_1, "progress", 5000).await;

    let session_id_2 = support::session::issue_session(
        &store_arc.lock(),
        user_id,
        "device_acc",
        row.file.file_id,
        2000,
    );
    heartbeat(&fix, &row, &session_id_2, "start", 0).await;
    heartbeat(&fix, &row, &session_id_1, "stop", 5000).await;

    let cur = store_arc
        .lock()
        .get_session(user_id, "device_acc")
        .unwrap()
        .unwrap();
    assert_eq!(cur.play_session_id.as_deref(), Some(session_id_2.as_str()));

    // The successful probe published reusable metadata, so dispatch can prove
    // the obligation complete even after its worker has finished.
    assert_eq!(
        api::scan_probe_dispatch::dispatch_once(fix.state.clone())
            .await
            .unwrap(),
        1
    );
    assert!(
        store_arc
            .lock()
            .scan_probe_obligations(10)
            .unwrap()
            .is_empty()
    );
    let latest = store_arc
        .lock()
        .latest_probe_job_for_scope(&format!("ledger:{}", row.file.file_id))
        .unwrap()
        .unwrap();
    assert_eq!(
        latest.id, job.id,
        "dispatch must reuse the successful probe"
    );
    fix.state.probe.shutdown_workers().await.unwrap();
}

#[tokio::test]
async fn failed_scan_probe_preserves_obligation_until_restart_retry_is_accepted() {
    let mut fix = fixture(true);
    let row = scan_published_movie(&fix).await;
    let store_arc = fix.state.store();
    let scope = format!("ledger:{}", row.file.file_id);
    let failed = store_arc
        .lock()
        .latest_probe_job_for_scope(&scope)
        .unwrap()
        .unwrap();
    assert!(fix.state.probe.try_start_workers());
    wait_for_probe(&fix, &failed.id, "failed").await;

    assert_eq!(
        api::scan_probe_dispatch::dispatch_once(fix.state.clone())
            .await
            .unwrap(),
        0
    );
    let obligations = store_arc.lock().scan_probe_obligations(10).unwrap();
    assert_eq!(obligations.len(), 1, "failed work must remain retryable");
    assert_eq!(obligations[0].ledger_id, row.file.file_id);
    assert_eq!(
        store_arc
            .lock()
            .latest_probe_job_for_scope(&scope)
            .unwrap()
            .unwrap()
            .id,
        failed.id
    );
    fix.state.probe.shutdown_workers().await.unwrap();

    // Restart drops the in-memory failure cooldown. The durable obligation
    // must still arrange a new job, which uses fake successful stream IO.
    fix.state.probe = Arc::new(
        api::probe_manager::ProbeManager::new(store_arc.clone())
            .with_tracks_probe(Arc::new(FixtureTracks { fail: false })),
    );
    assert_eq!(
        api::scan_probe_dispatch::dispatch_once(fix.state.clone())
            .await
            .unwrap(),
        1
    );
    assert!(
        store_arc
            .lock()
            .scan_probe_obligations(10)
            .unwrap()
            .is_empty()
    );
    let retry = store_arc
        .lock()
        .latest_probe_job_for_scope(&scope)
        .unwrap()
        .unwrap();
    assert_ne!(retry.id, failed.id);
    assert!(fix.state.probe.try_start_workers());
    wait_for_probe(&fix, &retry.id, "succeeded").await;
    assert_eq!(
        store_arc
            .lock()
            .get_probe_job(&failed.id)
            .unwrap()
            .unwrap()
            .status,
        "failed"
    );
    fix.state.probe.shutdown_workers().await.unwrap();
}

async fn wait_for_probe(fix: &PresenceFixture, job_id: &str, expected: &str) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let job = fix
                .state
                .store()
                .lock()
                .get_probe_job(job_id)
                .unwrap()
                .unwrap();
            if !job.is_active() {
                assert_eq!(job.status, expected, "{job:?}");
                let units = fix.state.store().lock().probe_job_units(job_id).unwrap();
                assert_eq!(units.len(), 1);
                assert_eq!(units[0].status, expected);
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("fake probe must finish without external IO");
}

async fn heartbeat(
    fix: &PresenceFixture,
    row: &domain::FileCoverage,
    session: &str,
    event: &str,
    position_ms: i64,
) {
    let (status, response) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "event_sequence": support::session::next_sequence(),
                "device_id": "device_acc",
                "media_item_id": row.file.media_id.to_string(),
                "file_id": row.file.file_id.to_string(),
                "event": event,
                "position_ms": position_ms,
                "play_session_id": session,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{event}: {response}");
}

async fn scan_published_movie(fix: &PresenceFixture) -> domain::FileCoverage {
    let movie_file = fix.root.join("correctness_movie (2024).mkv");
    std::fs::write(&movie_file, b"content").unwrap();
    let (status, res) = fix.scan_and_wait(&fix.library_id).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["inserted"], 1);

    let store_arc = fix.state.store();
    let row = store_arc
        .lock()
        .file_coverage_by_path(&movie_file.display().to_string())
        .unwrap()
        .unwrap();
    // Movie scans register file facts; publish WHOLE coverage for session issuance.
    publish_fixture_file(
        &store_arc.lock(),
        &FixtureFile {
            media_id: row.file.media_id,
            path: row.file.path.display().to_string(),
            resolution: row.file.resolution.clone(),
            codec: row.file.codec.clone(),
            hdr: row.file.hdr.clone(),
            quality_source: row.file.quality_source,
            confidence: row.file.confidence,
            filter_score: row.file.filter_score,
            ..FixtureFile::default()
        },
    )
    .unwrap();
    row
}
