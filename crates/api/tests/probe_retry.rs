#[path = "probe_retry_support.rs"]
mod support;

use support::ProbeScenario;
use api::probe_manager::policy::{ProbeRequestOrigin, ProbeRequestResult};

#[tokio::test]
async fn missing_outro_skips_metadata_and_preserves_intro() {
    let mut s = ProbeScenario::cached_season();
    s.missing_outro(8);
    s.fail_next_outro(8, "HTTP error 403 Forbidden");
    assert!(matches!(
        s.request(8, ProbeRequestOrigin::Detail),
        ProbeRequestResult::Queued { .. }
    ));
    s.drain().await;
    assert_eq!(s.metadata_reads(8), 0);
    assert_eq!(s.intro_reads(8), 0);
    assert_eq!(s.outro_reads(8), 1);
    assert_eq!(s.fingerprint_job_status(8), "partial");
}

#[tokio::test]
async fn cached_complete_episode_creates_no_job() {
    let s = ProbeScenario::cached_season();
    assert_eq!(
        s.request(1, ProbeRequestOrigin::Detail),
        ProbeRequestResult::Complete
    );
}
