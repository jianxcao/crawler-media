mod support;

use std::sync::atomic::{AtomicBool, Ordering};

use axum::http::StatusCode;
use domain::MediaKind;
use support::library_presence::PresenceFixture;

#[tokio::test]
async fn prune_purges_orphan_media_with_its_last_file() {
    let fix = PresenceFixture::new();
    let (media_id, row) = fix.insert_media_and_ledger("target.mkv");

    // Set user playback progress and favorite
    let user_id = domain::UserId::new();
    {
        let store_arc = fix.state.store();
        let guard = store_arc.as_ref();
        guard
            .set_playback_state(user_id, media_id, 3000, Some(true), Some(true), None, None)
            .unwrap();
    }

    // Verify to mark it missing
    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/verify", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["data"]["missing"], 1);

    // Prune the missing row
    let (status, res) = fix
        .request(
            "DELETE",
            &format!("/api/v1/libraries/{}/missing-rows", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["data"]["deleted"], 1);

    // Verify ledger row is deleted
    let store_arc = fix.state.store();
    let guard = store_arc.as_ref();
    assert!(guard.file_coverage_by_id(row.file_id).unwrap().is_none());

    // The Media lost its last file: it and its per-user state are purged
    // (no orphan rows; 2026-10-29 E2E item 1).
    assert!(guard.get_media(media_id).unwrap().is_none());
    assert_eq!(guard.playback_progress(user_id, media_id).unwrap(), None);
}

#[tokio::test]
async fn frozen_or_deleted_library_rejects_presence_operations() {
    let fix = PresenceFixture::new();
    let (_media_id, _row) = fix.insert_media_and_ledger("target.mkv");

    // Freeze library
    {
        let store_arc = fix.state.store();
        let guard = store_arc.as_ref();
        guard.begin_library_removal(&fix.library_id).unwrap();
    }

    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/verify", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(res["error"]["code"], "library.deleting");

    let (status, res) = fix
        .request(
            "DELETE",
            &format!("/api/v1/libraries/{}/missing-rows", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(res["error"]["code"], "library.deleting");
}

/// 在 `readable_root`（scope 已捕获、发布屏障尚未进入）里执行一次生命周期变更。
fn install_root_hook<F>(fix: &PresenceFixture, action: F)
where
    F: Fn(&api::Store, &str) + Send + Sync + 'static,
{
    let store = fix.state.store();
    let library_id = fix.library_id.clone();
    let fired = AtomicBool::new(false);
    *fix.mock_files.on_readable_root.lock() = Some(Box::new(move |_path| {
        if fired.swap(true, Ordering::SeqCst) {
            return;
        }
        action(store.as_ref(), &library_id);
    }));
}

/// 发布屏障内的 scope 换代必须按类型判成 `ScopeChanged`（409
/// `library.scope_changed`）。按文案分类的旧实现会把它落成伪造的 Store 故障
/// （500 `internal_error`）：同一个生命周期竞争被当成真实故障。
#[tokio::test]
async fn scope_change_race_is_classified_as_scope_changed() {
    let fix = PresenceFixture::new();
    let (_media_id, row) = fix.insert_media_and_ledger("target.mkv");
    // 第二个 Library 在 scope 捕获之后被冻结：`accepts_work` 集合换代，
    // 目标 Library 本身仍然接受工作（不会被误判成 Deleting）。
    let other_root = fix.dir.path().join("other");
    std::fs::create_dir_all(&other_root).unwrap();
    let other_id = {
        let store = fix.state.store();
        let guard = store.as_ref();
        guard
            .create_library(
                MediaKind::Movie,
                "Other",
                &[other_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
            .id
    };
    install_root_hook(&fix, move |store, _library_id| {
        store.begin_library_removal(&other_id).unwrap();
    });

    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/verify", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{res}");
    assert_eq!(res["error"]["code"], "library.scope_changed");

    // 竞争发生时屏障整体回滚：不写任何 presence 观察，也不动账本行。
    let guard = fix.state.store();
    let guard = guard.as_ref();
    assert!(guard.missing_at_by_path().unwrap().is_empty());
    assert!(guard.file_coverage_by_id(row.file_id).unwrap().is_some());
}

#[tokio::test]
async fn prune_with_permission_denied_on_recheck_skips_and_preserves_ledger() {
    let fix = PresenceFixture::new();
    let (_media_id, row) = fix.insert_media_and_ledger("perm_denied.mkv");
    let file_path = std::path::PathBuf::from(&row.path);

    // 1. First observation: Ok(false) -> marked missing
    fix.mock_files
        .files
        .lock()
        .insert(file_path.clone(), Ok(false));
    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/verify", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["data"]["missing"], 1);

    // 2. Before prune: file has permission denied on disk
    fix.mock_files
        .files
        .lock()
        .insert(file_path, Err(std::io::ErrorKind::PermissionDenied));

    // 3. Prune missing-rows: must NOT delete unverified/permission-denied files
    let (status, res) = fix
        .request(
            "DELETE",
            &format!("/api/v1/libraries/{}/missing-rows", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["data"]["deleted"], 0);
    assert_eq!(res["data"]["skipped"], 1);

    // 4. Ledger row is safely preserved!
    let store_arc = fix.state.store();
    let guard = store_arc.as_ref();
    assert!(guard.file_coverage_by_id(row.file_id).unwrap().is_some());
}

#[tokio::test]
async fn stale_missing_observation_never_cancels_republished_generation() {
    stale_presence_generation_request(false).await;
    stale_presence_generation_request(true).await;
}

async fn stale_presence_generation_request(prune: bool) {
    use api::probe_manager::policy::{ProbeRequestOrigin, ProbeRequestResult};
    use domain::UnitKey;
    use store::library_write::{FilePublication, WriteScope};
    // This case observes durable work immediately after a presence request.
    // Build outside Tokio so unrelated background probe workers cannot consume it.
    let fix = std::thread::spawn(PresenceFixture::new).join().unwrap();
    assert_eq!(fix.state.probe.worker_count(), 0);
    let (_, file) = fix.insert_media_and_ledger("replace-during-observe.mkv");
    fix.mock_files
        .files
        .lock()
        .insert(file.path.clone(), Ok(false));
    if prune {
        let store = fix.state.store();
        let store = store.as_ref();
        let row = store.file_coverage_by_id(file.file_id).unwrap().unwrap();
        store
            .apply_ledger_presence(
                &[store::LedgerPresenceObservation {
                    path_key: row.path_key.clone(),
                    file: row,
                    missing_at: None,
                    presence: domain::FilePresence::Missing,
                }],
                store::LedgerPresenceMode::Verify,
                1000,
            )
            .unwrap();
    }
    let state = fix.state.clone();
    let admitted_job = std::sync::Arc::new(std::sync::Mutex::new(None));
    let recorded_job = admitted_job.clone();
    let fired = AtomicBool::new(false);
    let file_id = file.file_id;
    *fix.mock_files.on_presence.lock() = Some(Box::new(move |_| {
        if fired.swap(true, Ordering::SeqCst) {
            return;
        }
        let store = state.store();
        assert!(
            store
                .writers()
                .with_writer_timeout(std::time::Duration::ZERO, store::Db::Library, |_| Ok(()))
                .is_ok(),
            "filesystem observations release Store"
        );
        let row = store
            .as_ref()
            .file_coverage_by_id(file_id)
            .unwrap()
            .unwrap();
        let mut replacement = row.file.clone();
        replacement.version.source_version = "replacement-v2".into();
        replacement.presence = domain::FilePresence::Present;
        store
            .as_ref()
            .publish_files(
                &WriteScope {
                    library_id: replacement.library_id,
                    config_revision: 0,
                },
                &[FilePublication {
                    file: &replacement,
                    units: &[UnitKey::WHOLE],
                    expected_version: Some(&row.file.version),
                    nfo_observation: None,
                    probe_required: true,
                }],
                2000,
            )
            .unwrap();
        let replacement = store
            .as_ref()
            .file_coverage_by_id(file_id)
            .unwrap()
            .unwrap();
        let ProbeRequestResult::Queued { job_id } = state
            .probe
            .request_probe(&replacement, ProbeRequestOrigin::Filesystem)
            .unwrap()
        else {
            panic!("replacement probe must be queued");
        };
        *recorded_job.lock().unwrap() = Some(job_id);
    }));
    let (status, response) = fix
        .request(
            if prune { "DELETE" } else { "POST" },
            &format!(
                "/api/v1/libraries/{}/{}",
                fix.library_id,
                if prune { "missing-rows" } else { "verify" }
            ),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        response["data"][if prune { "deleted" } else { "missing" }],
        0
    );
    assert_eq!(response["data"]["skipped"], 1);
    let store = fix.state.store();
    let store = store.as_ref();
    let row = store.file_coverage_by_id(file_id).unwrap().unwrap();
    assert_eq!(row.file.version.source_version, "replacement-v2");
    assert_eq!(row.file.presence, domain::FilePresence::Present);
    let active = store
        .active_probe_unit_for_ledger(&file_id.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(Some(active.job_id), *admitted_job.lock().unwrap());
    assert_eq!(store.count_probe_obligations().unwrap(), 1);
}

#[tokio::test]
async fn empty_presence_window_still_revalidates_captured_scope() {
    let fix = PresenceFixture::new();
    let other = fix.dir.path().join("other-empty");
    std::fs::create_dir_all(&other).unwrap();
    let other_id = fix
        .state
        .store()
        .create_library(
            MediaKind::Movie,
            "Other",
            &[other.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap()
        .id;
    install_root_hook(&fix, move |store, _| {
        store.begin_library_removal(&other_id).unwrap();
    });
    let (status, response) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/verify", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(response["error"]["code"], "library.scope_changed");
}
