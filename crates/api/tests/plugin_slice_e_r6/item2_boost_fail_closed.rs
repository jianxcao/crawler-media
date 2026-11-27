use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::{DownloaderId, SiteId, Torrent};
use downloader::{
    ClientSubmitResult, DiskSpaceInfo, Downloader, DownloaderError, MemoryDownloader,
    SubmitOptions, SubmitPayload, TaskFileSet, TaskSnapshot,
};
use indexer::RegistrySnapshot;
use serde_json::{Value, json};
use store::{BoostTaskState, PrepareBoostCleanupPlanInput};

use crate::support::plugin_http_support::{HttpEnv, TOKEN, assert_ok, req_json};
use crate::support::{NoopFetcher, poll_op, seed_boost_task};

struct FailingSnapshotsDownloader {
    inner: MemoryDownloader,
    fail_snapshots: AtomicBool,
}

impl FailingSnapshotsDownloader {
    fn new(stage: PathBuf) -> Self {
        Self {
            inner: MemoryDownloader::new(stage),
            fail_snapshots: AtomicBool::new(false),
        }
    }
}

impl Downloader for FailingSnapshotsDownloader {
    fn remember_identity(&self, t: &Torrent, h: &str) -> Result<(), DownloaderError> {
        self.inner.remember_identity(t, h)
    }
    fn submit_verified(
        &self,
        p: &SubmitPayload,
        o: &SubmitOptions,
    ) -> Result<ClientSubmitResult, DownloaderError> {
        self.inner.submit_verified(p, o)
    }
    fn completed_files(&self, t: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        self.inner.completed_files(t)
    }
    fn delete_task(&self, h: &str, df: bool) -> Result<(), DownloaderError> {
        self.inner.delete_task(h, df)
    }
    fn pause_task(&self, h: &str) -> Result<(), DownloaderError> {
        self.inner.pause_task(h)
    }
    fn resume_task(&self, h: &str) -> Result<(), DownloaderError> {
        self.inner.resume_task(h)
    }
    fn task_snapshots(&self) -> Result<Vec<TaskSnapshot>, DownloaderError> {
        if self.fail_snapshots.load(Ordering::SeqCst) {
            return Err(DownloaderError::Message("torrents/info rpc 500".into()));
        }
        self.inner.task_snapshots()
    }
    fn task_files_by_hash(&self, h: &str) -> Result<Option<TaskFileSet>, DownloaderError> {
        self.inner.task_files_by_hash(h)
    }
    fn disk_free_space(&self) -> Result<DiskSpaceInfo, DownloaderError> {
        self.inner.disk_free_space()
    }
}

async fn run_boost_cmd(env: &HttpEnv, idem: &str, kind: &str, payload: Value) -> Value {
    let rev = env.get_plugin_rev("builtin.boost").await;
    let (st, _, body) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/builtin.boost/commands",
            Some(TOKEN),
            &[
                ("If-Match", &format!("\"{rev}\"")),
                ("Idempotency-Key", idem),
            ],
            Some(json!({ "kind": kind, "payload": payload })),
        ))
        .await;
    let op = assert_ok(st, &body, StatusCode::ACCEPTED);
    poll_op(env, op["operation_id"].as_str().unwrap()).await
}

fn seed_planned_victim(
    store: &Store,
    dl_id: DownloaderId,
    site_id: SiteId,
    victim: &Path,
    tag: &str,
    hash: &str,
    handle: &str,
) -> (String, i64) {
    let stage = victim.parent().unwrap();
    let tf = seed_boost_task(
        store,
        dl_id,
        site_id,
        tag,
        hash,
        BoostTaskState::Seeding,
        64,
        0,
        Some(false),
        None,
    );
    let pjson =
        json!({ "save_path": stage, "allowed_roots": [stage], "paths": [victim] }).to_string();
    let ret = store
        .mark_boost_task_retained_with_paths(&tf, 1, 64, Some(&pjson), 1_700_000_010)
        .unwrap();
    let planned = store
        .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: tf.clone(),
            expected_revision: ret.revision,
            cleanup_plan_handle: handle.into(),
            cleanup_plan_json: pjson,
            ttl_secs: 600,
            now_utc: api::job_loop::unix_now(),
        })
        .unwrap();
    (tf, planned.revision)
}

#[tokio::test]
async fn slice_e_r6_item2_fail_closed_on_enabled_downloader_connect_and_task_snapshots_errors() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let stage = dir.path().join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    let victim_snap = stage.join("SharedSnap.mkv");
    let victim_unreach = stage.join("SharedUnreach.mkv");
    std::fs::write(&victim_snap, b"keep-snap").unwrap();
    std::fs::write(&victim_unreach, b"keep-unreach").unwrap();

    let dl = Arc::new(FailingSnapshotsDownloader::new(stage.clone()));
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let state = ApiState::new_arc(
        store,
        TOKEN.into(),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl.clone(),
    )
    .unwrap();
    let env = HttpEnv {
        _dir: dir,
        app: router(state.clone()),
        state,
        member_token: String::new(),
    };
    let site_id = env.insert_site("PT-R6");
    let dl_def = DownloaderId::new();
    env.state
        .store()
        .insert_downloader(&store::DownloaderRow {
            id: dl_def,
            name: "DefaultDL".into(),
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

    let (_, snap_rev) = seed_planned_victim(
        env.state.store().as_ref(),
        dl_def,
        site_id,
        &victim_snap,
        "r6-snap-err",
        "1010101010101010101010101010101010101010",
        "r6-snap-plan",
    );
    let (st_tok, _, b_tok) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/builtin.boost/brush-cleanup-preview?plan_handle=r6-snap-plan",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let tok = assert_ok(st_tok, &b_tok, StatusCode::OK)["confirmation_token"]
        .as_str()
        .unwrap()
        .to_string();

    dl.fail_snapshots.store(true, Ordering::SeqCst);
    let (st_prev_err, _, _) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/builtin.boost/brush-cleanup-preview?plan_handle=r6-snap-plan",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_ne!(st_prev_err, StatusCode::OK);

    let del_snap = run_boost_cmd(
        &env,
        "r6-del-snap-err",
        "downloader.delete-files",
        json!({
            "host_file_plan_handle": "r6-snap-plan",
            "revision": snap_rev,
            "confirmation_token": tok,
        }),
    )
    .await;
    assert_eq!(del_snap["state"], "failed", "del_snap={del_snap}");
    assert_eq!(del_snap["phase"], "read-protection-failed");
    assert!(
        victim_snap.exists(),
        "shared file must NOT be unlinked on task_snapshots RPC failure"
    );

    dl.fail_snapshots.store(false, Ordering::SeqCst);
    assert_unreachable_enabled_downloader_fails_closed(&env, site_id, &stage, &victim_unreach)
        .await;
}

async fn assert_unreachable_enabled_downloader_fails_closed(
    env: &HttpEnv,
    site_id: SiteId,
    stage: &Path,
    victim_unreach: &Path,
) {
    let dl_live = env.insert_downloader("SecondEnabledUnreachableQbit");
    let tf_ret = seed_boost_task(
        env.state.store().as_ref(),
        dl_live,
        site_id,
        "r6-unreach-ret",
        "2020202020202020202020202020202020202020",
        BoostTaskState::Seeding,
        64,
        0,
        Some(false),
        None,
    );
    let pjson = json!({
        "save_path": stage,
        "allowed_roots": [stage],
        "paths": [victim_unreach],
    })
    .to_string();
    env.state
        .store()
        .mark_boost_task_retained_with_paths(&tf_ret, 1, 64, Some(&pjson), 1_700_000_020)
        .unwrap();

    let prep_unreach = run_boost_cmd(
        env,
        "r6-prep-unreach",
        "downloader.prepare-cleanup",
        json!({ "task_ids": [tf_ret] }),
    )
    .await;
    assert_eq!(
        prep_unreach["state"], "failed",
        "prep_unreach={prep_unreach}"
    );
    assert_eq!(prep_unreach["phase"], "read-protection-failed");
    assert!(victim_unreach.exists());

    env.state
        .store()
        .save_downloader(&store::DownloaderRow {
            id: dl_live,
            name: "SecondEnabledUnreachableQbit".into(),
            kind: "qbittorrent".into(),
            url: "http://127.0.0.1:9".into(),
            username: None,
            password: None,
            api_key: None,
            category: None,
            path_maps: vec![],
            is_default: false,
            enabled: false,
        })
        .unwrap();
    let (st_dis, _, _) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/builtin.boost/brush-cleanup-preview?task_ids={tf_ret}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_eq!(st_dis, StatusCode::OK);
}
