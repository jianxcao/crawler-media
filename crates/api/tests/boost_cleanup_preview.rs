#[path = "builtin_boost/support.rs"]
mod support;

use std::path::Path;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use domain::{DownloaderId, SiteId};
use serde_json::json;
use store::{BoostTaskState, PrepareBoostCleanupPlanInput};
use support::plugin_http_support::{HttpEnv, TOKEN, assert_err, assert_ok, req_json};
use support::{BoostTestDownloader, seed_boost_task};

#[allow(clippy::too_many_arguments)]
fn seed_cleanup_plan_task(
    store: &api::Store,
    dl_id: DownloaderId,
    site_id: SiteId,
    key: &str,
    hash: &str,
    file: &Path,
    handle: &str,
    now_utc: i64,
    ttl_secs: i64,
) -> String {
    let tid = seed_boost_task(
        store,
        dl_id,
        site_id,
        key,
        hash,
        BoostTaskState::Seeding,
        100,
        0,
        Some(false),
        None,
    );
    let pjson = json!({ "paths": [file] }).to_string();
    let ret = store
        .mark_boost_task_retained_with_paths(&tid, 1, 100, Some(&pjson), now_utc)
        .unwrap();
    store
        .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: tid.clone(),
            expected_revision: ret.revision,
            cleanup_plan_handle: handle.into(),
            cleanup_plan_json: pjson,
            ttl_secs,
            now_utc,
        })
        .unwrap();
    tid
}

#[tokio::test]
async fn brush_cleanup_preview_selection_never_elevates_to_plan_mode() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let stage = dir.path().join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    let dl = Arc::new(BoostTestDownloader::new(stage.clone()));
    let (state, site_id, dl_id) = support::setup_state(dir.path(), dl, None);
    let env = HttpEnv {
        _dir: dir,
        app: router(state.clone()),
        state,
        member_token: String::new(),
    };
    let f1 = stage.join("Act.mkv");
    let f2 = stage.join("Exp.mkv");
    std::fs::write(&f1, b"1").unwrap();
    std::fs::write(&f2, b"2").unwrap();
    let now = api::job_loop::unix_now();

    let active_tid = seed_cleanup_plan_task(
        env.state.store().as_ref(),
        dl_id,
        site_id,
        "prev-act",
        "7777000011111111111111111111111111111111",
        &f1,
        "act-handle",
        now,
        600,
    );
    let exp_tid = seed_cleanup_plan_task(
        env.state.store().as_ref(),
        dl_id,
        site_id,
        "prev-exp",
        "6666000011111111111111111111111111111111",
        &f2,
        "exp-handle",
        50,
        10,
    );

    for tid in [active_tid, exp_tid] {
        let (st, _, b) = env
            .send(req_json(
                "GET",
                &format!(
                    "/api/v1/plugins/{}/brush-cleanup-preview?task_ids={tid}",
                    support::BOOST_PLUGIN_ID
                ),
                Some(TOKEN),
                &[],
                None,
            ))
            .await;
        let dto = assert_ok(st, &b, StatusCode::OK);
        assert_eq!(dto["mode"], "selection");
        assert!(dto["plan_handle"].is_null());
        assert!(dto["confirmation_token"].is_null());
        assert!(dto["revision"].is_null());
    }

    let (st_act, _, b_act) = env
        .send(req_json(
            "GET",
            &format!(
                "/api/v1/plugins/{}/brush-cleanup-preview?plan_handle=act-handle",
                support::BOOST_PLUGIN_ID
            ),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let act_dto = assert_ok(st_act, &b_act, StatusCode::OK);
    assert_eq!(act_dto["mode"], "plan");
    assert!(act_dto["confirmation_token"].as_str().is_some());

    let (st_exp, _, b_exp) = env
        .send(req_json(
            "GET",
            &format!(
                "/api/v1/plugins/{}/brush-cleanup-preview?plan_handle=exp-handle",
                support::BOOST_PLUGIN_ID
            ),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_err(
        st_exp,
        &b_exp,
        StatusCode::CONFLICT,
        "plugin.cleanup_plan_expired",
    );
}
