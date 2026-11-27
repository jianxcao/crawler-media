#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use std::sync::Arc;

use api::{ApiState, Store, record_subscribe_trace_for_test, router};
use axum::http::StatusCode;
use domain::DownloaderId;
use downloader::{Downloader, MemoryDownloader};
use hooks::{HookStepStatus, HookStepTrace, HookTrace};
use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
use plugin_http_support::{ADMIN_PASS, HttpEnv, TOKEN, assert_ok, req_json};
use serde_json::json;
use store::download_operations::{
    AcceptDownloadOperationInput, DownloadOwnershipState, DownloadProvenanceKind,
    RecordDownloadOwnershipInput,
};
use tower::ServiceExt;

struct LeakingFetcher;
impl Fetcher for LeakingFetcher {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch(
            "GET https://secret-pt.internal/rss.php?passkey=FETCH_SECRET_PK_999 failed".into(),
        ))
    }
}

async fn make_r11_env() -> (HttpEnv, Arc<MemoryDownloader>, DownloaderId) {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl_mem = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(LeakingFetcher),
        dl_mem.clone() as Arc<dyn Downloader>,
    )
    .unwrap();
    let app = router(state.clone());
    let _ = app
        .clone()
        .oneshot(req_json(
            "POST",
            "/api/v1/users",
            Some(TOKEN),
            &[],
            Some(json!({ "login": "r11-member", "password": "r11-member-password-123" })),
        ))
        .await
        .unwrap();
    let login_resp = app
        .clone()
        .oneshot(req_json(
            "POST",
            "/api/v1/auth/login",
            None,
            &[],
            Some(json!({ "username": "r11-member", "password": "r11-member-password-123" })),
        ))
        .await
        .unwrap();
    let login_bytes = axum::body::to_bytes(login_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let login_json: serde_json::Value = serde_json::from_slice(&login_bytes).unwrap();
    let member_token = login_json["data"]["token"].as_str().unwrap().to_string();
    let env = HttpEnv {
        _dir: dir,
        state,
        app,
        member_token,
    };
    let dl_id = env.insert_downloader("DL-R11-Default");
    env.state
        .store()
        .with_writer(store::Db::App, |c| {
            c.execute(
                "UPDATE downloaders SET is_default = 1 WHERE id = ?1",
                [dl_id.to_string()],
            )?;
            Ok(())
        })
        .unwrap();
    (env, dl_mem, dl_id)
}

async fn create_member_sub(env: &HttpEnv, title: &str) -> domain::SubscribeId {
    let (st_rs, _, b_rs) = env
        .send(req_json(
            "POST",
            "/api/v1/rule-sets",
            Some(TOKEN),
            &[],
            Some(json!({
                "name": format!("RS-{title}"),
                "atoms": [{ "kind": "title_match", "value": "Show", "priority": 80 }]
            })),
        ))
        .await;
    let filter_id = assert_ok(st_rs, &b_rs, StatusCode::CREATED)["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (st_sub, _, b_sub) = env
        .send(req_json(
            "POST",
            "/api/v1/subscriptions",
            Some(&env.member_token),
            &[],
            Some(json!({
                "media": {
                    "kind": "movie",
                    "title": title,
                    "original_title": title,
                    "year": 2026
                },
                "coverage": { "kind": "movie" },
                "fetch_mode": "search",
                "filter_id": filter_id
            })),
        ))
        .await;
    assert_ok(st_sub, &b_sub, StatusCode::CREATED)["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

#[tokio::test]
async fn manual_submit_allows_paused_subscription_without_409_conflict() {
    let (env, _dl_mem, dl_id) = make_r11_env().await;
    let lib_dir = env._dir.path().join("lib-r11-paused");
    std::fs::create_dir_all(&lib_dir).unwrap();
    let (st_lib, _, b_lib) = env
        .send(req_json(
            "POST",
            "/api/v1/libraries",
            Some(TOKEN),
            &[],
            Some(json!({
                "name": "Movies R11",
                "kind": "movie",
                "root_paths": [lib_dir.to_string_lossy()]
            })),
        ))
        .await;
    assert_ok(st_lib, &b_lib, StatusCode::OK);

    let sub_id = create_member_sub(&env, "Paused Sub Movie").await;
    let (st_pause, _, b_pause) = env
        .send(req_json(
            "PATCH",
            &format!("/api/v1/subscriptions/{sub_id}"),
            Some(&env.member_token),
            &[],
            Some(json!({ "tracking_state": "paused" })),
        ))
        .await;
    assert_ok(st_pause, &b_pause, StatusCode::OK);

    let hash = "1234abcd1234abcd1234abcd1234abcd1234abcd";
    let (st_sub, _, b_sub) = env
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(&env.member_token),
            &[],
            Some(json!({
                "title": "Paused.Sub.Movie.2026.1080p",
                "download_url": format!("magnet:?xt=urn:btih:{hash}&dn=Paused.Sub.Movie.2026"),
                "subscribe_id": sub_id.to_string(),
                "downloader_id": dl_id.to_string(),
                "size_bytes": 4096,
                "force": true
            })),
        ))
        .await;
    assert_eq!(
        st_sub,
        StatusCode::OK,
        "manual submit to paused subscription must succeed instead of 409: {b_sub}"
    );
}

fn seed_r11_pending_and_owned(
    store: &Store,
    sub_id: domain::SubscribeId,
    dl_id: DownloaderId,
    site_id: domain::SiteId,
    raw_enc: &str,
    hash: &str,
) {
    store
        .record_pending_submission(
            sub_id,
            100,
            &store::PendingDownload {
                torrent: domain::Torrent {
                    site_id,
                    title: "Member.Redact.Movie.2026.1080p".into(),
                    enclosure: raw_enc.into(),
                    size_bytes: Some(4096),
                    seeders: Some(5),
                    free: false,
                    hr: Some(false),
                    imdb_id: None,
                    id: Some("55".into()),
                    leechers: None,
                    snatched: None,
                    upload_time: None,
                    detail_url: None,
                    category: None,
                    poster_url: None,
                },
                downloader_id: Some(dl_id),
                submitted_at: Some(1000),
                release_override: None,
            },
        )
        .unwrap();
    store.set_pending_info_hash(sub_id, raw_enc, hash).unwrap();
    store
        .accept_download_operation(&AcceptDownloadOperationInput {
            operation_id: "op-r11-1".into(),
            idempotency_key: "ik-r11-1".into(),
            source_kind: "subscribe".into(),
            subscribe_id: Some(sub_id),
            plugin_id: None,
            subject: "system".into(),
            cause_id: "c-r11".into(),
            site_id: Some(site_id),
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            enclosure: raw_enc.into(),
            ownership_tag: "cm-op:op-r11-1".into(),
            info_hash: Some(hash.into()),
            bytes_digest: None,
            verified_bytes: Some(4096),
            request_digest: "rd-r11-1".into(),
            now_utc: 1000,
        })
        .unwrap();
    store
        .record_download_ownership(&RecordDownloadOwnershipInput {
            downloader_id: dl_id,
            ownership_tag: "cm-op:op-r11-1".into(),
            info_hash: Some(hash.into()),
            bytes_digest: None,
            operation_id: "op-r11-1".into(),
            subscribe_id: Some(sub_id),
            plugin_id: None,
            subject: "system".into(),
            site_id: Some(site_id),
            enclosure: raw_enc.into(),
            ownership_state: DownloadOwnershipState::Owned,
            provenance_kind: DownloadProvenanceKind::CreatedFresh,
            verified_bytes: Some(4096),
            now_utc: 1000,
        })
        .unwrap();
}

#[tokio::test]
async fn member_tasks_and_activities_redact_pt_enclosures_and_step_traces() {
    let (env, _dl_mem, dl_id) = make_r11_env().await;
    let site_id = env.insert_site("PrivatePT");
    let sub_id = create_member_sub(&env, "Member Redact Movie").await;
    let raw_enc =
        "https://user:SECRET_PW@secret-pt.internal/download.php?id=55&passkey=TASK_ID_SECRET_PK";
    let hash = "5555aaaa5555aaaa5555aaaa5555aaaa5555aaaa";
    seed_r11_pending_and_owned(
        env.state.store().as_ref(),
        sub_id,
        dl_id,
        site_id,
        raw_enc,
        hash,
    );

    let (st_tasks, _, b_tasks) = env
        .send(req_json(
            "GET",
            "/api/v1/downloaders/tasks",
            Some(&env.member_token),
            &[],
            None,
        ))
        .await;
    assert_eq!(st_tasks, StatusCode::OK);
    let tasks_str = b_tasks.to_string();
    for secret in ["TASK_ID_SECRET_PK", "SECRET_PW", "secret-pt.internal"] {
        assert!(
            !tasks_str.contains(secret),
            "member GET /downloaders/tasks leaked `{secret}`: {tasks_str}"
        );
    }

    record_subscribe_trace_for_test(
        &env.state,
        sub_id,
        HookTrace {
            id: "tr-r11".into(),
            hook_id: "download.before".into(),
            workflow_id: "wf-r11".into(),
            input_revision: 1,
            decision: "reject".into(),
            reason: Some("top https://secret-pt.internal/dl?passkey=TOP_PK".into()),
            duration_ms: 1,
            steps: vec![HookStepTrace {
                plugin_id: "org.example.gate".into(),
                component: "gate".into(),
                status: HookStepStatus::Rejected,
                decision: "reject".into(),
                duration_ms: 1,
                reason: Some(
                    "step https://secret-pt.internal/dl?passkey=STEP_SECRET_PK_777".into(),
                ),
            }],
        },
    );
    let (st_act, _, b_act) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/subscriptions/{sub_id}/activities"),
            Some(&env.member_token),
            &[],
            None,
        ))
        .await;
    assert_eq!(st_act, StatusCode::OK);
    let act_str = b_act.to_string();
    for secret in ["STEP_SECRET_PK_777", "TOP_PK", "secret-pt.internal"] {
        assert!(
            !act_str.contains(secret),
            "member GET /subscriptions/{sub_id}/activities leaked `{secret}`: {act_str}"
        );
    }
}

#[tokio::test]
async fn member_search_and_run_subscribe_redact_site_fetch_urls_and_passkeys() {
    let (env, _dl_mem, _dl_id) = make_r11_env().await;
    let _site_id = env.insert_site("PrivatePT");
    let sub_id = create_member_sub(&env, "Member Search Redact").await;

    let (st_srch, _, b_srch) = env
        .send(req_json(
            "GET",
            "/api/v1/search/torrents?keyword=Show&skip_history=true",
            Some(&env.member_token),
            &[],
            None,
        ))
        .await;
    assert_eq!(st_srch, StatusCode::OK);
    let srch_str = b_srch.to_string();
    assert!(
        !srch_str.contains("FETCH_SECRET_PK_999") && !srch_str.contains("secret-pt.internal"),
        "member GET /search/torrents leaked site fetch URL/passkey: {srch_str}"
    );

    let (st_run, _, b_run) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/subscriptions/{sub_id}/run"),
            Some(&env.member_token),
            &[],
            None,
        ))
        .await;
    assert_ne!(st_run, StatusCode::OK);
    let run_str = b_run.to_string();
    assert!(
        !run_str.contains("FETCH_SECRET_PK_999") && !run_str.contains("secret-pt.internal"),
        "member POST /subscriptions/{sub_id}/run leaked site fetch URL/passkey: {run_str}"
    );
}
