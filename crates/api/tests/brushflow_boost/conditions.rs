use downloader::MemoryTaskMetrics;
use serde_json::json;
use store::{BoostProtectionState, BoostTaskState};

use super::support::{
    BRUSH_PLUGIN_ID, BrushEnv, SiteFeedData, make_torrent, save_plugin_cfg, seed_task_with_live,
};

#[tokio::test]
async fn delete_on_free_expired_aborts_incomplete_downloading_and_spares_seeding() {
    let env = BrushEnv::new(4096);
    let now = api::job_loop::unix_now();

    let (tid_exp, _) = seed_task_with_live(
        &env,
        env.site_search,
        "fe-exp",
        "FreeExpDl",
        4096,
        BoostTaskState::Downloading,
        Some(false),
        None,
        Some(now + 120),
        BoostProtectionState::Eligible,
        None,
        MemoryTaskMetrics {
            progress: 0.4,
            ..Default::default()
        },
    );
    let (tid_seed, _) = seed_task_with_live(
        &env,
        env.site_search,
        "fe-seed",
        "FreeExpSeed",
        4096,
        BoostTaskState::Seeding,
        Some(false),
        None,
        Some(now + 120),
        BoostProtectionState::Eligible,
        None,
        MemoryTaskMetrics {
            progress: 1.0,
            ..Default::default()
        },
    );
    let (tid_far, _) = seed_task_with_live(
        &env,
        env.site_search,
        "fe-far",
        "FreeFarDl",
        4096,
        BoostTaskState::Downloading,
        Some(false),
        None,
        Some(now + 3600),
        BoostProtectionState::Eligible,
        None,
        MemoryTaskMetrics {
            progress: 0.4,
            ..Default::default()
        },
    );

    save_plugin_cfg(
        &env.http.state,
        BRUSH_PLUGIN_ID,
        json!({
            "sites": [env.site_search.to_string()],
            "downloader_id": env.dl_id.to_string(),
            "delete_on_free_expired": true,
            "dry_run": false,
        }),
    );
    let res = env.run_cycle(json!({}), "free-exp").await;
    assert_eq!(res["evicted_tasks"], 1, "res={res}");
    assert_eq!(
        env.http
            .state
            .store()
            .get_boost_task_fact(&tid_exp)
            .unwrap()
            .unwrap()
            .state,
        BoostTaskState::RetainedOnly
    );
    assert_eq!(
        env.http
            .state
            .store()
            .get_boost_task_fact(&tid_seed)
            .unwrap()
            .unwrap()
            .state,
        BoostTaskState::Seeding
    );
    assert_eq!(
        env.http
            .state
            .store()
            .get_boost_task_fact(&tid_far)
            .unwrap()
            .unwrap()
            .state,
        BoostTaskState::Downloading
    );
}

fn build_condition_cases(
    now: i64,
) -> Vec<(
    &'static str,
    &'static str,
    i64,
    BoostTaskState,
    MemoryTaskMetrics,
)> {
    vec![
        (
            "c-sh",
            "CondSeedHours",
            1000,
            BoostTaskState::Seeding,
            MemoryTaskMetrics {
                progress: 1.0,
                seeding_secs: 7200,
                upload_speed: 500,
                last_activity_at: Some(now),
                ..Default::default()
            },
        ),
        (
            "c-rt",
            "CondRatio",
            1000,
            BoostTaskState::Seeding,
            MemoryTaskMetrics {
                progress: 1.0,
                uploaded_bytes: 2500,
                upload_speed: 500,
                last_activity_at: Some(now),
                ..Default::default()
            },
        ),
        (
            "c-ub",
            "CondUpBytes",
            10_000,
            BoostTaskState::Seeding,
            MemoryTaskMetrics {
                progress: 1.0,
                uploaded_bytes: 5000,
                upload_speed: 500,
                last_activity_at: Some(now),
                ..Default::default()
            },
        ),
        (
            "c-dh",
            "CondDlHours",
            10_000,
            BoostTaskState::Downloading,
            MemoryTaskMetrics {
                progress: 0.2,
                added_at: Some(now - 7200),
                uploaded_bytes: 1_000_000,
                upload_speed: 500,
                last_activity_at: Some(now),
                ..Default::default()
            },
        ),
        (
            "c-au",
            "CondAvgUp",
            10_000,
            BoostTaskState::Seeding,
            MemoryTaskMetrics {
                progress: 1.0,
                added_at: Some(now - 1000),
                uploaded_bytes: 500,
                upload_speed: 500,
                last_activity_at: Some(now),
                ..Default::default()
            },
        ),
        (
            "c-in",
            "CondInact",
            10_000,
            BoostTaskState::Seeding,
            MemoryTaskMetrics {
                progress: 1.0,
                added_at: Some(now - 1000),
                uploaded_bytes: 1_000_000,
                last_activity_at: Some(now - 3600),
                ..Default::default()
            },
        ),
    ]
}

#[tokio::test]
async fn condition_deletes_cover_seed_ratio_uploaded_dl_hours_upspeed_and_inactive() {
    let env = BrushEnv::new(4096);
    let now = api::job_loop::unix_now();

    let mut targets = Vec::new();
    for (tag, name, size, st, metrics) in build_condition_cases(now) {
        let (tid, _) = seed_task_with_live(
            &env,
            env.site_search,
            tag,
            name,
            size,
            st,
            Some(false),
            None,
            None,
            BoostProtectionState::Eligible,
            None,
            metrics,
        );
        targets.push(tid);
    }
    let (tid_spare, _) = seed_task_with_live(
        &env,
        env.site_search,
        "c-sp",
        "CondSpare",
        10_000,
        BoostTaskState::Seeding,
        Some(false),
        None,
        None,
        BoostProtectionState::Eligible,
        None,
        MemoryTaskMetrics {
            progress: 1.0,
            added_at: Some(now - 100),
            seeding_secs: 100,
            uploaded_bytes: 3000,
            upload_speed: 500,
            last_activity_at: Some(now),
            ..Default::default()
        },
    );

    save_plugin_cfg(
        &env.http.state,
        BRUSH_PLUGIN_ID,
        json!({
            "sites": [env.site_search.to_string()],
            "downloader_id": env.dl_id.to_string(),
            "max_seed_hours": 1.5,
            "target_task_ratio": 2.0,
            "target_uploaded_bytes": 4000,
            "max_download_hours": 1.5,
            "min_avg_upspeed_bytes": 10,
            "max_inactive_minutes": 30,
            "dry_run": false,
        }),
    );
    let res = env.run_cycle(json!({}), "cond-del").await;
    assert_eq!(res["evicted_tasks"], 6, "res={res}");
    for tid in targets {
        assert_eq!(
            env.http
                .state
                .store()
                .get_boost_task_fact(&tid)
                .unwrap()
                .unwrap()
                .state,
            BoostTaskState::RetainedOnly
        );
    }
    assert_eq!(
        env.http
            .state
            .store()
            .get_boost_task_fact(&tid_spare)
            .unwrap()
            .unwrap()
            .state,
        BoostTaskState::Seeding
    );
}

#[tokio::test]
async fn delivery_options_free_until_and_per_site_rules_persist_accurately() {
    let env = BrushEnv::new(4096);
    let now = api::job_loop::unix_now();
    let free_deadline = now + 7200;

    let mut t_a = make_torrent(env.site_search, &env.srv.url("site-a.torrent"), "SiteA.TV");
    t_a.free_until = Some(free_deadline);
    let mut t_b = make_torrent(env.site_rss, &env.srv.url("site-b.torrent"), "SiteB.TV");
    t_b.free_2x = true;
    t_b.free_until = Some(free_deadline + 600);

    env.search_provider.feeds.lock().unwrap().insert(
        env.site_search,
        SiteFeedData {
            search_hits: vec![t_a],
            ..Default::default()
        },
    );
    env.rss_provider.feeds.lock().unwrap().insert(
        env.site_rss,
        SiteFeedData {
            rss_hits: vec![t_b],
            ..Default::default()
        },
    );

    save_plugin_cfg(
        &env.http.state,
        BRUSH_PLUGIN_ID,
        json!({
            "sites": [env.site_search.to_string(), env.site_rss.to_string()],
            "downloader_id": env.dl_id.to_string(),
            "hold_days": 2,
            "relative_directory": "brush/global",
            "category": "brush-global",
            "tags": ["global-tag"],
            "upload_limit_bytes": 100_000,
            "download_limit_bytes": 200_000,
            "sequential": false,
            "first_last_piece": false,
            "rules": [{
                "site_id": env.site_rss.to_string(),
                "free_mode": "2x_free",
                "hold_days": 9,
                "relative_directory": "brush/rss-vip",
                "category": "rss-cat",
                "tags": ["rss-tag"],
                "upload_limit_bytes": 555_000,
                "download_limit_bytes": 777_000,
                "sequential": true,
                "first_last_piece": true
            }],
            "dry_run": false,
        }),
    );

    let res = env.run_cycle(json!({}), "delivery-rules").await;
    assert_eq!(res["added"], 2, "res={res}");

    let opts = env.dl.submitted_options();
    assert_eq!(opts.len(), 2);
    let opt_b = opts
        .iter()
        .find(|o| o.category.as_deref() == Some("rss-cat"))
        .expect("rule delivery option category=rss-cat");
    assert_eq!(opt_b.save_path.as_deref(), Some("brush/rss-vip"));
    assert_eq!(opt_b.upload_limit, Some(555_000));
    assert_eq!(opt_b.download_limit, Some(777_000));
    assert!(opt_b.sequential);
    assert!(opt_b.first_last_piece);
    assert!(
        opt_b
            .extra_tags
            .contains(&format!("plugin:{BRUSH_PLUGIN_ID}:auto"))
    );
    assert!(opt_b.extra_tags.contains(&"global-tag".to_string()));
    assert!(opt_b.extra_tags.contains(&"rss-tag".to_string()));

    let facts = env
        .http
        .state
        .store()
        .get_boost_snapshot_for_plugin(BRUSH_PLUGIN_ID, &[], &[env.dl_id])
        .unwrap()
        .tasks;
    assert_eq!(facts.len(), 2);
    let fact_a = facts
        .iter()
        .find(|f| f.site_id == Some(env.site_search))
        .unwrap();
    let fact_b = facts
        .iter()
        .find(|f| f.site_id == Some(env.site_rss))
        .unwrap();
    assert_eq!(fact_a.free_until, Some(free_deadline));
    assert_eq!(fact_b.free_until, Some(free_deadline + 600));
    let hold_a = fact_a.hold_until.unwrap();
    let hold_b = fact_b.hold_until.unwrap();
    assert!(
        hold_b - hold_a >= 6 * 86_400,
        "rule hold_days=9 vs global hold_days=2: a={hold_a}, b={hold_b}"
    );
}
