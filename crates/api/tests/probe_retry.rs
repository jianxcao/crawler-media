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
async fn queries_cannot_bypass_outro_retry_deadline() {
    let mut s = ProbeScenario::cached_season();
    s.set_time_ms(1_000_000);
    s.missing_outro(8);
    s.fail_next_outro(8, "HTTP error 403 Forbidden");
    s.request(8, ProbeRequestOrigin::Detail);
    s.drain().await;
    for _ in 0..20 {
        assert_eq!(
            s.request(8, ProbeRequestOrigin::Detail),
            ProbeRequestResult::Waiting {
                next_retry_at_ms: 1_060_000
            }
        );
    }
    s.reopen();
    s.set_time_ms(1_059_999);
    assert_eq!(s.dispatch_due(), 0);
    s.set_time_ms(1_060_000);
    assert_eq!(s.dispatch_due(), 1);
    s.drain().await;
    assert_eq!(s.metadata_reads(8), 0);
    assert_eq!(s.intro_reads(8), 0);
    assert_eq!(s.outro_reads(8), 2);
}

#[tokio::test]
async fn fifth_failure_exhausts_automatic_retries() {
    let mut s = ProbeScenario::cached_season();
    s.set_time_ms(1_000_000);
    s.missing_outro(8);
    s.fail_next_outro(8, "HTTP error 403 Forbidden");
    s.request(8, ProbeRequestOrigin::Detail);
    s.drain().await;
    let deadlines = [1_060_000, 1_360_000, 2_260_000, 5_860_000];
    for deadline in deadlines {
        s.set_time_ms(deadline);
        assert_eq!(s.dispatch_due(), 1);
        s.drain().await;
    }
    s.set_time_ms(i64::MAX / 4);
    assert_eq!(s.dispatch_due(), 0);
    assert_eq!(
        s.request(8, ProbeRequestOrigin::Detail),
        ProbeRequestResult::Exhausted
    );
    assert_eq!(s.outro_reads(8), 5);
}

#[tokio::test]
async fn repeated_outro_failure_does_not_recompare_season() {
    let mut s = ProbeScenario::cached_season();
    s.set_time_ms(1_000_000);
    s.missing_outro(8);
    s.fail_next_outro(8, "HTTP error 403 Forbidden");
    s.request(8, ProbeRequestOrigin::Detail);
    s.drain().await;
    let after_first = s.comparison_runs();
    s.set_time_ms(1_060_000);
    assert_eq!(s.dispatch_due(), 1);
    s.drain().await;
    assert_eq!(s.comparison_runs(), after_first);
}

#[tokio::test]
async fn disabled_master_switch_blocks_fingerprint() {
    let mut s = ProbeScenario::cached_season();
    s.set_intro_settings(false, true);
    s.missing_outro(8);
    assert_eq!(
        s.request(8, ProbeRequestOrigin::Detail),
        ProbeRequestResult::Disabled
    );
    assert_eq!(s.dispatch_due(), 0);
    assert_eq!(s.outro_reads(8), 0);
}

#[tokio::test]
async fn delete_restore_retries_only_missing_episode_stage() {
    let mut s = ProbeScenario::cached_season();
    s.set_time_ms(1_000_000);
    s.missing_outro(8);
    s.fail_next_outro(8, "HTTP error 403 Forbidden");
    s.request(8, ProbeRequestOrigin::Detail);
    s.drain().await;
    for episode in 1..=7 {
        assert_eq!(s.metadata_reads(episode), 0);
        assert_eq!(s.intro_reads(episode), 0);
        assert_eq!(s.outro_reads(episode), 0);
    }
    assert_eq!(s.metadata_reads(8), 0);
    assert_eq!(s.intro_reads(8), 0);
    assert_eq!(s.outro_reads(8), 1);
    assert_eq!(s.fingerprint_job_status(8), "partial");
    for _ in 0..20 {
        assert!(matches!(
            s.request(8, ProbeRequestOrigin::Detail),
            ProbeRequestResult::Waiting { .. }
        ));
    }
    s.reopen();
    s.set_time_ms(1_060_000);
    *s.engine.outro_error.lock() = None;
    assert_eq!(s.dispatch_due(), 1);
    s.drain().await;
    assert_eq!(s.outro_reads(8), 2);
    assert_eq!(s.fingerprint_job_status(8), "succeeded");
}

#[tokio::test]
async fn cached_complete_episode_creates_no_job() {
    let s = ProbeScenario::cached_season();
    assert_eq!(
        s.request(1, ProbeRequestOrigin::Detail),
        ProbeRequestResult::Complete
    );
}
