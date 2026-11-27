#[path = "builtin_boost/support.rs"]
mod support;
#[path = "boost_cleanup_safety/target_mismatch.rs"]
mod target_mismatch;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::{DownloaderId, Torrent};
use downloader::{
    ClientSubmitResult, DiskSpaceInfo, Downloader, DownloaderError, MemoryDownloader,
    SubmitOptions, SubmitPayload, TaskFileSet, TaskSnapshot,
};
use indexer::RegistrySnapshot;
use serde_json::{Value, json};
use store::{BoostTaskState, Db, PrepareBoostCleanupPlanInput};
use support::plugin_http_support::{HttpEnv, TOKEN, assert_err, assert_ok, req_json};
use support::{NoopFetcher, poll_op, seed_boost_task, seed_library_file_entry};

struct FailingTaskFilesDownloader {
    inner: MemoryDownloader,
    fail_task_files: AtomicBool,
}

impl FailingTaskFilesDownloader {
    fn new(stage: PathBuf) -> Self {
        Self {
            inner: MemoryDownloader::new(stage),
            fail_task_files: AtomicBool::new(false),
        }
    }
}

impl Downloader for FailingTaskFilesDownloader {
    fn remember_identity(&self, torrent: &Torrent, hash: &str) -> Result<(), DownloaderError> {
        self.inner.remember_identity(torrent, hash)
    }
    fn submit_verified(
        &self,
        payload: &SubmitPayload,
        options: &SubmitOptions,
    ) -> Result<ClientSubmitResult, DownloaderError> {
        self.inner.submit_verified(payload, options)
    }
    fn completed_files(&self, torrent: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        self.inner.completed_files(torrent)
    }
    fn delete_task(&self, info_hash: &str, delete_files: bool) -> Result<(), DownloaderError> {
        self.inner.delete_task(info_hash, delete_files)
    }
    fn pause_task(&self, info_hash: &str) -> Result<(), DownloaderError> {
        self.inner.pause_task(info_hash)
    }
    fn resume_task(&self, info_hash: &str) -> Result<(), DownloaderError> {
        self.inner.resume_task(info_hash)
    }
    fn task_snapshots(&self) -> Result<Vec<TaskSnapshot>, DownloaderError> {
        self.inner.task_snapshots()
    }
    fn task_files_by_hash(&self, info_hash: &str) -> Result<Option<TaskFileSet>, DownloaderError> {
        if self.fail_task_files.load(Ordering::SeqCst) {
            return Err(DownloaderError::Message("simulated rpc failure".into()));
        }
        self.inner.task_files_by_hash(info_hash)
    }
    fn set_task_tags(&self, info_hash: &str, tags: &[String]) -> Result<(), DownloaderError> {
        self.inner.set_task_tags(info_hash, tags)
    }
    fn set_task_category(&self, info_hash: &str, category: &str) -> Result<(), DownloaderError> {
        self.inner.set_task_category(info_hash, category)
    }
    fn set_task_limits(
        &self,
        info_hash: &str,
        upload_limit: Option<u64>,
        download_limit: Option<u64>,
    ) -> Result<(), DownloaderError> {
        self.inner
            .set_task_limits(info_hash, upload_limit, download_limit)
    }
    fn disk_free_space(&self) -> Result<DiskSpaceInfo, DownloaderError> {
        self.inner.disk_free_space()
    }
}

pub(crate) fn env_with_dl<D: Downloader + 'static>(
    dir: tempfile::TempDir,
    dl: Arc<D>,
) -> (HttpEnv, domain::SiteId, DownloaderId) {
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let state = ApiState::new_arc(
        store,
        TOKEN.into(),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl,
    )
    .unwrap();
    let app = router(state.clone());
    let env = HttpEnv {
        _dir: dir,
        state,
        app,
        member_token: String::new(),
    };
    support::install_cm_boost_plugin(env.state.store().as_ref());
    let site_id = env.insert_site("PT-Safety");
    let dl_id = DownloaderId::new();
    env.state
        .store()
        .insert_downloader(&store::DownloaderRow {
            id: dl_id,
            name: "SafetyDL".into(),
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
    (env, site_id, dl_id)
}

pub(crate) fn boost_rev(env: &HttpEnv) -> i64 {
    env.state
        .store()
        .get_plugin("cm.boost")
        .unwrap()
        .unwrap()
        .revision
}

pub(crate) async fn send_boost_cmd(
    env: &HttpEnv,
    token: &str,
    idem: &str,
    kind: &str,
    payload: Value,
) -> (StatusCode, Value) {
    let rev = boost_rev(env);
    let (st, _, body) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            Some(token),
            &[
                ("If-Match", &format!("\"{rev}\"")),
                ("Idempotency-Key", idem),
            ],
            Some(json!({ "kind": kind, "payload": payload })),
        ))
        .await;
    (st, body)
}

async fn run_boost_cmd(
    env: &HttpEnv,
    token: &str,
    idem: &str,
    kind: &str,
    payload: Value,
) -> Value {
    let (st, body) = send_boost_cmd(env, token, idem, kind, payload).await;
    let op = assert_ok(st, &body, StatusCode::ACCEPTED);
    poll_op(env, op["operation_id"].as_str().unwrap()).await
}

fn seed_pending_rename(store: &Store, from_path: &Path, to_path: &Path) {
    let lib_id = domain::LibraryId::new();
    let root_id = uuid::Uuid::new_v4().to_string();
    let file_id = uuid::Uuid::new_v4().to_string();
    let rename_id = uuid::Uuid::new_v4().to_string();
    let root_dir = from_path.parent().unwrap().to_string_lossy().into_owned();
    store
        .with_writer(Db::Library, |conn| {
            conn.execute(
                "INSERT INTO libraries (library_id, kind, name) VALUES (?1, 'movie', 'Movies')",
                [lib_id.to_string()],
            )?;
            conn.execute(
                "INSERT INTO library_roots (root_id, library_id, path_key, display_path) VALUES (?1, ?2, ?3, ?3)",
                rusqlite::params![root_id, lib_id.to_string(), root_dir],
            )?;
            let from_str = from_path.to_string_lossy().into_owned();
            let to_str = to_path.to_string_lossy().into_owned();
            conn.execute(
                "INSERT INTO pending_file_renames (
                    rename_id, file_id, from_path, from_path_key, to_path, to_path_key,
                    expected_revision, expected_source_version,
                    from_library_id, from_root_id, to_library_id, to_root_id, phase,
                    source_size, source_device, source_inode, source_mtime_ns,
                    from_config_revision, to_config_revision, created_at_ms, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?3, ?4, ?4, 1, 'v1', ?5, ?6, ?5, ?6, 'planned', 100, 1, 1, 1, 1, 1, 1, 1)",
                rusqlite::params![
                    rename_id,
                    file_id,
                    from_str,
                    to_str,
                    lib_id.to_string(),
                    root_id,
                ],
            )?;
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn admin_command_with_global_grant_and_downloader_target_succeeds() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let (env, site_id, dl_id) = env_with_dl(dir, dl);
    let tf = seed_boost_task(
        env.state.store().as_ref(),
        dl_id,
        site_id,
        "global-tgt",
        "aaaa000011111111111111111111111111111111",
        BoostTaskState::Seeding,
        1024,
        0,
        Some(false),
        None,
    );
    let rev = boost_rev(&env);
    let (st, _, body) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            Some(TOKEN),
            &[
                ("If-Match", &format!("\"{rev}\"")),
                ("Idempotency-Key", "cmd-tgt-1"),
            ],
            Some(json!({
                "kind": "downloader.control",
                "targets": [{ "kind": "downloader", "id": dl_id.to_string() }],
                "payload": { "task_id": tf, "action": "pause" }
            })),
        ))
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "body={body}");
}

async fn create_second_admin_token(env: &HttpEnv) -> String {
    let user = domain::User {
        id: domain::UserId::new(),
        login: "admin-b".into(),
        enabled: true,
        role: domain::UserRole::Admin,
    };
    let token = "admin-b-session-token".to_string();
    let store = env.state.store();
    store
        .insert_user_with_password(&user, "admin-b-password-1")
        .unwrap();
    store.set_user_token(user.id, &token).unwrap();
    token
}

#[tokio::test]
async fn admin_delete_files_rejects_from_plugin_and_binds_and_consumes_token() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let stage = dir.path().join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    let victim = stage.join("TokenBind.mkv");
    std::fs::write(&victim, b"secret-data").unwrap();
    let dl = Arc::new(MemoryDownloader::new(stage.clone()));
    let (env, site_id, dl_id) = env_with_dl(dir, dl);

    let tf_id = seed_boost_task(
        env.state.store().as_ref(),
        dl_id,
        site_id,
        "tok-bind",
        "bbbb000011111111111111111111111111111111",
        BoostTaskState::Seeding,
        11,
        0,
        Some(false),
        None,
    );
    let plan_json =
        json!({ "save_path": stage, "allowed_roots": [stage], "paths": [victim] }).to_string();
    let ret = env
        .state
        .store()
        .mark_boost_task_retained_with_paths(&tf_id, 1, 11, Some(&plan_json), 1_700_000_010)
        .unwrap();
    let planned = env
        .state
        .store()
        .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: tf_id,
            expected_revision: ret.revision,
            cleanup_plan_handle: "plan-tok-1".into(),
            cleanup_plan_json: plan_json,
            ttl_secs: 600,
            now_utc: api::job_loop::unix_now(),
        })
        .unwrap();

    let (st_p, _, b_p) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/cm.boost/brush-cleanup-preview?plan_handle=plan-tok-1",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let tok = assert_ok(st_p, &b_p, StatusCode::OK)["confirmation_token"]
        .as_str()
        .unwrap()
        .to_string();

    let (st_fp, b_fp) = send_boost_cmd(
        &env,
        TOKEN,
        "df-from-plugin",
        "downloader.delete-files",
        json!({ "host_file_plan_handle": "plan-tok-1", "revision": planned.revision, "confirmation_token": tok, "from_plugin": true }),
    )
    .await;
    assert_err(
        st_fp,
        &b_fp,
        StatusCode::BAD_REQUEST,
        "plugin.invalid_input",
    );
    assert!(victim.exists());

    let admin_b_tok = create_second_admin_token(&env).await;
    let (st_b, b_b) = send_boost_cmd(
        &env,
        &admin_b_tok,
        "df-admin-b",
        "downloader.delete-files",
        json!({ "host_file_plan_handle": "plan-tok-1", "revision": planned.revision, "confirmation_token": tok }),
    )
    .await;
    assert_eq!(
        st_b,
        StatusCode::CONFLICT,
        "cross-admin token must fail: {b_b}"
    );

    let done = run_boost_cmd(
        &env,
        TOKEN,
        "df-admin-a",
        "downloader.delete-files",
        json!({ "host_file_plan_handle": "plan-tok-1", "revision": planned.revision, "confirmation_token": tok }),
    )
    .await;
    assert_eq!(done["state"], "succeeded");
    assert!(!victim.exists());

    let (st_replay, _) = send_boost_cmd(
        &env,
        TOKEN,
        "df-replay",
        "downloader.delete-files",
        json!({ "host_file_plan_handle": "plan-tok-1", "revision": planned.revision, "confirmation_token": tok }),
    )
    .await;
    assert!(matches!(
        st_replay,
        StatusCode::FORBIDDEN | StatusCode::CONFLICT
    ));
}

#[tokio::test]
async fn delete_task_blocks_pending_file_renames_and_library_files() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let stage = dir.path().join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    let dl = Arc::new(MemoryDownloader::new(stage.clone()));
    let (env, site_id, dl_id) = env_with_dl(dir, dl.clone());

    let f_tr = stage.join("tr/ActiveTransfer.mkv");
    let f_lib = stage.join("lib/InLibrary.mkv");
    std::fs::create_dir_all(f_tr.parent().unwrap()).unwrap();
    std::fs::create_dir_all(f_lib.parent().unwrap()).unwrap();
    std::fs::write(&f_tr, b"tr").unwrap();
    std::fs::write(&f_lib, b"lib").unwrap();

    let h_tr = "cccc000011111111111111111111111111111111";
    let h_lib = "dddd000011111111111111111111111111111111";
    dl.submit_verified(
        &SubmitPayload::Magnet {
            uri: format!("magnet:?xt=urn:btih:{h_tr}"),
            info_hash: h_tr.into(),
        },
        &SubmitOptions {
            ownership_tag: "tr-tag".into(),
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
    dl.submit_verified(
        &SubmitPayload::Magnet {
            uri: format!("magnet:?xt=urn:btih:{h_lib}"),
            info_hash: h_lib.into(),
        },
        &SubmitOptions {
            ownership_tag: "lib-tag".into(),
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
    dl.set_task_files(h_tr, vec![f_tr.clone()], 100);
    dl.set_task_files(h_lib, vec![f_lib.clone()], 100);

    let tf_tr = seed_boost_task(
        env.state.store().as_ref(),
        dl_id,
        site_id,
        "tr-tag",
        h_tr,
        BoostTaskState::Seeding,
        100,
        0,
        Some(false),
        None,
    );
    let tf_lib = seed_boost_task(
        env.state.store().as_ref(),
        dl_id,
        site_id,
        "lib-tag",
        h_lib,
        BoostTaskState::Seeding,
        100,
        0,
        Some(false),
        None,
    );
    seed_pending_rename(env.state.store().as_ref(), &f_tr, &stage.join("Dest.mkv"));
    seed_library_file_entry(env.state.store().as_ref(), &f_lib);

    let op_tr = run_boost_cmd(
        &env,
        TOKEN,
        "dt-tr",
        "downloader.delete-task",
        json!({ "task_id": tf_tr }),
    )
    .await;
    assert_eq!(op_tr["state"], "rejected");
    assert_eq!(op_tr["phase"], "protection-denied:transfer-active");

    let op_lib = run_boost_cmd(
        &env,
        TOKEN,
        "dt-lib",
        "downloader.delete-task",
        json!({ "task_id": tf_lib }),
    )
    .await;
    assert_eq!(op_lib["state"], "rejected");
    assert_eq!(op_lib["phase"], "protection-denied:library-referenced");
    assert!(dl.deleted_tasks().is_empty());
}

#[tokio::test]
async fn delete_task_aborts_fail_closed_on_task_files_error_or_missing_task() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let stage = dir.path().join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    let dl = Arc::new(FailingTaskFilesDownloader::new(stage));
    let (env, site_id, dl_id) = env_with_dl(dir, dl.clone());

    let h_err = "eeee000011111111111111111111111111111111";
    let h_miss = "ffff000011111111111111111111111111111111";
    let tf_err = seed_boost_task(
        env.state.store().as_ref(),
        dl_id,
        site_id,
        "err-tag",
        h_err,
        BoostTaskState::Seeding,
        500,
        0,
        Some(false),
        None,
    );
    let tf_miss = seed_boost_task(
        env.state.store().as_ref(),
        dl_id,
        site_id,
        "miss-tag",
        h_miss,
        BoostTaskState::Seeding,
        500,
        0,
        Some(false),
        None,
    );

    dl.fail_task_files.store(true, Ordering::SeqCst);
    let op_err = run_boost_cmd(
        &env,
        TOKEN,
        "dt-err",
        "downloader.delete-task",
        json!({ "task_id": tf_err }),
    )
    .await;
    assert_eq!(op_err["state"], "failed");
    assert!(
        op_err["phase"]
            .as_str()
            .unwrap_or("")
            .starts_with("downloader-task-files-failed:")
    );
    assert_eq!(
        env.state
            .store()
            .get_boost_task_fact(&tf_err)
            .unwrap()
            .unwrap()
            .state,
        BoostTaskState::Seeding
    );

    dl.fail_task_files.store(false, Ordering::SeqCst);
    let op_miss = run_boost_cmd(
        &env,
        TOKEN,
        "dt-miss",
        "downloader.delete-task",
        json!({ "task_id": tf_miss }),
    )
    .await;
    assert_eq!(op_miss["state"], "rejected");
    assert_eq!(op_miss["phase"], "downloader-task-missing");
    assert!(dl.inner.deleted_tasks().is_empty());
}

#[tokio::test]
async fn prepare_cleanup_and_delete_files_reject_empty_paths() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let stage = dir.path().join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    let dl = Arc::new(MemoryDownloader::new(stage.clone()));
    let (env, site_id, dl_id) = env_with_dl(dir, dl);

    let tf_ret = seed_boost_task(
        env.state.store().as_ref(),
        dl_id,
        site_id,
        "empty-ret",
        "1234000011111111111111111111111111111111",
        BoostTaskState::Seeding,
        9999,
        0,
        Some(false),
        None,
    );
    let empty_json =
        json!({ "save_path": stage, "allowed_roots": [stage], "paths": [] }).to_string();
    env.state
        .store()
        .mark_boost_task_retained_with_paths(&tf_ret, 1, 9999, Some(&empty_json), 1_700_000_010)
        .unwrap();

    let prep_op = run_boost_cmd(
        &env,
        TOKEN,
        "prep-empty",
        "downloader.prepare-cleanup",
        json!({ "task_ids": [tf_ret] }),
    )
    .await;
    assert_eq!(prep_op["state"], "rejected");
    assert_eq!(prep_op["phase"], "manual-cleanup-required");

    let tf_plan = seed_boost_task(
        env.state.store().as_ref(),
        dl_id,
        site_id,
        "empty-plan",
        "5678000011111111111111111111111111111111",
        BoostTaskState::Seeding,
        9999,
        0,
        Some(false),
        None,
    );
    let ret2 = env
        .state
        .store()
        .mark_boost_task_retained_with_paths(&tf_plan, 1, 9999, Some(&empty_json), 1_700_000_010)
        .unwrap();
    env.state
        .store()
        .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: tf_plan.clone(),
            expected_revision: ret2.revision,
            cleanup_plan_handle: "plan-empty-1".into(),
            cleanup_plan_json: empty_json,
            ttl_secs: 600,
            now_utc: api::job_loop::unix_now(),
        })
        .unwrap();

    let (st_prev, _, b_prev) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/cm.boost/brush-cleanup-preview?plan_handle=plan-empty-1",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_err(
        st_prev,
        &b_prev,
        StatusCode::CONFLICT,
        "plugin.cleanup_blocked",
    );
    assert!(
        b_prev["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("manual-cleanup-required")
    );
    assert_eq!(
        env.state
            .store()
            .get_boost_task_fact(&tf_plan)
            .unwrap()
            .unwrap()
            .retained_bytes,
        9999
    );
}

#[tokio::test]
async fn prepare_cleanup_rejects_multi_target_http() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let stage = dir.path().join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    let dl = Arc::new(MemoryDownloader::new(stage.clone()));
    let (env, site_id, dl_id) = env_with_dl(dir, dl);

    let f1 = stage.join("M1.mkv");
    let f2 = stage.join("M2.mkv");
    std::fs::write(&f1, b"1").unwrap();
    std::fs::write(&f2, b"2").unwrap();
    let t1 = seed_boost_task(
        env.state.store().as_ref(),
        dl_id,
        site_id,
        "multi-1",
        "9999000011111111111111111111111111111111",
        BoostTaskState::Seeding,
        100,
        0,
        Some(false),
        None,
    );
    let t2 = seed_boost_task(
        env.state.store().as_ref(),
        dl_id,
        site_id,
        "multi-2",
        "8888000011111111111111111111111111111111",
        BoostTaskState::Seeding,
        100,
        0,
        Some(false),
        None,
    );
    env.state
        .store()
        .mark_boost_task_retained_with_paths(
            &t1,
            1,
            100,
            Some(&json!({"paths":[f1]}).to_string()),
            1_700_000_010,
        )
        .unwrap();
    env.state
        .store()
        .mark_boost_task_retained_with_paths(
            &t2,
            1,
            100,
            Some(&json!({"paths":[f2]}).to_string()),
            1_700_000_010,
        )
        .unwrap();

    let (st, body) = send_boost_cmd(
        &env,
        TOKEN,
        "prep-multi",
        "downloader.prepare-cleanup",
        json!({ "task_ids": [t1, t2] }),
    )
    .await;
    assert_err(st, &body, StatusCode::BAD_REQUEST, "plugin.invalid_input");
    assert_eq!(
        env.state
            .store()
            .get_boost_task_fact(&t1)
            .unwrap()
            .unwrap()
            .state,
        BoostTaskState::RetainedOnly
    );
    assert_eq!(
        env.state
            .store()
            .get_boost_task_fact(&t2)
            .unwrap()
            .unwrap()
            .state,
        BoostTaskState::RetainedOnly
    );
}
