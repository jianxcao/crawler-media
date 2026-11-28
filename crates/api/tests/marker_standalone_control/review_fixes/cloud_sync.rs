//! TDD regressions for round-three items 3 & 4:
//! 3. Explicit `theintrodb_only` sync distinguishes upstream 401/429/503 + cooldown from
//!    genuine `no_submissions`, reports HTTP error on upstream failure, and notifies chapter
//!    frames for earlier updated files when a later file in a batch fails.
//! 4. Concurrent manual lock winning before cloud UPSERT commits yields `updated_episodes == 0`
//!    and preserves the locked manual marker.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use domain::UnitKey;
use serde_json::json;
use tower::ServiceExt;

use super::super::{ControllableFingerprint, common, seed_tv_library_and_episodes, test_state};
use super::{
    PanicChapterProbe, ScriptedHttpResponse, StubIntroDbFactory, StubIntroDbServer,
    publish_duration, seed_movie_with_duration,
};

struct StepClock(AtomicI64);
impl StepClock {
    fn new(start_ms: i64) -> Arc<Self> {
        Arc::new(Self(AtomicI64::new(start_ms)))
    }
    fn advance(&self, delta_ms: i64) {
        self.0.fetch_add(delta_ms, Ordering::SeqCst);
    }
}
impl api::playback_events::PlaybackClock for StepClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn scripted(status_line: &str, headers: &[(&str, &str)], body: &str) -> ScriptedHttpResponse {
    ScriptedHttpResponse {
        status_line: status_line.to_string(),
        headers: headers
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
        body: body.to_string(),
    }
}

async fn post_cloud_sync(
    app: &axum::Router,
    url: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(common::request(
            "POST",
            url,
            Some("management-secret"),
            body,
        ))
        .await
        .unwrap();
    let status = resp.status();
    (status, common::json_body(resp).await)
}

async fn assert_batch_partial_503_preserves_first_episode(
    state: &api::ApiState,
    app: &axum::Router,
    dir: &std::path::Path,
    server: &StubIntroDbServer,
) {
    let (tv_lib, tv_media, ep_ids, _) =
        seed_tv_library_and_episodes(&state.store(), dir, &[1, 2], false);
    for &fid in &ep_ids {
        publish_duration(&state.store(), fid, 1_500_000);
    }
    server.push_response(scripted(
        "200 OK",
        &[],
        r#"{"type":"tv","season":1,"episode":1,"group_duration_ms":1500000,"intro":[{"start_ms":10000,"end_ms":40000}],"credits":[]}"#,
    ));
    server.push_response(scripted(
        "503 Service Unavailable",
        &[],
        r#"{"error":"down"}"#,
    ));
    let (st, _) = post_cloud_sync(
        app,
        &format!("/api/v1/libraries/{tv_lib}/items/{tv_media}/markers/detect"),
        json!({ "season": 1, "mode": "theintrodb_only" }),
    )
    .await;
    assert!(
        st.is_server_error(),
        "batch 503 on second episode must fail HTTP request: {st}"
    );
    assert_eq!(
        state
            .store()
            .get_file_marker(ep_ids[0], UnitKey::new(1, 1))
            .unwrap()
            .expect("first episode marker must remain persisted")
            .source,
        "theintrodb"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_cloud_sync_reports_upstream_failure() {
    let dir = tempfile::tempdir().unwrap();
    let clock = StepClock::new(1_700_000_000_000);
    let server = StubIntroDbServer::start_scripted(vec![
        scripted("401 Unauthorized", &[], r#"{"error":"invalid_api_key"}"#),
        scripted(
            "429 Too Many Requests",
            &[("Retry-After", "30")],
            r#"{"error":"rate_limited"}"#,
        ),
        scripted("503 Service Unavailable", &[], r#"{"error":"overloaded"}"#),
        scripted("404 Not Found", &[], r#"{"code":"no_submissions"}"#),
    ]);
    let state = test_state(dir.path(), ControllableFingerprint::fast())
        .with_chapter_probe(Arc::new(PanicChapterProbe))
        .with_theintrodb_factory(Arc::new(StubIntroDbFactory(server.url.clone())))
        .with_playback_clock(clock.clone());
    state
        .store()
        .save_raw_scrape_settings(r#"{"theintrodb_enabled":true,"theintrodb_api_key":"bad-key"}"#)
        .unwrap();
    let app = api::router(state.clone());
    let (mv_lib, mv_media, mv_fid) =
        seed_movie_with_duration(&state.store(), dir.path(), 7_200_000);
    let url = format!("/api/v1/libraries/{mv_lib}/items/{mv_media}/markers/detect");
    let body = || json!({ "mode": "theintrodb_only" });

    // 1. Upstream 401 -> error envelope, and cooldown keeps failing.
    let (st_401, body_401) = post_cloud_sync(&app, &url, body()).await;
    assert!(
        st_401.is_server_error() || st_401 == StatusCode::TOO_MANY_REQUESTS,
        "{body_401}"
    );
    assert!(body_401["error"]["code"].is_string(), "{body_401}");
    let (st_cd, _) = post_cloud_sync(&app, &url, body()).await;
    assert!(st_cd.is_server_error() || st_cd == StatusCode::TOO_MANY_REQUESTS);

    // 2. Past 1h auth cooldown -> 429; past 429 cooldown -> 503.
    clock.advance(3_700_000);
    let (st_429, _) = post_cloud_sync(&app, &url, body()).await;
    assert!(st_429 == StatusCode::TOO_MANY_REQUESTS || st_429.is_server_error());
    clock.advance(120_000);
    let (st_503, _) = post_cloud_sync(&app, &url, body()).await;
    assert!(st_503.is_server_error(), "503 must return 5xx: {st_503}");

    // 3. Past 503 cooldown -> genuine no_submissions returns 200 updated_episodes=0, then valid succeeds.
    clock.advance(120_000);
    let (st_none, body_none) = post_cloud_sync(&app, &url, body()).await;
    assert_eq!(st_none, StatusCode::OK, "{body_none}");
    assert_eq!(body_none["data"]["updated_episodes"], 0);
    let (st_ok, body_ok) = post_cloud_sync(&app, &url, body()).await;
    assert_eq!(st_ok, StatusCode::OK, "{body_ok}");
    assert_eq!(body_ok["data"]["updated_episodes"], 1);
    assert_eq!(
        state
            .store()
            .get_file_marker(mv_fid, UnitKey::WHOLE)
            .unwrap()
            .unwrap()
            .source,
        "theintrodb"
    );

    // 4. Batch partial failure preserves first episode and fails HTTP.
    assert_batch_partial_503_preserves_first_episode(&state, &app, dir.path(), &server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cloud_sync_skips_marker_locked_before_commit() {
    let dir = tempfile::tempdir().unwrap();
    let server = StubIntroDbServer::start();
    let state = test_state(dir.path(), ControllableFingerprint::fast())
        .with_chapter_probe(Arc::new(PanicChapterProbe))
        .with_theintrodb_factory(Arc::new(StubIntroDbFactory(server.url.clone())));
    state
        .store()
        .save_raw_scrape_settings(r#"{"theintrodb_enabled":true}"#)
        .unwrap();
    let app = api::router(state.clone());
    let (mv_lib, mv_media, mv_fid) =
        seed_movie_with_duration(&state.store(), dir.path(), 7_200_000);
    let row = state.store().file_coverage_by_id(mv_fid).unwrap().unwrap();
    let url = format!("/api/v1/libraries/{mv_lib}/items/{mv_media}/markers/detect");

    // Hold Db::Library writer briefly while cloud_sync reads unlocked state on the read pool,
    // then commit a locked manual marker on the held writer before releasing so cloud UPSERT
    // affects 0 rows!
    let store_blocker = state.store();
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let version = row.file.version.clone();
    let blocker = std::thread::spawn(move || {
        let _ = store_blocker.with_writer(api::store::Db::Library, |conn| {
            let _ = entered_tx.send(());
            // Give the foreground cloud handler enough time to pass pre-write read check
            // and wait on the Library writer (well below 1s STORE_WRITE_WAIT).
            std::thread::sleep(Duration::from_millis(250));
            conn.execute(
                "INSERT INTO file_markers (file_id,file_revision,source_version,season,episode,
                    intro_start_ms,intro_end_ms,outro_start_ms,outro_end_ms,source,locked,updated_at)
                 VALUES (?1,?2,?3,?4,?5,5000,35000,NULL,NULL,'manual',1,100)",
                rusqlite::params![
                    mv_fid.to_string(),
                    version.revision,
                    version.source_version,
                    UnitKey::WHOLE.season,
                    UnitKey::WHOLE.episode,
                ],
            )?;
            Ok(())
        });
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();

    let res = app
        .clone()
        .oneshot(common::request(
            "POST",
            &url,
            Some("management-secret"),
            json!({ "mode": "theintrodb_only" }),
        ))
        .await
        .unwrap();
    blocker.join().unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let data = common::json_data(res).await;
    assert_eq!(
        data["updated_episodes"], 0,
        "concurrent manual lock making cloud UPSERT affect 0 rows must report updated_episodes=0: {data}"
    );

    let diag_res = app
        .oneshot(common::request(
            "GET",
            &format!("/api/v1/libraries/{mv_lib}/items/{mv_media}/markers"),
            Some("management-secret"),
            serde_json::Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(diag_res.status(), StatusCode::OK);
    let diag = common::json_data(diag_res).await;
    let persisted = &diag["episodes"][0]["marker"];
    assert_eq!(persisted["source"], "manual");
    assert_eq!(persisted["locked"], true);
    assert_eq!(persisted["intro_start_ms"], 5000);
    assert_eq!(persisted["intro_end_ms"], 35000);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn theintrodb_only_returns_503_when_library_writer_busy_and_keeps_expected_skips_200() {
    let dir = tempfile::tempdir().unwrap();
    let server = StubIntroDbServer::start();
    let state = test_state(dir.path(), ControllableFingerprint::fast())
        .with_chapter_probe(Arc::new(PanicChapterProbe))
        .with_theintrodb_factory(Arc::new(StubIntroDbFactory(server.url.clone())));
    state
        .store()
        .save_raw_scrape_settings(r#"{"theintrodb_enabled":true}"#)
        .unwrap();
    let app = api::router(state.clone());
    let (mv_lib, mv_media, mv_fid) =
        seed_movie_with_duration(&state.store(), dir.path(), 7_200_000);
    let url = format!("/api/v1/libraries/{mv_lib}/items/{mv_media}/markers/detect");

    let store_for_blocker = state.store();
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let blocker = std::thread::spawn(move || {
        let _ = store_for_blocker.with_writer(api::store::Db::Library, |_| {
            let _ = entered_tx.send(());
            let _ = release_rx.recv_timeout(Duration::from_secs(10));
            Ok(())
        });
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();

    let busy_res = app
        .clone()
        .oneshot(common::request(
            "POST",
            &url,
            Some("management-secret"),
            json!({ "mode": "theintrodb_only" }),
        ))
        .await
        .unwrap();
    let busy_status = busy_res.status();
    let has_retry_after = busy_res.headers().contains_key("retry-after");
    let busy_body = common::json_body(busy_res).await;
    let _ = release_tx.send(());
    blocker.join().unwrap();

    assert_eq!(busy_status, StatusCode::SERVICE_UNAVAILABLE, "{busy_body}");
    assert_eq!(busy_body["error"]["code"], "runtime.busy", "{busy_body}");
    assert!(has_retry_after, "503 runtime.busy must include Retry-After");
    assert!(
        state
            .store()
            .get_file_marker(mv_fid, UnitKey::WHOLE)
            .unwrap()
            .is_none()
    );

    let (ok_st, ok_body) = post_cloud_sync(&app, &url, json!({ "mode": "theintrodb_only" })).await;
    assert_eq!(ok_st, StatusCode::OK, "{ok_body}");
    assert_eq!(ok_body["data"]["updated_episodes"], 1);
    assert_eq!(
        state
            .store()
            .get_file_marker(mv_fid, UnitKey::WHOLE)
            .unwrap()
            .unwrap()
            .source,
        "theintrodb"
    );

    let lock_res = app
        .clone()
        .oneshot(common::request(
            "PUT",
            &format!("/api/v1/libraries/{mv_lib}/items/{mv_media}/markers"),
            Some("management-secret"),
            json!({
                "file_id": mv_fid.to_string(),
                "intro_start_ms": 1000,
                "intro_end_ms": 30000,
                "locked": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(lock_res.status(), StatusCode::OK);

    let (skip_st, skip_body) =
        post_cloud_sync(&app, &url, json!({ "mode": "theintrodb_only" })).await;
    assert_eq!(skip_st, StatusCode::OK, "{skip_body}");
    assert_eq!(skip_body["data"]["updated_episodes"], 0);
}
