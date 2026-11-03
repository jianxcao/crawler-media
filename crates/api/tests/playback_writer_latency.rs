//! In-process HTTP plus actual public scan/publication: writer lifetimes and durable heartbeats.
#[path = "playback_writer_latency/fixture.rs"]
mod fixture;
#[path = "support/mod.rs"]
mod support;
#[path = "playback_writer_latency/workload.rs"]
mod workload;
use fixture::*;
use serde_json::json;
use std::{
    sync::{Arc, atomic::Ordering},
    time::Instant,
};
use support::library_presence::PresenceFixture;
use tower::ServiceExt;
use workload::*;

#[test]
fn native_playback_persists_during_scan_and_transfer_publication() {
    measure(false);
}
#[test]
fn protocol_playback_persists_during_scan_and_transfer_publication() {
    measure(true);
}
fn measure(protocol: bool) {
    let env = Env::new();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(exercise(&env, protocol));
}
async fn exercise(env: &Env, protocol: bool) {
    let store = env.fixture.state.store();
    let session = support::session::issue_session(
        &store,
        env.user,
        "l1-player",
        env.playing.file_id,
        BASE / 1000,
    );
    store
        .mutate_playback(
            env.media,
            &store::playback::PlaybackMutation::marks(env.user, -1, -1, Some(true), Some(true)),
            BASE,
        )
        .unwrap();
    env.event(protocol, &session, "start", 1, 0).await;
    let mut sequence = 1;
    let mut position = 0;
    for (phase, (scan, dispersed)) in [(true, true), (true, false), (false, true), (false, false)]
        .into_iter()
        .enumerate()
    {
        run_phase(
            env,
            protocol,
            &session,
            (phase, scan, dispersed),
            &mut sequence,
            &mut position,
        )
        .await;
    }
    finish(env, protocol, &session, sequence, position).await;
}
async fn run_phase(
    env: &Env,
    protocol: bool,
    session: &str,
    config: (usize, bool, bool),
    sequence: &mut i64,
    position: &mut i64,
) {
    let (phase, scan, dispersed) = config;
    let store = env.fixture.state.store();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let (thread_store, templates, root, scope, start) = (
        store.clone(),
        env.templates.clone(),
        env.root_path.clone(),
        env.scope.clone(),
        barrier.clone(),
    );
    let worker = std::thread::spawn(move || {
        run(
            thread_store,
            templates,
            root,
            scope,
            (scan, dispersed, phase),
            start,
        )
    });
    barrier.wait();
    let mut latencies = Vec::new();
    let mut waits = Vec::new();
    let mut holds = Vec::new();
    let mut overlapping = 0;
    while !worker.is_finished() || latencies.len() < 20 {
        if !worker.is_finished() {
            overlapping += 1;
        }
        *sequence += 1;
        *position += 2000;
        let now = BASE + *position;
        env.clock.0.store(now, Ordering::SeqCst);
        let (_, ms, metrics) = env
            .event(protocol, session, "progress", *sequence, *position)
            .await;
        latencies.push(ms);
        if let (Some(wait), Some(hold)) = (metrics.writer_wait_ns, metrics.writer_hold_ns) {
            waits.push(wait as f64 / 1e6);
            holds.push(hold as f64 / 1e6);
        } else {
            assert!(protocol, "native writer metrics must be observed");
        }
        env.assert_progress(session, *sequence, *position, now);
        assert!(latencies.len() < 5000, "background workload must finish");
    }
    let report = worker.join().unwrap();
    let label = format!("protocol={protocol} scan={scan} dispersed={dispersed}");
    summarize(&report, &label);
    distribution(&format!("{label} HTTP"), latencies);
    if waits.is_empty() {
        eprintln!(
            "L1 {label} heartbeat_writer_wait/hold=unavailable; protocol provider does not propagate request metrics"
        );
    } else {
        distribution(&format!("{label} heartbeat_writer_wait"), waits);
        distribution(&format!("{label} heartbeat_writer_hold"), holds);
    }
    eprintln!(
        "L1 {label} overlapping_requests={overlapping} 503=0 durable_verified_every_sequence=true"
    );
    assert!(overlapping > 0);
    assert_eq!(report.samples.len(), 20);
    assert!(
        report
            .samples
            .iter()
            .all(|s| s.outcome == "ok" && s.covered_owned_write_tx_count == 1)
    );
    assert!(
        report.samples.iter().all(
            |s| s.writer_hold_ns < 1_000_000_000 && s.covered_owned_write_tx_ns < 1_000_000_000
        ),
        "actual background writer exceeded1s budget"
    );
}
async fn finish(env: &Env, protocol: bool, session: &str, sequence: i64, position: i64) {
    env.clock.0.store(BASE + position + 2000, Ordering::SeqCst);
    env.event(protocol, session, "stop", sequence + 1, position + 2000)
        .await;
    let reopened = store::Store::open(env.fixture.dir.path().join("data")).unwrap();
    let unit = reopened
        .unit_state(env.user, env.media, -1, -1)
        .unwrap()
        .unwrap();
    assert_eq!(unit.position_ms, position + 2000);
    assert!(unit.favorite);
    assert!(unit.played);
    assert_eq!(unit.play_count, 0);
    assert_eq!(
        reopened.playback_progress(env.user, env.media).unwrap(),
        Some(position + 2000)
    );
    let logs = reopened
        .list_logs(
            10,
            None,
            None,
            None,
            (BASE + position + 2000) / 1000,
            Some(env.user),
        )
        .unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].end_position_ms, position + 2000);
    assert_eq!(logs[0].watched_ms, position + 2000);
    eprintln!(
        "L1 protocol={protocol} reopened position={} marks=true/true play_count=0 logs=1 watch_ms={}",
        unit.position_ms, logs[0].watched_ms
    );
}
