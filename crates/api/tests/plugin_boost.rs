#[path = "builtin_boost/support.rs"]
mod boost_support;
#[path = "support/writer_hold.rs"]
mod writer_hold;

use boost_support::plugin_http_support;

use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use api::{ApiState, Store};
use axum::http::StatusCode;
use domain::DownloaderId;
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
use plugin_http_support::{HttpEnv, TOKEN, assert_err, assert_ok, req_json};
use serde_json::{Value, json};
use store::Db;

struct MockTorrentFetcher;

impl Fetcher for MockTorrentFetcher {
    fn fetch(&self, req: &FetchRequest) -> Result<String, IndexerError> {
        let _ = req;
        Ok("<html><body><table class='torrents'></table></body></html>".into())
    }
}

fn sample_torrent_bytes(name: &str, length: usize) -> Vec<u8> {
    let info = format!(
        "d6:lengthi{length}e4:name{}:{name}12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaae",
        name.len()
    );
    format!("d8:announce16:http://127.0.0.14:info{info}e").into_bytes()
}

fn start_torrent_server(
    bytes: Vec<u8>,
) -> (
    String,
    std::net::SocketAddr,
    std::thread::JoinHandle<()>,
    Arc<std::sync::atomic::AtomicBool>,
) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_flag = stop.clone();
    let handle = std::thread::spawn(move || {
        while !stop_flag.load(std::sync::atomic::Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let mut buf = [0u8; 2048];
                    let _ = std::io::Read::read(&mut stream, &mut buf);
                    let resp_hdr = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/x-bittorrent\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        bytes.len()
                    );
                    let _ = stream.write_all(resp_hdr.as_bytes());
                    let _ = stream.write_all(&bytes);
                    let _ = stream.flush();
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });
    (
        format!("http://{addr}/download.torrent"),
        addr,
        handle,
        stop,
    )
}

async fn poll_plugin_op(env: &HttpEnv, op_id: &str) -> Value {
    for _ in 0..80 {
        let _ = env
            .send(req_json(
                "POST",
                "/api/v1/jobs/tick",
                Some(TOKEN),
                &[],
                None,
            ))
            .await;
        let (st, _, body) = env
            .send(req_json(
                "GET",
                &format!("/api/v1/plugin-operations/{op_id}"),
                Some(TOKEN),
                &[],
                None,
            ))
            .await;
        let data = assert_ok(st, &body, StatusCode::OK);
        let state_str = data["state"].as_str().unwrap_or("");
        if matches!(state_str, "succeeded" | "rejected" | "failed" | "cancelled") {
            return data;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("plugin operation {op_id} timed out");
}

async fn verify_boost_config_and_reenable(env: &HttpEnv, site_id: domain::SiteId, rev: i64) {
    let (st_dis, _, b_dis) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.boost/disable",
            Some(TOKEN),
            &[
                ("If-Match", &format!("\"{rev}\"")),
                ("Idempotency-Key", "boost-dis-1"),
            ],
            Some(json!({})),
        ))
        .await;
    let dis_op = assert_ok(st_dis, &b_dis, StatusCode::ACCEPTED);
    poll_plugin_op(env, dis_op["operation_id"].as_str().unwrap()).await;

    let (st_cfg_get, _, b_cfg_get) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/cm.boost/config",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let cfg_view = assert_ok(st_cfg_get, &b_cfg_get, StatusCode::OK);
    let schema_rev = cfg_view["schema_revision"].as_str().unwrap();

    let (_, _, b_p2) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/cm.boost",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let rev2 = assert_ok(StatusCode::OK, &b_p2, StatusCode::OK)["revision"]
        .as_i64()
        .unwrap();

    let (st_put, _, b_put) = env
        .send(req_json(
            "PUT",
            "/api/v1/plugins/cm.boost/config",
            Some(TOKEN),
            &[
                ("If-Match", &format!("\"{rev2}\"")),
                ("Idempotency-Key", "boost-cfg-1"),
            ],
            Some(json!({
                "schema_revision": schema_rev,
                "values": {
                    "sites": [site_id.to_string()],
                    "budget_bytes": 1048576,
                    "dry_run": true
                },
                "secrets": {}
            })),
        ))
        .await;
    assert_ok(st_put, &b_put, StatusCode::OK);

    let (st_p3, _, b_p3) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/cm.boost",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let rev3 = assert_ok(st_p3, &b_p3, StatusCode::OK)["revision"]
        .as_i64()
        .unwrap();

    let (st_en, _, b_en) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.boost/enable",
            Some(TOKEN),
            &[
                ("If-Match", &format!("\"{rev3}\"")),
                ("Idempotency-Key", "boost-en-1"),
            ],
            Some(json!({})),
        ))
        .await;
    let en_op = assert_ok(st_en, &b_en, StatusCode::ACCEPTED);
    let en_done = poll_plugin_op(env, en_op["operation_id"].as_str().unwrap()).await;
    assert_eq!(en_done["state"], "succeeded");
}

#[tokio::test]
async fn legacy_boost_routes_return_404_and_builtin_boost_lifecycle_roundtrip() {
    let env = HttpEnv::new().await;
    boost_support::install_cm_boost_plugin(env.state.store().as_ref());
    let site_id = env.insert_site("PT-Legacy");

    let (st_stats, _, _) = env
        .send(req_json(
            "GET",
            "/api/v1/sites/boost-stats",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_eq!(st_stats, StatusCode::NOT_FOUND);

    let patch_resp = tower::ServiceExt::oneshot(
        env.app.clone(),
        req_json(
            "PATCH",
            &format!("/api/v1/sites/{site_id}/boost"),
            Some(TOKEN),
            &[],
            Some(json!({ "enabled": true })),
        ),
    )
    .await
    .unwrap();
    assert_eq!(patch_resp.status(), StatusCode::NOT_FOUND);

    let (st_get, _, b_get) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/cm.boost",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let plugin = assert_ok(st_get, &b_get, StatusCode::OK);
    assert_eq!(plugin["lifecycle"], "enabled");
    let rev = plugin["revision"].as_i64().unwrap();
    verify_boost_config_and_reenable(&env, site_id, rev).await;
}

#[tokio::test]
async fn brush_state_and_cleanup_preview_contracts_and_lock_hygiene() {
    let env = HttpEnv::new().await;
    boost_support::install_cm_boost_plugin(env.state.store().as_ref());
    let dl_id = DownloaderId::new();
    env.state
        .store()
        .insert_downloader(&store::DownloaderRow {
            id: dl_id,
            name: "MemDL".into(),
            kind: "qbittorrent".into(),
            url: "http://127.0.0.1:9999".into(),
            username: None,
            password: None,
            api_key: None,
            category: None,
            path_maps: vec![],
            is_default: true,
            enabled: true,
        })
        .unwrap();

    // Auth / RBAC checks
    let (st_unauth, _, _) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/cm.boost/brush-state",
            None,
            &[],
            None,
        ))
        .await;
    assert_eq!(st_unauth, StatusCode::UNAUTHORIZED);

    let (st_mem, _, _) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/cm.boost/brush-state",
            Some(&env.member_token),
            &[],
            None,
        ))
        .await;
    assert_eq!(st_mem, StatusCode::FORBIDDEN);

    // 400 on invalid query params for brush-cleanup-preview
    for bad_uri in [
        "/api/v1/plugins/cm.boost/brush-cleanup-preview",
        "/api/v1/plugins/cm.boost/brush-cleanup-preview?task_ids=t1&plan_handle=p1",
        "/api/v1/plugins/cm.boost/brush-cleanup-preview?unknown=1",
    ] {
        let (st, _, body) = env
            .send(req_json("GET", bad_uri, Some(TOKEN), &[], None))
            .await;
        assert_err(st, &body, StatusCode::BAD_REQUEST, "plugin.invalid_input");
    }

    // Non-blocking ReaderPool check while Db::Subscribe writer lock is held
    let hold_store = env.state.store();
    let (tx, rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = writer_hold::WriterHold::acquire(hold_store, Db::Subscribe);
        tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(350));
    });
    rx.recv_timeout(Duration::from_secs(2)).unwrap();

    let started = Instant::now();
    let (st_ok, _, b_ok) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/cm.boost/brush-state",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert!(
        started.elapsed() < Duration::from_millis(200),
        "brush-state must not block on Subscribe writer lock"
    );
    holder.join().unwrap();
    let state_data = assert_ok(st_ok, &b_ok, StatusCode::OK);
    assert_eq!(state_data["used_bytes"], 0);
    assert_eq!(state_data["reserved_bytes"], 0);
    assert_eq!(state_data["retained_file_bytes"], 0);
    assert!(state_data["disk"]["status"].is_string());
}

struct BoostCtx<'a> {
    env: &'a HttpEnv,
    site_id: domain::SiteId,
    dl_id: DownloaderId,
    rev: i64,
    staged_file: &'a std::path::Path,
}

async fn run_delete_task_phase(
    ctx: &BoostCtx<'_>,
    dl: &MemoryDownloader,
    tid: &str,
    info_hash: &str,
) {
    let (st_for, _, b_for) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            Some(TOKEN),
            &[
                ("If-Match", &format!("\"{}\"", ctx.rev)),
                ("Idempotency-Key", "cmd-foreign-1"),
            ],
            Some(json!({
                "kind": "downloader.delete-task",
                "payload": { "info_hash": "e".repeat(40) }
            })),
        ))
        .await;
    assert!(matches!(
        st_for,
        StatusCode::BAD_REQUEST
            | StatusCode::FORBIDDEN
            | StatusCode::NOT_FOUND
            | StatusCode::CONFLICT
    ));
    assert_ne!(b_for["error"]["code"], "not_found");

    let (st_dt, _, b_dt) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            Some(TOKEN),
            &[
                ("If-Match", &format!("\"{}\"", ctx.rev)),
                ("Idempotency-Key", "cmd-del-task-1"),
            ],
            Some(json!({
                "kind": "downloader.delete-task",
                "payload": { "task_id": tid }
            })),
        ))
        .await;
    let dt_op = assert_ok(st_dt, &b_dt, StatusCode::ACCEPTED);
    let dt_done = poll_plugin_op(ctx.env, dt_op["operation_id"].as_str().unwrap()).await;
    assert_eq!(dt_done["state"], "succeeded");
    assert_eq!(dl.deleted_tasks(), vec![(info_hash.to_string(), false)]);
    assert!(
        ctx.staged_file.exists(),
        "delete-task must NEVER delete files"
    );

    let (st_bs2, _, b_bs2) = ctx
        .env
        .send(req_json(
            "GET",
            &format!(
                "/api/v1/plugins/cm.boost/brush-state?site_id={}&downloader_id={}",
                ctx.site_id, ctx.dl_id
            ),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let bs2 = assert_ok(st_bs2, &b_bs2, StatusCode::OK);
    assert_eq!(bs2["retained_file_bytes"], 8192);
}

async fn run_cleanup_plan_and_delete_phase(ctx: &BoostCtx<'_>, tid: &str) {
    let (st_sel, _, b_sel) = ctx
        .env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/cm.boost/brush-cleanup-preview?task_ids={tid}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let sel = assert_ok(st_sel, &b_sel, StatusCode::OK);
    assert_eq!(sel["mode"], "selection");
    assert!(sel["confirmation_token"].is_null());

    let (st_prep, _, b_prep) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            Some(TOKEN),
            &[
                ("If-Match", &format!("\"{}\"", ctx.rev)),
                ("Idempotency-Key", "cmd-prep-1"),
            ],
            Some(json!({
                "kind": "downloader.prepare-cleanup",
                "payload": {
                    "site_id": ctx.site_id.to_string(),
                    "downloader_id": ctx.dl_id.to_string(),
                    "task_ids": [tid]
                }
            })),
        ))
        .await;
    let prep_op = assert_ok(st_prep, &b_prep, StatusCode::ACCEPTED);
    let prep_done = poll_plugin_op(ctx.env, prep_op["operation_id"].as_str().unwrap()).await;
    assert_eq!(prep_done["state"], "succeeded");
    let plan_handle = prep_done["result"]["plan_handle"]
        .as_str()
        .unwrap()
        .to_string();

    let (st_plan, _, b_plan) = ctx
        .env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/cm.boost/brush-cleanup-preview?plan_handle={plan_handle}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let plan = assert_ok(st_plan, &b_plan, StatusCode::OK);
    assert_eq!(plan["mode"], "plan");
    let tok = plan["confirmation_token"].as_str().unwrap().to_string();
    let plan_rev = plan["revision"].as_i64().unwrap();
    run_delete_files_phase(ctx, &plan_handle, plan_rev, &tok).await;
}

async fn run_delete_files_phase(ctx: &BoostCtx<'_>, plan_handle: &str, plan_rev: i64, tok: &str) {
    let (st_notok, _, _) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            Some(TOKEN),
            &[
                ("If-Match", &format!("\"{}\"", ctx.rev)),
                ("Idempotency-Key", "cmd-df-notok"),
            ],
            Some(json!({
                "kind": "downloader.delete-files",
                "payload": {
                    "host_file_plan_handle": plan_handle,
                    "revision": plan_rev,
                    "reason": "missing token"
                }
            })),
        ))
        .await;
    assert!(matches!(
        st_notok,
        StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN | StatusCode::CONFLICT
    ));
    assert!(ctx.staged_file.exists());

    let (st_df, _, b_df) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            Some(TOKEN),
            &[
                ("If-Match", &format!("\"{}\"", ctx.rev)),
                ("Idempotency-Key", "cmd-df-ok"),
            ],
            Some(json!({
                "kind": "downloader.delete-files",
                "payload": {
                    "host_file_plan_handle": plan_handle,
                    "revision": plan_rev,
                    "confirmation_token": tok,
                    "reason": "cleanup verified"
                }
            })),
        ))
        .await;
    let df_op = assert_ok(st_df, &b_df, StatusCode::ACCEPTED);
    let df_done = poll_plugin_op(ctx.env, df_op["operation_id"].as_str().unwrap()).await;
    assert_eq!(df_done["state"], "succeeded");
    assert!(!ctx.staged_file.exists(), "physical file must be deleted");

    let (st_bs3, _, b_bs3) = ctx
        .env
        .send(req_json(
            "GET",
            &format!(
                "/api/v1/plugins/cm.boost/brush-state?site_id={}&downloader_id={}",
                ctx.site_id, ctx.dl_id
            ),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let bs3 = assert_ok(st_bs3, &b_bs3, StatusCode::OK);
    assert_eq!(bs3["retained_file_bytes"], 0);
}

async fn run_add_command_phase(
    env: &HttpEnv,
    site_id: domain::SiteId,
    dl_id: DownloaderId,
    dl_url: &str,
    rev: i64,
) -> String {
    let (st_add, _, b_add) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            Some(TOKEN),
            &[
                ("If-Match", &format!("\"{rev}\"")),
                ("Idempotency-Key", "cmd-add-1"),
            ],
            Some(json!({
                "kind": "downloader.add",
                "payload": {
                    "downloader_id": dl_id.to_string(),
                    "site_id": site_id.to_string(),
                    "download_url": dl_url,
                    "title": "Boost.Target.2026"
                }
            })),
        ))
        .await;
    let add_op = assert_ok(st_add, &b_add, StatusCode::ACCEPTED);
    let add_done = poll_plugin_op(env, add_op["operation_id"].as_str().unwrap()).await;
    assert_eq!(add_done["state"], "succeeded");
    let (st_bs, _, b_bs) = env
        .send(req_json(
            "GET",
            &format!(
                "/api/v1/plugins/cm.boost/brush-state?site_id={site_id}&downloader_id={dl_id}"
            ),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let bs = assert_ok(st_bs, &b_bs, StatusCode::OK);
    assert_eq!(bs["used_bytes"], 8192);
    bs["tasks"][0]["info_hash"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn boost_downloader_commands_cleanup_and_protections_end_to_end() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let tbytes = sample_torrent_bytes("Boost.Target.2026", 8192);
    let (dl_url, srv_addr, srv_handle, stop) = start_torrent_server(tbytes);
    let state = ApiState::new_arc(
        Arc::new(store),
        TOKEN.into(),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(MockTorrentFetcher),
        dl.clone(),
    )
    .unwrap()
    .with_plugin_http_policy_override(
        net_guard::TargetPolicy::production().with_test_loopback_socket(srv_addr),
    );
    let env = HttpEnv {
        _dir: dir,
        app: api::router(state.clone()),
        state,
        member_token: String::new(),
    };
    boost_support::install_cm_boost_plugin(env.state.store().as_ref());
    let site_id = env.insert_site("PT-Boost");
    let dl_id = DownloaderId::new();
    env.state
        .store()
        .insert_downloader(&store::DownloaderRow {
            id: dl_id,
            name: "MemDL".into(),
            kind: "qbittorrent".into(),
            url: "http://127.0.0.1:9999".into(),
            username: None,
            password: None,
            api_key: None,
            category: None,
            path_maps: vec![],
            is_default: true,
            enabled: true,
        })
        .unwrap();

    let (st_p, _, b_p) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/cm.boost",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let rev = assert_ok(st_p, &b_p, StatusCode::OK)["revision"]
        .as_i64()
        .unwrap();

    let info_hash = run_add_command_phase(&env, site_id, dl_id, &dl_url, rev).await;
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = srv_handle.join();

    let staged_file = env
        .state
        .store()
        .data_dir()
        .parent()
        .unwrap()
        .join("stage/Boost.Target.2026.mkv");
    std::fs::write(&staged_file, vec![1u8; 8192]).unwrap();
    dl.set_task_files(&info_hash, vec![staged_file.clone()], 8192);

    let (st_tasks, _, b_tasks) = env
        .send(req_json(
            "GET",
            "/api/v1/downloaders/tasks",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let tasks_data = assert_ok(st_tasks, &b_tasks, StatusCode::OK);
    let tid = tasks_data["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["info_hash"].as_str() == Some(info_hash.as_str()))
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let ctx = BoostCtx {
        env: &env,
        site_id,
        dl_id,
        rev,
        staged_file: &staged_file,
    };
    run_delete_task_phase(&ctx, &dl, &tid, &info_hash).await;
    run_cleanup_plan_and_delete_phase(&ctx, &tid).await;
}
