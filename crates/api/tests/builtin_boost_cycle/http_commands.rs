use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use serde_json::{Value, json};

use super::support::plugin_http_support::{HttpEnv, TOKEN, assert_ok, req_json};
use super::support::{
    BOOST_PLUGIN_ID, BoostTestDownloader, StubTorrentServer, minimal_torrent_bytes, poll_op,
    save_boost_config, setup_state,
};

async fn submit_boost_http_cmd(env: &HttpEnv, kind: &str, payload: Value, idem: &str) -> String {
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
            &[
                ("If-Match", &format!("\"{rev}\"")),
                ("Idempotency-Key", idem),
            ],
            Some(json!({ "kind": kind, "payload": payload })),
        ))
        .await;
    let data = assert_ok(st, &b, StatusCode::ACCEPTED);
    data["operation_id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn http_downloader_add_11th_with_budget_none_settles_to_rejected() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(BoostTestDownloader::new(dir.path().join("stage")));
    let srv = StubTorrentServer::start_dynamic(1024);
    let (state, site_id, dl_id) = setup_state(dir.path(), dl, Some(&srv));
    let env = HttpEnv {
        _dir: dir,
        app: router(state.clone()),
        state,
        member_token: String::new(),
    };
    save_boost_config(&env.state, json!({ "max_adds_per_hour": 10 }));

    for idx in 1..=10 {
        let op_id = submit_boost_http_cmd(
            &env,
            "downloader.add",
            json!({
                "downloader_id": dl_id.to_string(),
                "site_id": site_id.to_string(),
                "download_ref": srv.url(&format!("hr-{idx}.torrent")),
                "title": format!("Hr-{idx}")
            }),
            &format!("http-hr-{idx}"),
        )
        .await;
        let op = poll_op(&env, &op_id).await;
        assert_eq!(op["state"], "succeeded", "idx={idx} op={op}");
        assert_eq!(op["phase"], "completed");
    }

    let op11_id = submit_boost_http_cmd(
        &env,
        "downloader.add",
        json!({
            "downloader_id": dl_id.to_string(),
            "site_id": site_id.to_string(),
            "download_ref": srv.url("hr-11.torrent"),
            "title": "Hr-11"
        }),
        "http-hr-11",
    )
    .await;
    let op11 = poll_op(&env, &op11_id).await;
    assert_eq!(op11["state"], "rejected", "op11={op11}");
    assert_eq!(op11["phase"], "quota_exceeded", "op11={op11}");
    assert_eq!(op11["error"], "quota_exceeded", "op11={op11}");
    let snap = env
        .state
        .store()
        .get_boost_snapshot_for_plugin(BOOST_PLUGIN_ID, &[], &[])
        .unwrap();
    assert_eq!(snap.reserved_bytes, 0);
}

#[tokio::test]
async fn http_downloader_add_budget_exhausted_and_duplicate_reservation_settle_to_rejected() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(BoostTestDownloader::new(dir.path().join("stage")));
    let srv = StubTorrentServer::start_dynamic(4096);
    let (state, site_id, dl_id) = setup_state(dir.path(), dl, Some(&srv));
    let env = HttpEnv {
        _dir: dir,
        app: router(state.clone()),
        state,
        member_token: String::new(),
    };
    save_boost_config(&env.state, json!({ "budget_bytes": 5000 }));

    let op1_id = submit_boost_http_cmd(
        &env,
        "downloader.add",
        json!({
            "downloader_id": dl_id.to_string(),
            "site_id": site_id.to_string(),
            "download_ref": srv.url("b1.torrent"),
            "title": "B1"
        }),
        "b1",
    )
    .await;
    assert_eq!(poll_op(&env, &op1_id).await["state"], "succeeded");

    let op2_id = submit_boost_http_cmd(
        &env,
        "downloader.add",
        json!({
            "downloader_id": dl_id.to_string(),
            "site_id": site_id.to_string(),
            "download_ref": srv.url("b2.torrent"),
            "title": "B2"
        }),
        "b2",
    )
    .await;
    let op2 = poll_op(&env, &op2_id).await;
    assert_eq!(op2["state"], "rejected", "op2={op2}");
    assert_eq!(op2["phase"], "quota_exceeded", "op2={op2}");

    save_boost_config(&env.state, json!({ "budget_bytes": 4096 }));
    let op3_id = submit_boost_http_cmd(
        &env,
        "downloader.add",
        json!({
            "downloader_id": dl_id.to_string(),
            "site_id": site_id.to_string(),
            "download_ref": srv.url("b3.torrent"),
            "title": "B3"
        }),
        "b3",
    )
    .await;
    let op3 = poll_op(&env, &op3_id).await;
    assert_eq!(op3["state"], "rejected", "op3={op3}");
    assert_eq!(op3["phase"], "quota_exceeded", "op3={op3}");

    // Duplicate active info_hash settles cleanly to rejected (duplicate_active_reservation).
    let dir2 = tempfile::tempdir_in("/tmp").unwrap();
    let dl2 = Arc::new(BoostTestDownloader::new(dir2.path().join("stage")));
    let srv_same = StubTorrentServer::start(minimal_torrent_bytes("SameHttpDup.2026", 4096));
    let (st2, s2, dl_id2) = setup_state(dir2.path(), dl2, Some(&srv_same));
    let env2 = HttpEnv {
        _dir: dir2,
        app: router(st2.clone()),
        state: st2,
        member_token: String::new(),
    };
    save_boost_config(&env2.state, json!({ "budget_bytes": 100_000 }));
    let same_a = submit_boost_http_cmd(
        &env2,
        "downloader.add",
        json!({
            "downloader_id": dl_id2.to_string(),
            "site_id": s2.to_string(),
            "download_ref": srv_same.url("same-a.torrent"),
            "title": "SameA"
        }),
        "same-a",
    )
    .await;
    assert_eq!(poll_op(&env2, &same_a).await["state"], "succeeded");

    let same_b = submit_boost_http_cmd(
        &env2,
        "downloader.add",
        json!({
            "downloader_id": dl_id2.to_string(),
            "site_id": s2.to_string(),
            "download_ref": srv_same.url("same-b.torrent"),
            "title": "SameB"
        }),
        "same-b",
    )
    .await;
    let op_dup = poll_op(&env2, &same_b).await;
    assert_eq!(op_dup["state"], "rejected", "op_dup={op_dup}");
    assert_eq!(
        op_dup["phase"], "duplicate_active_reservation",
        "op_dup={op_dup}"
    );
}

#[tokio::test]
async fn http_boost_run_invalid_site_id_settles_to_rejected() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(BoostTestDownloader::new(dir.path().join("stage")));
    let (state, _, _) = setup_state(dir.path(), dl, None);
    let env = HttpEnv {
        _dir: dir,
        app: router(state.clone()),
        state,
        member_token: String::new(),
    };

    let op1_id = submit_boost_http_cmd(
        &env,
        "boost.run",
        json!({ "site_id": "not-a-uuid" }),
        "bad-site-1",
    )
    .await;
    let op1 = poll_op(&env, &op1_id).await;
    assert_eq!(op1["state"], "rejected", "op1={op1}");
    assert_eq!(op1["phase"], "invalid-site_id-not-a-uuid", "op1={op1}");
    assert_eq!(op1["error"], "invalid-site_id-not-a-uuid", "op1={op1}");

    let op2_id = submit_boost_http_cmd(
        &env,
        "boost.run",
        json!({ "sites": ["bad site `1`"] }),
        "bad-site-2",
    )
    .await;
    let op2 = poll_op(&env, &op2_id).await;
    assert_eq!(op2["state"], "rejected", "op2={op2}");
    assert!(!op2["phase"].as_str().unwrap_or("").is_empty(), "op2={op2}");
}

#[tokio::test]
async fn deleting_sole_downloader_cleans_active_records_unblocks_drain_and_rejects_stale_adds() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(BoostTestDownloader::new(dir.path().join("stage")));
    let srv = StubTorrentServer::start_dynamic(2048);
    let (state, site_id, dl_id) = setup_state(dir.path(), dl.clone(), Some(&srv));
    let env = HttpEnv {
        _dir: dir,
        app: router(state.clone()),
        state,
        member_token: String::new(),
    };
    save_boost_config(
        &env.state,
        json!({ "budget_bytes": 100_000, "downloader_id": dl_id.to_string() }),
    );

    let ok_id = submit_boost_http_cmd(
        &env,
        "downloader.add",
        json!({
            "downloader_id": dl_id.to_string(),
            "site_id": site_id.to_string(),
            "download_ref": srv.url("del-ok.torrent"),
            "title": "DelOk"
        }),
        "del-ok",
    )
    .await;
    assert_eq!(poll_op(&env, &ok_id).await["state"], "succeeded");

    dl.unknown_on_submit
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let unk_id = submit_boost_http_cmd(
        &env,
        "downloader.add",
        json!({
            "downloader_id": dl_id.to_string(),
            "site_id": site_id.to_string(),
            "download_ref": srv.url("del-unk.torrent"),
            "title": "DelUnk"
        }),
        "del-unk",
    )
    .await;
    let _ = env
        .send(req_json(
            "POST",
            "/api/v1/jobs/tick",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;

    let (st_del, _, b_del) = env
        .send(req_json(
            "DELETE",
            &format!("/api/v1/downloaders/{dl_id}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_ok(st_del, &b_del, StatusCode::OK);

    let snap = env
        .state
        .store()
        .get_boost_snapshot_for_plugin(BOOST_PLUGIN_ID, &[], &[])
        .unwrap();
    assert_eq!(
        snap.reserved_bytes, 0,
        "deleted dl must release boost reservations"
    );
    assert_eq!(
        snap.used_bytes, 0,
        "deleted dl must retire non-retained task facts"
    );

    let (st_tasks, _, b_tasks) = env
        .send(req_json(
            "GET",
            "/api/v1/downloaders/tasks",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let tasks = assert_ok(st_tasks, &b_tasks, StatusCode::OK);
    assert_eq!(tasks["items"].as_array().unwrap().len(), 0, "tasks={tasks}");

    let unk_done = poll_op(&env, &unk_id).await;
    assert_eq!(unk_done["state"], "failed", "unk_done={unk_done}");

    assert_deleted_downloader_rejects_stale_commands(&env, &srv, site_id, dl_id).await;
}

async fn assert_deleted_downloader_rejects_stale_commands(
    env: &HttpEnv,
    srv: &StubTorrentServer,
    site_id: domain::SiteId,
    dl_id: domain::DownloaderId,
) {
    let rev = env
        .state
        .store()
        .get_plugin(BOOST_PLUGIN_ID)
        .unwrap()
        .unwrap()
        .revision;
    let (st_add_missing, _, b_add_missing) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{BOOST_PLUGIN_ID}/commands"),
            Some(TOKEN),
            &[
                ("If-Match", &format!("\"{rev}\"")),
                ("Idempotency-Key", "del-stale-add"),
            ],
            Some(json!({
                "kind": "downloader.add",
                "payload": {
                    "downloader_id": dl_id.to_string(),
                    "site_id": site_id.to_string(),
                    "download_ref": srv.url("del-stale.torrent"),
                    "title": "DelStale"
                }
            })),
        ))
        .await;
    assert_eq!(st_add_missing, StatusCode::NOT_FOUND, "{b_add_missing}");

    let run_id = submit_boost_http_cmd(env, "boost.run", json!({}), "del-stale-run").await;
    let run_op = poll_op(env, &run_id).await;
    assert_eq!(run_op["state"], "failed", "run_op={run_op}");
}

fn mk_direct_plan(
    site_id: domain::SiteId,
    target_dl: domain::DownloaderId,
    purpose: downloader::SubmissionPurpose,
    url: String,
) -> api::FrozenDownloadPlan {
    api::FrozenDownloadPlan {
        torrent: domain::Torrent {
            site_id,
            id: Some("1".into()),
            title: "PreSend.Test.1080p".into(),
            enclosure: url.clone(),
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
        },
        source: downloader::TorrentSource::Http(downloader::TorrentHttpSource::get(url)),
        purpose,
        downloader_id: Some(target_dl),
        frozen_endpoint: None,
        subscribe_id: None,
        subscribe_generation: None,
        policy_revision: None,
        media_id: None,
        score: None,
        release_override: None,
        force: false,
        save_path: None,
        category: None,
        tags: vec![],
        paused: false,
        upload_limit: None,
        download_limit: None,
        sequential: false,
        first_last_piece: false,
        budget_key: (purpose == downloader::SubmissionPurpose::Boost)
            .then(|| format!("{BOOST_PLUGIN_ID}:{target_dl}")),
        plan_digest: None,
    }
}

#[tokio::test]
async fn pre_send_client_connect_failure_settles_without_leaking_boost_reservation() {
    use api::{ApiDownloadService, DownloadOpContext, DownloadSubmitter};
    use domain::{DownloaderId, PluginOperationId};
    use downloader::SubmissionPurpose;

    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(BoostTestDownloader::new(dir.path().join("stage")));
    let srv = StubTorrentServer::start_dynamic(2048);
    let (state, site_id, _) = setup_state(dir.path(), dl, Some(&srv));
    save_boost_config(&state, json!({ "budget_bytes": 100_000 }));

    let bad_dl_id = DownloaderId::new();
    state
        .store()
        .insert_downloader(&store::DownloaderRow {
            id: bad_dl_id,
            name: "UnreachableQB".into(),
            kind: "qbittorrent".into(),
            url: "http://127.0.0.1:9".into(),
            username: Some("admin".into()),
            password: Some("secret".into()),
            api_key: None,
            category: None,
            path_maps: vec![],
            is_default: false,
            enabled: true,
        })
        .unwrap();

    let op1 = PluginOperationId::new();
    let res1 = ApiDownloadService::new(state.clone())
        .submit(
            mk_direct_plan(
                site_id,
                bad_dl_id,
                SubmissionPurpose::Boost,
                srv.url("presend-fail.torrent"),
            ),
            DownloadOpContext {
                operation_id: op1,
                cause_id: "c1".into(),
                subject: format!("plugin:{BOOST_PLUGIN_ID}"),
                idempotency_key: "presend-1".into(),
                plugin_id: Some(BOOST_PLUGIN_ID.into()),
                generation: Some(1),
                config_revision: None,
                dry_run: false,
            },
        )
        .await;
    assert!(res1.is_err(), "unreachable client connect must fail");
    let rec1 = state
        .store()
        .get_download_operation(&op1.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(rec1.state, store::DownloadOperationState::Failed);
    let snap = state
        .store()
        .get_boost_snapshot_for_plugin(BOOST_PLUGIN_ID, &[], &[])
        .unwrap();
    assert_eq!(
        snap.reserved_bytes, 0,
        "Final boost reservation must not leak"
    );
}

struct DisablingFetcher(Arc<api::Store>, domain::DownloaderId);

impl api::download_service::fetcher::TorrentPayloadFetcher for DisablingFetcher {
    fn prepare(
        &self,
        source: &downloader::TorrentSource,
        purpose: downloader::SubmissionPurpose,
        policy: &net_guard::TargetPolicy,
    ) -> Result<downloader::PreparedTorrent, downloader::TorrentFetchError> {
        self.0
            .mutate_downloader(self.1, false, |r| {
                r.enabled = false;
                Ok(())
            })
            .unwrap();
        downloader::prepare_download_payload(
            &net_guard::NetGuardClient::new(),
            source,
            purpose,
            policy,
        )
    }
}

#[tokio::test]
async fn mid_fetch_downloader_disable_rejects_before_submitting() {
    use api::{ApiDownloadService, DownloadOpContext, DownloadSubmitter};
    use domain::PluginOperationId;
    use downloader::SubmissionPurpose;

    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(BoostTestDownloader::new(dir.path().join("stage")));
    let srv = StubTorrentServer::start_dynamic(2048);
    let (state, site_id, default_dl_id) = setup_state(dir.path(), dl, Some(&srv));
    let store = state.store().clone();
    let state2 =
        state.with_torrent_payload_fetcher(Arc::new(DisablingFetcher(store, default_dl_id)));
    let op2 = PluginOperationId::new();
    let res2 = ApiDownloadService::new(state2.clone())
        .submit(
            mk_direct_plan(
                site_id,
                default_dl_id,
                SubmissionPurpose::Subscribe,
                srv.url("mid-fetch-disable.torrent"),
            ),
            DownloadOpContext {
                operation_id: op2,
                cause_id: "c2".into(),
                subject: "sub:mid-disable".into(),
                idempotency_key: "mid-disable-1".into(),
                plugin_id: None,
                generation: None,
                config_revision: None,
                dry_run: false,
            },
        )
        .await;
    assert!(
        res2.is_err(),
        "mid-fetch disabled downloader must reject non-manual submit"
    );
    let rec2 = state2
        .store()
        .get_download_operation(&op2.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(rec2.state, store::DownloadOperationState::Failed);
}
