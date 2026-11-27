#[path = "builtin_boost_cycle/http_commands.rs"]
mod http_commands;
#[path = "builtin_boost/support.rs"]
mod support;

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex as StdMutex};

use api::{ApiDownloadService, DownloadOutcome, DownloadSubmitter, router};
use axum::http::StatusCode;
use domain::Torrent;
use downloader::{Downloader, SubmitOptions, SubmitPayload};
use indexer::{IndexerError, RegistrySnapshot};
use serde_json::json;
use store::{BoostProtectionState, BoostTaskState};
use support::plugin_http_support::{HttpEnv, TOKEN, req_json};
use support::*;

struct StubBoostProvider(Arc<StdMutex<Vec<String>>>, indexer::ProviderDescriptor);

impl indexer::IndexerProvider for StubBoostProvider {
    fn descriptor(&self) -> &indexer::ProviderDescriptor {
        &self.1
    }
    fn search(
        &self,
        _: &indexer::ProviderCallContext<'_>,
        site: &domain::Site,
        _: &indexer::SearchQuery<'_>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, u)| Torrent {
                site_id: site.id,
                id: Some(format!("cand-{i}")),
                title: format!("Cand.{i}"),
                enclosure: u.clone(),
                size_bytes: Some(2048),
                seeders: Some(10),
                leechers: Some(1),
                snatched: None,
                upload_time: None,
                poster_url: None,
                free: true,
                hr: Some(false),
                imdb_id: None,
                detail_url: None,
                category: None,
                free_2x: false,
                free_until: None,
            })
            .collect())
    }
    fn rss(
        &self,
        _: &indexer::ProviderCallContext<'_>,
        _: &domain::Site,
        _: Option<&str>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        Ok(vec![])
    }
    fn resolve_download(
        &self,
        _: &indexer::ProviderCallContext<'_>,
        _: &domain::Site,
        _: &Torrent,
    ) -> Result<Option<indexer::ResolvedDownload>, IndexerError> {
        Ok(None)
    }
}

fn setup_multi_candidate_env(
    dir: &std::path::Path,
    srv: &StubTorrentServer,
) -> (
    HttpEnv,
    domain::SiteId,
    domain::DownloaderId,
    Arc<BoostTestDownloader>,
) {
    let dl = Arc::new(BoostTestDownloader::new(dir.join("stage")));
    let store = Arc::new(api::Store::open(dir.join("data")).unwrap());
    let mut reg = indexer::RegistryCandidate::from_builtin().unwrap();
    let urls = Arc::new(StdMutex::new(vec![
        srv.url("cand-1.torrent"),
        srv.url("cand-2.torrent"),
    ]));
    let desc = indexer::ProviderDescriptor {
        id: "boost_stub_idx".into(),
        name: "BoostStub".into(),
        kind: "builtin".into(),
        source: indexer::ProviderSource::Builtin,
        auth_types: vec![indexer::AuthType::Cookie],
        capabilities: vec![indexer::ProviderCapability::Search],
        availability: indexer::ProviderAvailability::Enabled,
        hidden_from_catalog: false,
    };
    reg = reg
        .add_indexer_provider(Arc::new(StubBoostProvider(urls, desc)))
        .unwrap();

    let state = api::ApiState::new_arc(
        store,
        TOKEN.into(),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl.clone(),
    )
    .unwrap()
    .with_plugin_http_policy_override(
        net_guard::TargetPolicy::production().with_test_loopback_socket(srv.addr()),
    );
    support::install_cm_boost_plugin(state.store().as_ref());
    state.indexer().publish(reg).unwrap();
    let site_id = domain::SiteId::new();
    state
        .store()
        .insert_site(&domain::Site {
            id: site_id,
            name: "PT-Idx".into(),
            url: "https://pt.example/".into(),
            profile_id: "boost_stub_idx".into(),
            cookie: None,
            api_key: None,
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: Some(60),
            cdp_url: None,
            downloader_id: None,
            enabled: true,
            maintenance_via: domain::MaintenanceVia::Http,
            browser_search: false,
            timezone: "UTC".into(),
        })
        .unwrap();
    let dl_id = domain::DownloaderId::new();
    state
        .store()
        .insert_downloader(&store::DownloaderRow {
            id: dl_id,
            name: "BoostDL".into(),
            kind: "qbittorrent".into(),
            url: "http://127.0.0.1:9".into(),
            username: None,
            password: None,
            api_key: None,
            category: None,
            path_maps: vec![],
            is_default: true,
            enabled: true,
        })
        .unwrap();
    let env = HttpEnv {
        _dir: tempfile::tempdir_in("/tmp").unwrap(),
        app: router(state.clone()),
        state,
        member_token: String::new(),
    };
    (env, site_id, dl_id, dl)
}

async fn run_boost_cycle_cmd(
    env: &HttpEnv,
    site_id: domain::SiteId,
    dl_id: domain::DownloaderId,
    dry_run: bool,
    idem: &str,
) -> serde_json::Value {
    let rev = env
        .state
        .store()
        .get_plugin(BOOST_PLUGIN_ID)
        .unwrap()
        .unwrap()
        .revision;
    let (st, _, b) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{BOOST_PLUGIN_ID}/commands"),
            Some(TOKEN),
            &[("If-Match", &format!("\"{rev}\"")), ("Idempotency-Key", idem)],
            Some(json!({
                "kind": "boost.run",
                "payload": { "site_id": site_id.to_string(), "downloader_id": dl_id.to_string(), "dry_run": dry_run }
            })),
        ))
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "body={b}");
    poll_op(env, b["data"]["operation_id"].as_str().unwrap()).await
}

#[tokio::test]
async fn repeated_boost_cycles_and_post_cleanup_readd_do_not_collide_or_leak_reserved() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let srv = StubTorrentServer::start_dynamic(2048);
    let (env, site_id, dl_id, dl) = setup_multi_candidate_env(dir.path(), &srv);
    save_boost_config(
        &env.state,
        json!({ "sites": [site_id.to_string()], "budget_bytes": 50_000, "dry_run": false }),
    );

    let op1 = run_boost_cycle_cmd(&env, site_id, dl_id, false, "run-1").await;
    assert_eq!(op1["state"], "succeeded", "op1={op1}");
    assert_eq!(op1["result"]["added"], 2, "op1={op1}");

    let op2 = run_boost_cycle_cmd(&env, site_id, dl_id, false, "run-2").await;
    assert_eq!(op2["state"], "succeeded");
    assert_eq!(op2["result"]["added"], 0);

    let snap = env
        .state
        .store()
        .get_boost_snapshot_for_plugin(BOOST_PLUGIN_ID, &[], &[])
        .unwrap();
    assert_eq!(snap.reserved_bytes, 0);
    assert_eq!(snap.tasks.len(), 2);

    // Clean one task (including removing from downloader just like downloader.delete-task)
    // and re-run boost cycle -> reactivates cleanly without UNIQUE error.
    let tf = &snap.tasks[0];
    dl.delete_task(&tf.info_hash, false).unwrap();
    let stage_file = dir.path().join("stage/readd.mkv");
    std::fs::create_dir_all(stage_file.parent().unwrap()).unwrap();
    std::fs::write(&stage_file, b"x").unwrap();
    let pjson = json!({ "save_path": stage_file.parent().unwrap(), "allowed_roots": [stage_file.parent().unwrap()], "paths": [stage_file] }).to_string();
    let ret = env
        .state
        .store()
        .mark_boost_task_retained_with_paths(
            &tf.task_fact_id,
            tf.revision,
            2048,
            Some(&pjson),
            api::job_loop::unix_now(),
        )
        .unwrap();
    let plan = env
        .state
        .store()
        .prepare_boost_cleanup_plan(&store::PrepareBoostCleanupPlanInput {
            task_fact_id: tf.task_fact_id.clone(),
            expected_revision: ret.revision,
            cleanup_plan_handle: "plan-readd".into(),
            cleanup_plan_json: pjson,
            ttl_secs: 60,
            now_utc: api::job_loop::unix_now(),
        })
        .unwrap();
    env.state
        .store()
        .complete_boost_cleanup("plan-readd", plan.revision, api::job_loop::unix_now())
        .unwrap();

    let op3 = run_boost_cycle_cmd(&env, site_id, dl_id, false, "run-3").await;
    assert_eq!(op3["state"], "succeeded");
    assert_eq!(op3["result"]["added"], 1);
    let snap3 = env
        .state
        .store()
        .get_boost_snapshot_for_plugin(BOOST_PLUGIN_ID, &[], &[])
        .unwrap();
    assert_eq!(snap3.reserved_bytes, 0);
    assert_eq!(snap3.tasks.len(), 2);
}

#[tokio::test]
async fn active_duplicate_info_hash_and_unknown_outcome_preserve_hold_and_hr_unknown() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(BoostTestDownloader::new(dir.path().join("stage")));
    let srv = StubTorrentServer::start(minimal_torrent_bytes("SameHash.2026", 4096));
    let (state, site_id, dl_id) = setup_state(dir.path(), dl.clone(), Some(&srv));
    save_boost_config(&state, json!({ "budget_bytes": 50_000, "hold_days": 5 }));

    let svc = ApiDownloadService::new(state.clone());
    let out1 = svc
        .submit(
            boost_plan(site_id, dl_id, &srv.url("a.torrent"), "SameA"),
            boost_ctx("dup-a"),
        )
        .await
        .unwrap();
    assert!(matches!(out1, DownloadOutcome::Submitted { .. }));

    // Different enclosure URL resolving to same info_hash must reject as duplicate without leaking reserved_bytes or double-charging adds.
    let out2 = svc
        .submit(
            boost_plan(site_id, dl_id, &srv.url("b.torrent"), "SameB"),
            boost_ctx("dup-b"),
        )
        .await
        .unwrap();
    assert!(
        matches!(
            out2,
            DownloadOutcome::Rejected { ref reason, .. }
                if reason.contains("active boost reservation")
        ),
        "expected duplicate active boost reservation rejection, got {out2:?}"
    );
    assert_eq!(
        state
            .store()
            .count_boost_adds_since(BOOST_PLUGIN_ID, 0)
            .unwrap(),
        1
    );
    let snap = state
        .store()
        .get_boost_snapshot_for_plugin(BOOST_PLUGIN_ID, &[], &[])
        .unwrap();
    assert_eq!(snap.reserved_bytes, 0);

    // UnknownOutcome reconciliation preserves hold_until & fail-closed hr_active: None.
    let srv2 = StubTorrentServer::start(minimal_torrent_bytes("UnknownHold.2026", 4096));
    let dir2 = tempfile::tempdir_in("/tmp").unwrap();
    let dl_unk = Arc::new(BoostTestDownloader::new(dir2.path().join("stage")));
    dl_unk.unknown_on_submit.store(true, Ordering::SeqCst);
    let (st_u, site_u, dl_u) = setup_state(dir2.path(), dl_unk.clone(), Some(&srv2));
    save_boost_config(&st_u, json!({ "budget_bytes": 50_000, "hold_days": 5 }));
    let unk = ApiDownloadService::new(st_u.clone())
        .submit(
            boost_plan(site_u, dl_u, &srv2.url("u.torrent"), "UnkHold"),
            boost_ctx("unk-hold"),
        )
        .await
        .unwrap();
    let DownloadOutcome::Unknown { .. } = unk else {
        panic!("expected Unknown")
    };

    let hash = downloader::parse_verified_torrent(&minimal_torrent_bytes("UnknownHold.2026", 4096))
        .unwrap()
        .info_hash;
    let op_id = st_u
        .store()
        .get_boost_snapshot_for_plugin(BOOST_PLUGIN_ID, &[], &[])
        .unwrap()
        .reservations[0]
        .operation_id
        .clone();
    dl_unk.unknown_on_submit.store(false, Ordering::SeqCst);
    dl_unk
        .submit_verified(
            &SubmitPayload::Magnet {
                uri: format!("magnet:?xt=urn:btih:{hash}"),
                info_hash: hash,
            },
            &SubmitOptions {
                ownership_tag: format!("cm-op:{op_id}"),
                save_path: None,
                category: None,
                extra_tags: vec![],
                paused: false,
                upload_limit: None,
                download_limit: None,
                sequential: false,
                first_last_piece: false,
            },
        )
        .unwrap();
    api::recover_plugin_startup_for_test(&st_u).unwrap();
    let tf = &st_u
        .store()
        .get_boost_snapshot_for_plugin(BOOST_PLUGIN_ID, &[], &[])
        .unwrap()
        .tasks[0];
    assert_eq!(tf.hr_active, None);
    assert!(tf.hold_until.is_some());
    assert_eq!(tf.protection_state, BoostProtectionState::Protected);
}

#[tokio::test]
async fn max_adds_per_hour_enforced_when_budget_bytes_is_none() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(BoostTestDownloader::new(dir.path().join("stage")));
    let srv = StubTorrentServer::start_dynamic(1024);
    let (state, site_id, dl_id) = setup_state(dir.path(), dl, Some(&srv));
    save_boost_config(&state, json!({ "max_adds_per_hour": 2 }));

    let svc = ApiDownloadService::new(state);
    for idx in 1..=3 {
        let out = svc
            .submit(
                boost_plan(
                    site_id,
                    dl_id,
                    &srv.url(&format!("hr-{idx}.torrent")),
                    &format!("Hr-{idx}"),
                ),
                boost_ctx(&format!("hr-{idx}")),
            )
            .await
            .unwrap();
        if idx <= 2 {
            assert!(
                matches!(out, DownloadOutcome::Submitted { .. }),
                "idx={idx} should submit"
            );
        } else {
            assert!(
                matches!(out, DownloadOutcome::Rejected { .. }),
                "idx={idx} must hit max_adds_per_hour"
            );
        }
    }
}

#[tokio::test]
async fn auto_cleanup_files_cleans_eligible_retained_while_respecting_protections_and_dry_run() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let stage = dir.path().join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    let dl = Arc::new(BoostTestDownloader::new(stage.clone()));
    let (state, site_id, dl_id) = setup_state(dir.path(), dl, None);
    let env = HttpEnv {
        _dir: dir,
        app: router(state.clone()),
        state,
        member_token: String::new(),
    };

    let f_ok = stage.join("AutoOk.mkv");
    let f_hr = stage.join("AutoHrUnknown.mkv");
    let f_lib = stage.join("AutoLib.mkv");
    for f in [&f_ok, &f_hr, &f_lib] {
        std::fs::write(f, b"data").unwrap();
    }
    let t_ok = seed_boost_task(
        env.state.store().as_ref(),
        dl_id,
        site_id,
        "auto-ok",
        "1111aaaa11111111111111111111111111111111",
        BoostTaskState::Seeding,
        100,
        0,
        Some(false),
        None,
    );
    let t_hr = seed_boost_task(
        env.state.store().as_ref(),
        dl_id,
        site_id,
        "auto-hr",
        "2222aaaa11111111111111111111111111111111",
        BoostTaskState::Seeding,
        100,
        0,
        None,
        None,
    );
    let t_lib = seed_boost_task(
        env.state.store().as_ref(),
        dl_id,
        site_id,
        "auto-lib",
        "3333aaaa11111111111111111111111111111111",
        BoostTaskState::Seeding,
        100,
        0,
        Some(false),
        None,
    );
    seed_library_file_entry(env.state.store().as_ref(), &f_lib);
    for (tid, f) in [(&t_ok, &f_ok), (&t_hr, &f_hr), (&t_lib, &f_lib)] {
        let pjson =
            json!({ "save_path": stage, "allowed_roots": [stage], "paths": [f] }).to_string();
        env.state
            .store()
            .mark_boost_task_retained_with_paths(tid, 1, 100, Some(&pjson), 1_700_000_010)
            .unwrap();
    }

    save_boost_config(
        &env.state,
        json!({ "sites": [site_id.to_string()], "auto_cleanup_files": true, "dry_run": true }),
    );
    run_boost_cycle_cmd(&env, site_id, dl_id, true, "auto-dry").await;
    assert!(f_ok.exists(), "dry_run=true must not delete files");

    save_boost_config(
        &env.state,
        json!({ "sites": [site_id.to_string()], "auto_cleanup_files": true, "dry_run": false }),
    );
    run_boost_cycle_cmd(&env, site_id, dl_id, false, "auto-live").await;
    assert!(!f_ok.exists(), "eligible retained file must be cleaned");
    assert!(f_hr.exists(), "hr_active=None must remain protected");
    assert!(
        f_lib.exists(),
        "library-referenced file must remain protected"
    );
    assert_eq!(
        env.state
            .store()
            .get_boost_task_fact(&t_ok)
            .unwrap()
            .unwrap()
            .state,
        BoostTaskState::Cleaned
    );
}

#[tokio::test]
async fn sequential_duplicate_active_info_hash_does_not_double_charge_budget_or_hourly_cap() {
    // Case 1: budget_bytes=6000, max_adds_per_hour=5, 4096B torrent -> dup must reject as duplicate_active_reservation.
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(BoostTestDownloader::new(dir.path().join("stage")));
    let srv_same = StubTorrentServer::start(minimal_torrent_bytes("DupBudget.2026", 4096));
    let (state, site_id, dl_id) = setup_state(dir.path(), dl.clone(), Some(&srv_same));
    save_boost_config(
        &state,
        json!({ "budget_bytes": 6000, "max_adds_per_hour": 5 }),
    );
    let svc = ApiDownloadService::new(state.clone());
    assert!(matches!(
        svc.submit(
            boost_plan(site_id, dl_id, &srv_same.url("a.torrent"), "A"),
            boost_ctx("c1-a")
        )
        .await
        .unwrap(),
        DownloadOutcome::Submitted { .. }
    ));
    let ctx_b = boost_ctx("c1-b");
    let out_b = svc
        .submit(
            boost_plan(site_id, dl_id, &srv_same.url("b.torrent"), "B"),
            ctx_b.clone(),
        )
        .await
        .unwrap();
    assert!(
        matches!(out_b, DownloadOutcome::Rejected { ref reason, .. } if reason == "active boost reservation already exists for info_hash"),
        "expected duplicate_active_reservation, got {out_b:?}"
    );
    let op_b = state
        .store()
        .get_download_operation(&ctx_b.operation_id.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(
        op_b.outcome_code.as_deref(),
        Some("duplicate_active_reservation")
    );
    let snap = state
        .store()
        .get_boost_snapshot_for_plugin(BOOST_PLUGIN_ID, &[], &[])
        .unwrap();
    assert_eq!(snap.used_bytes, 4096);
    assert_eq!(snap.reserved_bytes, 0);
    assert_eq!(
        state
            .store()
            .count_boost_adds_since(BOOST_PLUGIN_ID, 0)
            .unwrap(),
        1
    );

    // Case 2: budget_bytes=None, max_adds_per_hour=2 -> dup must not consume the 2nd hourly slot.
    let dir2 = tempfile::tempdir_in("/tmp").unwrap();
    let dl2 = Arc::new(BoostTestDownloader::new(dir2.path().join("stage")));
    let srv_h1 = StubTorrentServer::start(minimal_torrent_bytes("DupHourH1.2026", 1024));
    let srv_h2 = StubTorrentServer::start(minimal_torrent_bytes("DupHourH2.2026", 1024));
    let (st2, s2, dl_id2) = setup_state(dir2.path(), dl2, Some(&srv_h1));
    let st2 = st2.with_plugin_http_policy_override(
        net_guard::TargetPolicy::production()
            .with_test_loopback_socket(srv_h1.addr())
            .with_test_loopback_socket(srv_h2.addr()),
    );
    save_boost_config(&st2, json!({ "max_adds_per_hour": 2 }));
    let svc2 = ApiDownloadService::new(st2.clone());
    assert!(matches!(
        svc2.submit(
            boost_plan(s2, dl_id2, &srv_h1.url("1.torrent"), "H1a"),
            boost_ctx("h1a")
        )
        .await
        .unwrap(),
        DownloadOutcome::Submitted { .. }
    ));
    assert!(matches!(
        svc2.submit(
            boost_plan(s2, dl_id2, &srv_h1.url("2.torrent"), "H1b"),
            boost_ctx("h1b")
        )
        .await
        .unwrap(),
        DownloadOutcome::Rejected { .. }
    ));
    assert!(
        matches!(
            svc2.submit(
                boost_plan(s2, dl_id2, &srv_h2.url("3.torrent"), "H2"),
                boost_ctx("h2")
            )
            .await
            .unwrap(),
            DownloadOutcome::Submitted { .. }
        ),
        "distinct H2 must succeed when H1 duplicate did not consume hourly cap"
    );
}

#[tokio::test]
async fn boost_run_continues_past_active_duplicate_without_tripping_quota() {
    let dir3 = tempfile::tempdir_in("/tmp").unwrap();
    let srv3_dup = StubTorrentServer::start(minimal_torrent_bytes("CandSame.2026", 4096));
    let srv3_new = StubTorrentServer::start(minimal_torrent_bytes("CandNew.2026", 1024));
    let (mut env3, site3, dl3, _) = setup_multi_candidate_env(dir3.path(), &srv3_dup);
    env3.state = env3.state.with_plugin_http_policy_override(
        net_guard::TargetPolicy::production()
            .with_test_loopback_socket(srv3_dup.addr())
            .with_test_loopback_socket(srv3_new.addr()),
    );
    env3.app = router(env3.state.clone());
    save_boost_config(
        &env3.state,
        json!({ "sites": [site3.to_string()], "budget_bytes": 6000, "dry_run": false }),
    );
    let op_first = run_boost_cycle_cmd(&env3, site3, dl3, false, "c3-first").await;
    assert_eq!(op_first["result"]["added"], 1);
    let urls = Arc::new(StdMutex::new(vec![
        srv3_dup.url("dup-again.torrent"),
        srv3_new.url("new.torrent"),
    ]));
    let mut reg = indexer::RegistryCandidate::from_builtin().unwrap();
    let desc = indexer::ProviderDescriptor {
        id: "boost_stub_idx".into(),
        name: "BoostStub".into(),
        kind: "builtin".into(),
        source: indexer::ProviderSource::Builtin,
        auth_types: vec![indexer::AuthType::Cookie],
        capabilities: vec![indexer::ProviderCapability::Search],
        availability: indexer::ProviderAvailability::Enabled,
        hidden_from_catalog: false,
    };
    reg = reg
        .add_indexer_provider(Arc::new(StubBoostProvider(urls, desc)))
        .unwrap();
    env3.state.indexer().publish(reg).unwrap();
    let op_second = run_boost_cycle_cmd(&env3, site3, dl3, false, "c3-second").await;
    assert_eq!(op_second["state"], "succeeded");
    assert_eq!(op_second["result"]["added"], 1, "op_second={op_second}");
    assert_eq!(
        op_second["result"]["skipped_quota"], false,
        "op_second={op_second}"
    );
}
