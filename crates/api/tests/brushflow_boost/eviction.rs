use downloader::MemoryTaskMetrics;
use serde_json::json;
use store::{BoostProtectionState, BoostTaskState};

use super::support::{
    BRUSH_PLUGIN_ID, BrushEnv, SiteFeedData, make_torrent, save_plugin_cfg, seed_library_path,
    seed_task_with_live, seed_transfer_active_hash,
};

#[tokio::test]
async fn budget_full_stop_vs_dynamic_eviction_order_and_protections() {
    let env = BrushEnv::new(4096);
    let now = api::job_loop::unix_now();
    let cand = make_torrent(
        env.site_search,
        &env.srv.url("new-evict.torrent"),
        "EvictReplacement.S01E01",
    );
    env.search_provider.feeds.lock().unwrap().insert(
        env.site_search,
        SiteFeedData {
            search_hits: vec![cand],
            ..Default::default()
        },
    );

    let (tid_slow_new, h_slow_new) = seed_task_with_live(
        &env,
        env.site_search,
        "ev-slow-new",
        "SlowNew",
        4096,
        BoostTaskState::Seeding,
        Some(false),
        None,
        None,
        BoostProtectionState::Eligible,
        None,
        MemoryTaskMetrics {
            progress: 1.0,
            upload_speed: 10,
            added_at: Some(now - 100),
            ..Default::default()
        },
    );
    let (tid_slow_old, h_slow_old) = seed_task_with_live(
        &env,
        env.site_search,
        "ev-slow-old",
        "SlowOld",
        4096,
        BoostTaskState::Seeding,
        Some(false),
        Some(now - 60),
        None,
        BoostProtectionState::Protected,
        Some("hold-active"),
        MemoryTaskMetrics {
            progress: 1.0,
            upload_speed: 10,
            added_at: Some(now - 900),
            ..Default::default()
        },
    );
    let stage = env.stage_dir();
    let f_new = stage.join("SlowNew.mkv");
    let f_old = stage.join("SlowOld.mkv");
    std::fs::write(&f_new, vec![1u8; 4096]).unwrap();
    std::fs::write(&f_old, vec![2u8; 4096]).unwrap();
    env.dl
        .set_task_files(&h_slow_new, vec![f_new.clone()], 4096);
    env.dl
        .set_task_files(&h_slow_old, vec![f_old.clone()], 4096);

    save_plugin_cfg(
        &env.http.state,
        BRUSH_PLUGIN_ID,
        json!({
            "sites": [env.site_search.to_string()],
            "downloader_id": env.dl_id.to_string(),
            "budget_bytes": 8192,
            "watermark_low_bytes": 4096,
            "dynamic_delete": false,
            "dry_run": false,
        }),
    );
    let r_stop = env.run_cycle(json!({}), "dyn-off").await;
    assert_eq!(r_stop["skipped_quota"], true, "r_stop={r_stop}");
    assert_eq!(r_stop["evicted_tasks"], 0);
    assert_eq!(r_stop["added"], 0);
    assert!(f_old.exists());

    let r_evict = env
        .run_cycle(json!({ "dynamic_delete": true }), "dyn-on")
        .await;
    assert_eq!(r_evict["evicted_tasks"], 1, "r_evict={r_evict}");
    assert_eq!(r_evict["cleaned_files_tasks"], 1, "r_evict={r_evict}");
    assert_eq!(
        r_evict["added"], 1,
        "must admit new cand same cycle: {r_evict}"
    );
    assert!(
        !f_old.exists(),
        "oldest slowest eligible file must be deleted"
    );
    assert!(f_new.exists(), "newer task must be spared");
    assert_eq!(
        env.http
            .state
            .store()
            .get_boost_task_fact(&tid_slow_old)
            .unwrap()
            .unwrap()
            .state,
        BoostTaskState::Cleaned
    );
    assert_eq!(
        env.http
            .state
            .store()
            .get_boost_task_fact(&tid_slow_new)
            .unwrap()
            .unwrap()
            .state,
        BoostTaskState::Seeding
    );
}

fn seed_protected_eviction_cases(env: &BrushEnv, now: i64, files: &[std::path::PathBuf; 5]) {
    let m_seed = MemoryTaskMetrics {
        progress: 1.0,
        added_at: Some(now - 500),
        ..Default::default()
    };
    let cases = [
        (
            "p-hr",
            "ProtHr",
            BoostTaskState::Seeding,
            Some(true),
            None,
            BoostProtectionState::Protected,
            Some("hr-active"),
            m_seed.clone(),
            0usize,
        ),
        (
            "p-hold",
            "ProtHold",
            BoostTaskState::Seeding,
            Some(false),
            Some(now + 86_400),
            BoostProtectionState::Protected,
            Some("hold-active"),
            m_seed.clone(),
            1,
        ),
        (
            "p-lib",
            "ProtLib",
            BoostTaskState::Seeding,
            Some(false),
            None,
            BoostProtectionState::Eligible,
            None,
            m_seed.clone(),
            2,
        ),
        (
            "p-transfer",
            "ProtTransfer",
            BoostTaskState::Seeding,
            Some(false),
            None,
            BoostProtectionState::Eligible,
            None,
            m_seed.clone(),
            3,
        ),
        (
            "p-sib-a",
            "ProtSibA",
            BoostTaskState::Seeding,
            Some(false),
            None,
            BoostProtectionState::Eligible,
            None,
            m_seed,
            4,
        ),
        (
            "p-sib-b",
            "ProtSibB",
            BoostTaskState::Downloading,
            Some(false),
            None,
            BoostProtectionState::Eligible,
            None,
            MemoryTaskMetrics {
                progress: 0.5,
                added_at: Some(now - 100),
                ..Default::default()
            },
            4,
        ),
    ];
    for (tag, name, st, hr, hold, prot, reason, metrics, file_idx) in cases {
        let (_, hash) = seed_task_with_live(
            env,
            env.site_search,
            tag,
            name,
            2048,
            st,
            hr,
            hold,
            None,
            prot,
            reason,
            metrics,
        );
        env.dl
            .set_task_files(&hash, vec![files[file_idx].clone()], 2048);
        if tag == "p-transfer" {
            seed_transfer_active_hash(env.http.state.store().as_ref(), &hash);
        }
    }
}

#[tokio::test]
async fn dynamic_eviction_protects_hr_hold_library_transfer_and_siblings() {
    let env = BrushEnv::new(4096);
    let now = api::job_loop::unix_now();
    let cand = make_torrent(
        env.site_search,
        &env.srv.url("prot-cand.torrent"),
        "ProtectedBlocked.S01E01",
    );
    env.search_provider.feeds.lock().unwrap().insert(
        env.site_search,
        SiteFeedData {
            search_hits: vec![cand],
            ..Default::default()
        },
    );

    let stage = env.stage_dir();
    let files = [
        stage.join("ProtHr.mkv"),
        stage.join("ProtHold.mkv"),
        stage.join("ProtLib.mkv"),
        stage.join("ProtTransfer.mkv"),
        stage.join("ProtShared.mkv"),
    ];
    for f in &files {
        std::fs::write(f, vec![9u8; 2048]).unwrap();
    }
    seed_library_path(env.http.state.store().as_ref(), &files[2]);
    seed_protected_eviction_cases(&env, now, &files);

    save_plugin_cfg(
        &env.http.state,
        BRUSH_PLUGIN_ID,
        json!({
            "sites": [env.site_search.to_string()],
            "downloader_id": env.dl_id.to_string(),
            "budget_bytes": 12_288,
            "watermark_low_bytes": 2048,
            "dynamic_delete": true,
            "dry_run": false,
        }),
    );
    let r = env.run_cycle(json!({}), "prot-evict").await;
    assert_eq!(r["evicted_tasks"], 0, "all tasks protected: {r}");
    assert_eq!(r["cleaned_files_tasks"], 0);
    assert_eq!(r["skipped_quota"], true);
    assert_eq!(r["added"], 0);
    for f in &files {
        assert!(f.exists(), "protected file must stay on disk: {f:?}");
    }
}

#[tokio::test]
async fn max_downloading_and_global_speed_gates_block_new_adds() {
    let env = BrushEnv::new(4096);
    let cand = make_torrent(env.site_search, &env.srv.url("gate.torrent"), "Gate.S01E01");
    env.search_provider.feeds.lock().unwrap().insert(
        env.site_search,
        SiteFeedData {
            search_hits: vec![cand],
            ..Default::default()
        },
    );
    seed_task_with_live(
        &env,
        env.site_search,
        "gate-dl",
        "GateDl",
        2048,
        BoostTaskState::Downloading,
        Some(false),
        None,
        None,
        BoostProtectionState::Eligible,
        None,
        MemoryTaskMetrics {
            progress: 0.3,
            upload_speed: 1500,
            download_speed: 2500,
            ..Default::default()
        },
    );

    save_plugin_cfg(
        &env.http.state,
        BRUSH_PLUGIN_ID,
        json!({
            "sites": [env.site_search.to_string()],
            "downloader_id": env.dl_id.to_string(),
            "dry_run": false,
        }),
    );
    let r_dl = env
        .run_cycle(json!({ "max_downloading": 1 }), "gate-max-dl")
        .await;
    assert_eq!(r_dl["added"], 0, "max_downloading=1 must block: {r_dl}");

    let r_up = env
        .run_cycle(json!({ "global_max_upspeed_bytes": 1000 }), "gate-up")
        .await;
    assert_eq!(
        r_up["added"], 0,
        "global_max_upspeed_bytes must block: {r_up}"
    );

    let r_dn = env
        .run_cycle(json!({ "global_max_dlspeed_bytes": 2000 }), "gate-dn")
        .await;
    assert_eq!(
        r_dn["added"], 0,
        "global_max_dlspeed_bytes must block: {r_dn}"
    );
}
