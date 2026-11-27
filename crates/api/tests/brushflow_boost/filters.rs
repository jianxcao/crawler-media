use serde_json::json;

use super::support::{BRUSH_PLUGIN_ID, BrushEnv, SiteFeedData, make_torrent, save_plugin_cfg};

fn format_utc_hhmm_window(start_min: i64, end_min: i64) -> String {
    let s = start_min.rem_euclid(1440);
    let e = end_min.rem_euclid(1440);
    format!("{:02}:{:02}-{:02}:{:02}", s / 60, s % 60, e / 60, e % 60)
}

fn format_site_time_local(utc_ts: i64, offset_secs: i32) -> String {
    let offset = time::UtcOffset::from_whole_seconds(offset_secs).unwrap();
    let dt = time::OffsetDateTime::from_unix_timestamp(utc_ts)
        .unwrap()
        .to_offset(offset);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        dt.year(),
        u8::from(dt.month()),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second()
    )
}

#[tokio::test]
async fn categories_search_filter_and_rss_post_filter_skip_movie_without_tmdb() {
    let env = BrushEnv::new(4096);
    let mut tv_s = make_torrent(env.site_search, &env.srv.url("s-tv.torrent"), "Show.S01E01");
    tv_s.category = Some("tv".into());
    let mut mov_s = make_torrent(
        env.site_search,
        &env.srv.url("s-mov.torrent"),
        "Film.2026.1080p",
    );
    mov_s.category = Some("movie".into());

    let mut tv_r = make_torrent(env.site_rss, &env.srv.url("r-tv.torrent"), "RssShow.S01E02");
    tv_r.category = Some("tv".into());
    let mut mov_r = make_torrent(
        env.site_rss,
        &env.srv.url("r-mov.torrent"),
        "RssFilm.2026.1080p",
    );
    mov_r.category = Some("movie".into());

    env.search_provider.feeds.lock().unwrap().insert(
        env.site_search,
        SiteFeedData {
            search_hits: vec![mov_s, tv_s],
            ..Default::default()
        },
    );
    env.rss_provider.feeds.lock().unwrap().insert(
        env.site_rss,
        SiteFeedData {
            rss_hits: vec![mov_r, tv_r],
            ..Default::default()
        },
    );

    save_plugin_cfg(
        &env.http.state,
        BRUSH_PLUGIN_ID,
        json!({
            "sites": [env.site_search.to_string(), env.site_rss.to_string()],
            "downloader_id": env.dl_id.to_string(),
            "categories": ["tv"],
            "dry_run": false,
        }),
    );
    let res = env.run_cycle(json!({}), "cat-tv").await;
    assert_eq!(res["added"], 2, "res={res}");

    let captured = env
        .search_provider
        .captured_search_categories
        .lock()
        .unwrap()
        .clone();
    assert_eq!(captured, vec![(env.site_search, vec!["tv".to_string()])]);

    let added_titles: Vec<String> = env.dl.inner.added().into_iter().map(|t| t.title).collect();
    assert!(added_titles.contains(&"Show.S01E01".to_string()));
    assert!(added_titles.contains(&"RssShow.S01E02".to_string()));
    assert!(!added_titles.iter().any(|t| t.contains("Film")));
}

fn build_filter_candidates(
    env: &BrushEnv,
    sh_site: domain::SiteId,
    now: i64,
    site_offset: i32,
) -> Vec<domain::Torrent> {
    let mut t_good = make_torrent(
        sh_site,
        &env.srv.url("good.torrent"),
        "Show.Good.1080p.WEB-DL",
    );
    t_good.free = true;
    t_good.free_2x = true;
    t_good.hr = Some(false);
    t_good.seeders = Some(10);
    t_good.leechers = Some(4);
    t_good.size_bytes = Some(3000);
    t_good.upload_time = Some(format_site_time_local(now - 300, site_offset));

    let mut t_only_free = t_good.clone();
    t_only_free.title = "Show.Free.1080p".into();
    t_only_free.enclosure = env.srv.url("free.torrent");
    t_only_free.free = true;
    t_only_free.free_2x = false;

    let mut t_paid = t_good.clone();
    t_paid.title = "Show.Paid.1080p".into();
    t_paid.enclosure = env.srv.url("paid.torrent");
    t_paid.free = false;
    t_paid.free_2x = false;

    let mut t_hr = t_good.clone();
    t_hr.title = "Show.HR.1080p".into();
    t_hr.enclosure = env.srv.url("hr.torrent");
    t_hr.hr = Some(true);

    let mut t_many_seed = t_good.clone();
    t_many_seed.title = "Show.TooManySeed.1080p".into();
    t_many_seed.enclosure = env.srv.url("many.torrent");
    t_many_seed.seeders = Some(50);

    let mut t_few_leech = t_good.clone();
    t_few_leech.title = "Show.FewLeech.1080p".into();
    t_few_leech.enclosure = env.srv.url("leech.torrent");
    t_few_leech.leechers = Some(1);

    let mut t_too_big = t_good.clone();
    t_too_big.title = "Show.TooBig.1080p".into();
    t_too_big.enclosure = env.srv.url("big.torrent");
    t_too_big.size_bytes = Some(9000);

    let mut t_too_old = t_good.clone();
    t_too_old.title = "Show.TooOld.1080p".into();
    t_too_old.enclosure = env.srv.url("old.torrent");
    t_too_old.upload_time = Some(format_site_time_local(now - 7200, site_offset));

    let mut t_cam = t_good.clone();
    t_cam.title = "Show.CAM.1080p".into();
    t_cam.enclosure = env.srv.url("cam.torrent");

    vec![
        t_only_free,
        t_paid,
        t_hr,
        t_many_seed,
        t_few_leech,
        t_too_big,
        t_too_old,
        t_cam,
        t_good,
    ]
}

#[tokio::test]
async fn candidate_filters_free_mode_hr_seeders_leechers_size_age_regex() {
    let env = BrushEnv::new(4096);
    let now = api::job_loop::unix_now();
    let site_offset = indexer::resolve_offset_secs("Asia/Shanghai", now);
    let sh_site =
        super::support::insert_site(&env.http.state, "SH", "brush_search_idx", "Asia/Shanghai");

    let hits = build_filter_candidates(&env, sh_site, now, site_offset);
    env.search_provider.feeds.lock().unwrap().insert(
        sh_site,
        SiteFeedData {
            search_hits: hits,
            ..Default::default()
        },
    );

    save_plugin_cfg(
        &env.http.state,
        BRUSH_PLUGIN_ID,
        json!({
            "sites": [sh_site.to_string()],
            "downloader_id": env.dl_id.to_string(),
            "free_mode": "2x_free",
            "exclude_hr": true,
            "min_seeders": 5,
            "max_seeders": 20,
            "min_leechers": 3,
            "min_size_bytes": 2000,
            "max_size_bytes": 5000,
            "max_pub_age_minutes": 30,
            "include_regex": "1080p",
            "exclude_regex": "CAM|TS",
            "dry_run": false,
        }),
    );
    let r1 = env.run_cycle(json!({}), "flt-2x").await;
    assert_eq!(r1["added"], 1, "r1={r1}");
    assert_eq!(env.dl.inner.added()[0].title, "Show.Good.1080p.WEB-DL");

    let r2 = env
        .run_cycle(json!({ "free_mode": "free" }), "flt-free")
        .await;
    assert_eq!(r2["added"], 1, "r2={r2}");
    assert!(
        env.dl
            .inner
            .added()
            .iter()
            .any(|t| t.title == "Show.Free.1080p")
    );

    let r3 = env
        .run_cycle(json!({ "free_mode": "any" }), "flt-any")
        .await;
    assert_eq!(r3["added"], 1, "r3={r3}");
    assert!(
        env.dl
            .inner
            .added()
            .iter()
            .any(|t| t.title == "Show.Paid.1080p")
    );
}

#[tokio::test]
async fn active_hours_and_target_site_ratio_gate_site_discovery() {
    let env = BrushEnv::new(4096);
    let now = api::job_loop::unix_now();
    let cur_min = now.rem_euclid(86_400) / 60;
    let outside = format_utc_hhmm_window(cur_min + 120, cur_min + 180);
    let inside = format_utc_hhmm_window(cur_min - 30, cur_min + 30);

    let cand = make_torrent(
        env.site_search,
        &env.srv.url("ah.torrent"),
        "Active.Hours.TV",
    );
    env.search_provider.feeds.lock().unwrap().insert(
        env.site_search,
        SiteFeedData {
            search_hits: vec![cand],
            stats: Some((4000, 1000)),
            ..Default::default()
        },
    );

    save_plugin_cfg(
        &env.http.state,
        BRUSH_PLUGIN_ID,
        json!({
            "sites": [env.site_search.to_string()],
            "downloader_id": env.dl_id.to_string(),
            "active_hours": outside,
            "dry_run": false,
        }),
    );
    let r_out = env.run_cycle(json!({}), "ah-out").await;
    assert_eq!(r_out["added"], 0, "outside active_hours must skip: {r_out}");

    let r_ratio = env
        .run_cycle(
            json!({ "active_hours": inside, "target_site_ratio": 3.0 }),
            "ratio-high",
        )
        .await;
    assert_eq!(r_ratio["added"], 0, "ratio 4.0 >= 3.0 must skip: {r_ratio}");

    env.search_provider
        .feeds
        .lock()
        .unwrap()
        .get_mut(&env.site_search)
        .unwrap()
        .stats = Some((1500, 1000));
    let r_ok = env
        .run_cycle(
            json!({ "active_hours": inside, "target_site_ratio": 3.0 }),
            "ratio-ok",
        )
        .await;
    assert_eq!(r_ok["added"], 1, "ratio 1.5 < 3.0 must admit: {r_ok}");
}
