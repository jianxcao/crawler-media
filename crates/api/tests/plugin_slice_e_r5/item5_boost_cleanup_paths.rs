use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::http::StatusCode;
use domain::Torrent;
use downloader::{
    ClientSubmitResult, DiskSpaceInfo, Downloader, DownloaderError, MemoryDownloader, PathMap,
    SubmitOptions, SubmitPayload, TaskFileSet, TaskSnapshot,
};
use serde_json::json;
use store::{BoostTaskState, PrepareBoostCleanupPlanInput};

use crate::support::plugin_http_support::{HttpEnv, TOKEN, assert_ok, req_json};
use crate::support::{seed_boost_task, seed_library_file_entry};
use crate::{env_with_dl_maps, run_boost_cmd};

struct ContainerMappedDownloader {
    inner: MemoryDownloader,
    container_save_path: String,
    fail_files_for_hash: parking_lot::Mutex<Option<String>>,
    force_fail_files: AtomicBool,
}

impl ContainerMappedDownloader {
    fn new(host_stage: PathBuf, container_save_path: &str) -> Self {
        Self {
            inner: MemoryDownloader::new(host_stage),
            container_save_path: container_save_path.into(),
            fail_files_for_hash: parking_lot::Mutex::new(None),
            force_fail_files: AtomicBool::new(false),
        }
    }
}

impl Downloader for ContainerMappedDownloader {
    fn remember_identity(&self, t: &Torrent, h: &str) -> Result<(), DownloaderError> {
        self.inner.remember_identity(t, h)
    }
    fn submit_verified(
        &self,
        payload: &SubmitPayload,
        options: &SubmitOptions,
    ) -> Result<ClientSubmitResult, DownloaderError> {
        self.inner.submit_verified(payload, options)
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
        self.inner.task_snapshots()
    }
    fn task_files_by_hash(&self, info_hash: &str) -> Result<Option<TaskFileSet>, DownloaderError> {
        if self.force_fail_files.load(Ordering::SeqCst)
            && self
                .fail_files_for_hash
                .lock()
                .as_deref()
                .is_none_or(|target| target.eq_ignore_ascii_case(info_hash))
        {
            return Err(DownloaderError::Message("transient rpc error".into()));
        }
        let Some(mut fset) = self.inner.task_files_by_hash(info_hash)? else {
            return Ok(None);
        };
        fset.save_path = self.container_save_path.clone();
        Ok(Some(fset))
    }
    fn disk_free_space(&self) -> Result<DiskSpaceInfo, DownloaderError> {
        self.inner.disk_free_space()
    }
}

#[tokio::test]
async fn slice_e_r5_item5a_mapped_task_files_ancestor_siblings_and_fail_closed_rpc() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let stage = dir.path().join("host_stage");
    let pack_dir = stage.join("SeasonPack");
    std::fs::create_dir_all(&pack_dir).unwrap();
    let lib_ep = pack_dir.join("LibEp01.mkv");
    let sib_ep = pack_dir.join("SibEp02.mkv");
    std::fs::write(&lib_ep, b"library-bytes").unwrap();
    std::fs::write(&sib_ep, b"sibling-bytes").unwrap();

    let dl = Arc::new(ContainerMappedDownloader::new(stage.clone(), "/dl"));
    let maps = vec![PathMap::new("/dl", stage.clone())];
    let (env, site_id, dl_id) = env_with_dl_maps(dir, dl.clone(), maps);

    // 1) Relative file path in TaskFileSet + container save_path `/dl` mapped to `stage`:
    //    both `/brush-state` and `downloader.delete-task` must recognize `library_paths`!
    let h_lib = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
    dl.inner
        .submit_verified(
            &SubmitPayload::Magnet {
                uri: format!("magnet:?xt=urn:btih:{h_lib}"),
                info_hash: h_lib.into(),
            },
            &SubmitOptions {
                ownership_tag: "r5-lib-tag".into(),
                ..Default::default()
            },
        )
        .unwrap();
    dl.inner
        .set_task_files(h_lib, vec![PathBuf::from("SeasonPack/LibEp01.mkv")], 13);
    let tf_lib = seed_boost_task(
        env.state.store().as_ref(),
        dl_id,
        site_id,
        "r5-lib-tag",
        h_lib,
        BoostTaskState::Seeding,
        13,
        0,
        Some(false),
        None,
    );
    seed_library_file_entry(env.state.store().as_ref(), &lib_ep);

    let (st_bs, _, b_bs) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/builtin.boost/brush-state",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let bs = assert_ok(st_bs, &b_bs, StatusCode::OK);
    assert_eq!(bs["tasks"][0]["library_referenced"], true, "bs={bs}");
    assert_eq!(bs["tasks"][0]["protection_state"], "protected");

    let dt_lib = run_boost_cmd(
        &env,
        "r5-dt-mapped-lib",
        "downloader.delete-task",
        json!({ "task_id": tf_lib }),
    )
    .await;
    assert_eq!(dt_lib["state"], "rejected");
    assert_eq!(dt_lib["phase"], "protection-denied:library-referenced");

    assert_dir_sibling_and_fail_closed(&env, &dl, site_id, dl_id, &stage, &sib_ep).await;
}

async fn assert_dir_sibling_and_fail_closed(
    env: &HttpEnv,
    dl: &Arc<ContainerMappedDownloader>,
    site_id: domain::SiteId,
    dl_id: domain::DownloaderId,
    stage: &Path,
    sib_ep: &Path,
) {
    let tf_ret = seed_boost_task(
        env.state.store().as_ref(),
        dl_id,
        site_id,
        "r5-ret-pack",
        "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2",
        BoostTaskState::Seeding,
        13,
        0,
        Some(false),
        None,
    );
    let pjson = json!({
        "save_path": stage,
        "allowed_roots": [stage],
        "paths": [sib_ep],
    })
    .to_string();
    env.state
        .store()
        .mark_boost_task_retained_with_paths(&tf_ret, 1, 13, Some(&pjson), 1_700_000_020)
        .unwrap();

    // Live sibling with empty `files` (metadata pending) and `snap.name = "SeasonPack"` under `/dl`!
    let live_sib = "c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3";
    dl.inner
        .submit_verified(
            &SubmitPayload::Magnet {
                uri: format!("magnet:?xt=urn:btih:{live_sib}"),
                info_hash: live_sib.into(),
            },
            &SubmitOptions {
                ownership_tag: "manual-dir-sib".into(),
                ..Default::default()
            },
        )
        .unwrap();
    dl.inner.push_snapshot(TaskSnapshot {
        info_hash: live_sib.into(),
        name: "SeasonPack".into(),
        progress: 0.5,
        state: "downloading".into(),
        size_bytes: 13,
        ..Default::default()
    });

    let (st_prev, _, b_prev) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/builtin.boost/brush-cleanup-preview?task_ids={tf_ret}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let prev = assert_ok(st_prev, &b_prev, StatusCode::OK);
    assert_eq!(prev["eligible_count"], 0, "prev={prev}");
    assert_eq!(
        prev["tasks"][0]["blocked_reason"],
        "shared-with-active-task"
    );

    let prep_dir = run_boost_cmd(
        env,
        "r5-prep-dir-sib",
        "downloader.prepare-cleanup",
        json!({ "task_ids": [tf_ret] }),
    )
    .await;
    assert_eq!(prep_dir["state"], "rejected");
    assert_eq!(prep_dir["phase"], "protection-denied:shared-path");

    // 3) Transient RPC failure on live sibling's `task_files_by_hash` MUST fail closed!
    *dl.fail_files_for_hash.lock() = Some(live_sib.into());
    dl.force_fail_files.store(true, Ordering::SeqCst);

    let prep_err = run_boost_cmd(
        env,
        "r5-prep-rpc-err",
        "downloader.prepare-cleanup",
        json!({ "task_ids": [tf_ret] }),
    )
    .await;
    assert_eq!(prep_err["state"], "failed", "prep_err={prep_err}");
    assert_eq!(prep_err["phase"], "read-protection-failed");

    let (st_prev_err, _, _) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/builtin.boost/brush-cleanup-preview?task_ids={tf_ret}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_ne!(st_prev_err, StatusCode::OK);
}

#[cfg(unix)]
#[tokio::test]
async fn slice_e_r5_item5b_slash_root_guard_and_pruned_parent_idempotent_retry() {
    // Part 1: `/` root guard refuses `path_maps` / `allowed_roots` resolving to `/`!
    let dir_slash = tempfile::tempdir_in("/tmp").unwrap();
    let victim_slash = dir_slash.path().join("NeverDeleteViaSlashRoot.mkv");
    std::fs::write(&victim_slash, b"precious").unwrap();
    let dl_slash = Arc::new(MemoryDownloader::new(dir_slash.path()));
    let (env_s, site_s, dl_id_s) =
        env_with_dl_maps(dir_slash, dl_slash, vec![PathMap::new("/dl", "/")]);
    let tf_s = seed_boost_task(
        env_s.state.store().as_ref(),
        dl_id_s,
        site_s,
        "r5-slash-root",
        "d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4",
        BoostTaskState::Seeding,
        8,
        0,
        Some(false),
        None,
    );
    let slash_json = json!({
        "save_path": "/",
        "allowed_roots": ["/"],
        "paths": [victim_slash],
    })
    .to_string();
    let ret_s = env_s
        .state
        .store()
        .mark_boost_task_retained_with_paths(&tf_s, 1, 8, Some(&slash_json), 1_700_000_010)
        .unwrap();
    let planned_s = env_s
        .state
        .store()
        .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: tf_s,
            expected_revision: ret_s.revision,
            cleanup_plan_handle: "r5-slash-plan".into(),
            cleanup_plan_json: slash_json,
            ttl_secs: 600,
            now_utc: api::job_loop::unix_now(),
        })
        .unwrap();
    let (st_tok_s, _, b_tok_s) = env_s
        .send(req_json(
            "GET",
            "/api/v1/plugins/builtin.boost/brush-cleanup-preview?plan_handle=r5-slash-plan",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let tok_s = assert_ok(st_tok_s, &b_tok_s, StatusCode::OK)["confirmation_token"]
        .as_str()
        .unwrap()
        .to_string();
    let del_slash = run_boost_cmd(
        &env_s,
        "r5-del-slash",
        "downloader.delete-files",
        json!({
            "host_file_plan_handle": "r5-slash-plan",
            "revision": planned_s.revision,
            "confirmation_token": tok_s,
        }),
    )
    .await;
    assert_eq!(del_slash["state"], "rejected", "del_slash={del_slash}");
    assert_eq!(del_slash["phase"], "manual-cleanup-required");
    assert!(victim_slash.exists(), "`/` root must never permit deletion");

    assert_pruned_parent_retry_succeeds().await;
}

#[cfg(unix)]
async fn assert_pruned_parent_retry_succeeds() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let real_root = dir.path().join("real_root");
    let nested = real_root.join("AlbumSub/Disc1");
    std::fs::create_dir_all(&nested).unwrap();
    let real_file = nested.join("Track01.mkv");
    std::fs::write(&real_file, b"music").unwrap();

    let sym_mount = dir.path().join("sym_mount");
    std::os::unix::fs::symlink(&real_root, &sym_mount).unwrap();
    let sym_target = sym_mount.join("AlbumSub/Disc1/Track01.mkv");

    let dl = Arc::new(MemoryDownloader::new(real_root.clone()));
    let (env, site_id, dl_id) =
        env_with_dl_maps(dir, dl, vec![PathMap::new("/dl", real_root.clone())]);
    let tf_id = seed_boost_task(
        env.state.store().as_ref(),
        dl_id,
        site_id,
        "r5-pruned-retry",
        "e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5",
        BoostTaskState::Seeding,
        5,
        0,
        Some(false),
        None,
    );
    let plan_json = json!({
        "save_path": sym_mount,
        "allowed_roots": [real_root],
        "paths": [sym_target],
    })
    .to_string();
    let ret = env
        .state
        .store()
        .mark_boost_task_retained_with_paths(&tf_id, 1, 5, Some(&plan_json), 1_700_000_010)
        .unwrap();
    let planned = env
        .state
        .store()
        .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: tf_id.clone(),
            expected_revision: ret.revision,
            cleanup_plan_handle: "r5-pruned-plan".into(),
            cleanup_plan_json: plan_json,
            ttl_secs: 600,
            now_utc: api::job_loop::unix_now(),
        })
        .unwrap();
    let (st_p, _, b_p) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/builtin.boost/brush-cleanup-preview?plan_handle=r5-pruned-plan",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let tok = assert_ok(st_p, &b_p, StatusCode::OK)["confirmation_token"]
        .as_str()
        .unwrap()
        .to_string();

    // Simulate prior partial execution that removed the file and pruned empty subdirectories!
    std::fs::remove_file(&real_file).unwrap();
    std::fs::remove_dir(&nested).unwrap();
    std::fs::remove_dir(real_root.join("AlbumSub")).unwrap();

    let del = run_boost_cmd(
        &env,
        "r5-del-pruned-retry",
        "downloader.delete-files",
        json!({
            "host_file_plan_handle": "r5-pruned-plan",
            "revision": planned.revision,
            "confirmation_token": tok,
        }),
    )
    .await;
    assert_eq!(del["state"], "succeeded", "del={del}");
    assert_eq!(
        env.state
            .store()
            .get_boost_task_fact(&tf_id)
            .unwrap()
            .unwrap()
            .state,
        BoostTaskState::Cleaned
    );
}
