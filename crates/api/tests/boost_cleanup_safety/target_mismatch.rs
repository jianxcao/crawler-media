use std::path::Path;
use std::sync::Arc;

use api::{PluginInvocationRole, Store, SubmitPluginCommand};
use axum::http::StatusCode;
use domain::{DownloaderId, PluginOperationId, SiteId};
use downloader::MemoryDownloader;
use serde_json::{Value, json};
use store::plugins::{
    CanonicalPluginRequest, PluginCommandCause, PluginCommandTarget, PluginGrant, PluginGrantScope,
    PluginOperationState, PluginSubject,
};
use store::{
    AcceptDownloadOperationInput, BoostProtectionState, BoostTaskState, DownloadOwnershipState,
    DownloadProvenanceKind, PrepareBoostCleanupPlanInput, RecordDownloadOwnershipInput,
    UpsertBoostTaskFactInput,
};

use super::support::plugin_http_support::{
    ADMIN_PASS, HttpEnv, TOKEN, admin_id, assert_err, assert_ok, pkg::build_task_package_custom,
    req_json,
};
use super::{boost_rev, env_with_dl, send_boost_cmd};

fn env_with_admin_dl(
    dir: tempfile::TempDir,
    dl: Arc<MemoryDownloader>,
) -> (HttpEnv, SiteId, DownloaderId) {
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let state = api::ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        indexer::RegistrySnapshot::builtin().unwrap(),
        Arc::new(super::support::NoopFetcher),
        dl,
    )
    .unwrap();
    let app = api::router(state.clone());
    let env = HttpEnv {
        _dir: dir,
        state,
        app,
        member_token: String::new(),
    };
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

async fn setup_narrow_plugin(
    env: &HttpEnv,
    plugin_id: &str,
    permissions: &[&str],
    grants: Vec<PluginGrant>,
) -> i64 {
    env.enable_dev_mode().await;
    let pkg = build_task_package_custom(plugin_id, "1.0.0", 1, false, None, false, permissions);
    let grants_json = serde_json::to_value(&grants).unwrap();
    let detail = env.install_pkg(&pkg, None, grants_json).await;
    env.enable_plugin(plugin_id, detail["revision"].as_i64().unwrap(), "en-narrow")
        .await;
    env.get_plugin_rev(plugin_id).await
}

fn setup_registered_narrow_plugin(store: &Store, plugin_id: &str, grants: Vec<PluginGrant>) -> i64 {
    let meta = store::plugins::PluginVersionMetadata {
        plugin_id: plugin_id.into(),
        package_hash: format!("{plugin_id}-h1"),
        version: "1.0.0".into(),
        manifest_json: "{}".into(),
        config_schema_json: "{}".into(),
        state_schema: "1".into(),
        artifact_path: "unused".into(),
        signing_key_id: None,
        signature_status: "development".into(),
        ui_metadata_ref: None,
        credential_bound: false,
    };
    let p = store.register_plugin_version(&meta, 0).unwrap();
    let p = store
        .replace_plugin_grants(&p.plugin_id, &p.package_hash, p.revision, &grants)
        .unwrap();
    let snap = store
        .begin_plugin_lifecycle(&p.plugin_id, p.revision, uuid::Uuid::new_v4(), true)
        .unwrap();
    store
        .finalize_plugin_lifecycle(&store::plugins::PluginLifecycleObservation {
            plugin_id: snap.plugin_id,
            revision: snap.revision,
            generation: snap.generation,
            operation_id: snap.active_operation_id.unwrap(),
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: true,
        })
        .unwrap()
        .revision
}

fn seed_plugin_task(
    store: &Store,
    plugin_id: &str,
    dl_id: DownloaderId,
    site_id: SiteId,
    tag: &str,
    hash: &str,
) -> String {
    let op_id = format!("op-{tag}");
    let now = 1_700_000_000i64;
    store
        .accept_download_operation(&AcceptDownloadOperationInput {
            operation_id: op_id.clone(),
            idempotency_key: format!("ik-{tag}"),
            source_kind: "boost".into(),
            subscribe_id: None,
            plugin_id: Some(plugin_id.into()),
            subject: plugin_id.into(),
            cause_id: "seed".into(),
            site_id: Some(site_id),
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            enclosure: format!("https://pt.example/{tag}.torrent"),
            ownership_tag: tag.into(),
            info_hash: Some(hash.into()),
            bytes_digest: Some(format!("dig-{tag}")),
            verified_bytes: Some(100),
            request_digest: format!("req-{tag}"),
            now_utc: now,
        })
        .unwrap();
    store
        .record_download_ownership(&RecordDownloadOwnershipInput {
            downloader_id: dl_id,
            ownership_tag: tag.into(),
            info_hash: Some(hash.into()),
            operation_id: op_id,
            subscribe_id: None,
            plugin_id: Some(plugin_id.into()),
            subject: plugin_id.into(),
            site_id: Some(site_id),
            enclosure: format!("https://pt.example/{tag}.torrent"),
            ownership_state: DownloadOwnershipState::Owned,
            provenance_kind: DownloadProvenanceKind::CreatedFresh,
            verified_bytes: Some(100),
            bytes_digest: Some(format!("dig-{tag}")),
            now_utc: now,
        })
        .unwrap();
    store
        .upsert_boost_task_fact(&UpsertBoostTaskFactInput {
            task_fact_id: format!("tf-{tag}"),
            plugin_id: plugin_id.into(),
            quota_key: format!("{plugin_id}:{dl_id}"),
            downloader_id: dl_id,
            site_id: Some(site_id),
            ownership_tag: tag.into(),
            info_hash: hash.into(),
            state: BoostTaskState::Seeding,
            occupied_bytes: 100,
            retained_bytes: 0,
            hr_active: Some(false),
            hr_until: None,
            hold_until: None,
            free_until: None,
            protection_state: BoostProtectionState::Eligible,
            protection_reason: None,
            observed_at: now,
            expected_revision: None,
            now_utc: now,
        })
        .unwrap()
        .task_fact_id
}

fn seed_cleanup_plan_on_dl(
    store: &Store,
    plugin_id: &str,
    dl_id: DownloaderId,
    site_id: SiteId,
    stage: &Path,
    victim: &Path,
    plan_handle: &str,
) -> (String, i64) {
    let tf_id = seed_plugin_task(
        store,
        plugin_id,
        dl_id,
        site_id,
        plan_handle,
        "aaaa111122223333444455556666777788889999",
    );
    let plan_json =
        json!({ "save_path": stage, "allowed_roots": [stage], "paths": [victim] }).to_string();
    let ret = store
        .mark_boost_task_retained_with_paths(&tf_id, 1, 100, Some(&plan_json), 1_700_000_010)
        .unwrap();
    let planned = store
        .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: tf_id.clone(),
            expected_revision: ret.revision,
            cleanup_plan_handle: plan_handle.into(),
            cleanup_plan_json: plan_json,
            ttl_secs: 600,
            now_utc: api::job_loop::unix_now(),
        })
        .unwrap();
    (tf_id, planned.revision)
}

async fn post_cmd(
    env: &HttpEnv,
    pid: &str,
    rev: i64,
    idem: &str,
    kind: &str,
    targets: Value,
    payload: Value,
) -> (StatusCode, Value) {
    let (st, _, b) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/commands"),
            Some(TOKEN),
            &[
                ("If-Match", &format!("\"{rev}\"")),
                ("Idempotency-Key", idem),
            ],
            Some(json!({ "kind": kind, "targets": targets, "payload": payload })),
        ))
        .await;
    (st, b)
}

fn seed_retained_task(
    store: &Store,
    plugin_id: &str,
    dl_id: DownloaderId,
    site_id: SiteId,
    victim: &Path,
    tag: &str,
    hash: &str,
) -> String {
    let tf_id = seed_plugin_task(store, plugin_id, dl_id, site_id, tag, hash);
    let stage = victim.parent().unwrap();
    let pjson =
        json!({ "save_path": stage, "allowed_roots": [stage], "paths": [victim] }).to_string();
    store
        .mark_boost_task_retained_with_paths(&tf_id, 1, 100, Some(&pjson), 1_700_000_010)
        .unwrap();
    tf_id
}

async fn fetch_cleanup_token(env: &HttpEnv, pid: &str, plan_handle: &str) -> String {
    let (st_prev, _, b_prev) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}/brush-cleanup-preview?plan_handle={plan_handle}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_ok(st_prev, &b_prev, StatusCode::OK)["confirmation_token"]
        .as_str()
        .unwrap()
        .to_string()
}

fn narrow_downloader_grants(dl_1: DownloaderId) -> Vec<PluginGrant> {
    [
        "command:downloader.add",
        "command:downloader.control",
        "command:downloader.delete-task",
        "command:downloader.prepare-cleanup",
        "command:downloader.delete-files",
    ]
    .into_iter()
    .map(|cap| PluginGrant {
        capability: cap.into(),
        scope: PluginGrantScope::Object {
            kind: "downloader".into(),
            id: dl_1.to_string(),
        },
        sensitive: false,
    })
    .collect()
}

#[tokio::test]
async fn narrow_downloader_grant_rejects_target_mismatch_across_all_downloader_commands() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let stage = dir.path().join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    let victim_dl2 = stage.join("dl2-victim.mkv");
    let victim_ret2 = stage.join("dl2-ret-victim.mkv");
    std::fs::write(&victim_dl2, b"keep-me").unwrap();
    std::fs::write(&victim_ret2, b"keep-ret").unwrap();
    let dl = Arc::new(MemoryDownloader::new(stage.clone()));
    let (env, site_id, dl_1) = env_with_admin_dl(dir, dl);
    let dl_2 = DownloaderId::new();

    let pid = "probe.narrow.dl";
    let rev = setup_registered_narrow_plugin(
        env.state.store().as_ref(),
        pid,
        narrow_downloader_grants(dl_1),
    );
    let store = env.state.store();
    let tf_dl1 = seed_plugin_task(
        store.as_ref(),
        pid,
        dl_1,
        site_id,
        "dl1-t",
        "1111000011111111111111111111111111111111",
    );
    let tf_dl2 = seed_plugin_task(
        store.as_ref(),
        pid,
        dl_2,
        site_id,
        "dl2-t",
        "2222000011111111111111111111111111111111",
    );
    let tf_ret2 = seed_retained_task(
        store.as_ref(),
        pid,
        dl_2,
        site_id,
        &victim_ret2,
        "dl2-ret",
        "3333000011111111111111111111111111111111",
    );
    let (_, plan_rev) = seed_cleanup_plan_on_dl(
        store.as_ref(),
        pid,
        dl_2,
        site_id,
        &stage,
        &victim_dl2,
        "plan-dl2",
    );
    let tok = fetch_cleanup_token(&env, pid, "plan-dl2").await;

    let tgts = json!([{ "kind": "downloader", "id": dl_1.to_string() }]);
    for (idem, kind, payload) in [
        (
            "m-add",
            "downloader.add",
            json!({ "downloader_id": dl_2.to_string(), "download_ref": "https://pt.example/1.torrent" }),
        ),
        (
            "m-ctrl-tf2",
            "downloader.control",
            json!({ "task_id": tf_dl2, "action": "pause" }),
        ),
        (
            "m-ctrl-dl2",
            "downloader.control",
            json!({ "task_id": tf_dl1, "downloader_id": dl_2.to_string(), "action": "pause" }),
        ),
        (
            "m-del-task",
            "downloader.delete-task",
            json!({ "task_id": tf_dl2 }),
        ),
        (
            "m-prep",
            "downloader.prepare-cleanup",
            json!({ "task_ids": [tf_ret2] }),
        ),
        (
            "m-del-files",
            "downloader.delete-files",
            json!({ "host_file_plan_handle": "plan-dl2", "revision": plan_rev, "confirmation_token": tok }),
        ),
    ] {
        let (st, b) = post_cmd(&env, pid, rev, idem, kind, tgts.clone(), payload).await;
        assert_err(st, &b, StatusCode::FORBIDDEN, "plugin.forbidden");
    }
    assert!(victim_dl2.exists());
    assert_eq!(
        store.get_boost_task_fact(&tf_ret2).unwrap().unwrap().state,
        BoostTaskState::RetainedOnly
    );

    let (st_ok, b_ok) = post_cmd(
        &env,
        pid,
        rev,
        "ok-ctrl",
        "downloader.control",
        tgts,
        json!({ "task_id": tf_dl1, "action": "pause" }),
    )
    .await;
    assert_ok(st_ok, &b_ok, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn narrow_site_grant_rejects_target_mismatch_on_site_maintenance_commands() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let (env, site_1, _) = env_with_admin_dl(dir, dl);
    let site_2 = env.insert_site("PT-Other");

    let pid = "probe.narrow.site";
    let perms = [
        "command:site.maintenance",
        "command:site.check-in",
        "command:site.verify-session",
    ];
    let grants = perms
        .iter()
        .map(|cap| PluginGrant {
            capability: (*cap).into(),
            scope: PluginGrantScope::Site(site_1),
            sensitive: false,
        })
        .collect();
    let rev = setup_registered_narrow_plugin(env.state.store().as_ref(), pid, grants);
    let tgts = json!([{ "kind": "site", "id": site_1.to_string() }]);

    for (idx, kind) in perms.iter().enumerate() {
        let cmd_kind = kind.trim_start_matches("command:");
        let (st, b) = post_cmd(
            &env,
            pid,
            rev,
            &format!("site-mis-{idx}"),
            cmd_kind,
            tgts.clone(),
            json!({ "site_id": site_2.to_string(), "action": "check-in" }),
        )
        .await;
        assert_err(st, &b, StatusCode::FORBIDDEN, "plugin.forbidden");
    }

    let (st_ok, b_ok) = post_cmd(
        &env,
        pid,
        rev,
        "site-ok",
        "site.verify-session",
        tgts,
        json!({ "site_id": site_1.to_string(), "action": "verify-session" }),
    )
    .await;
    assert_ok(st_ok, &b_ok, StatusCode::ACCEPTED);
}

fn queue_cmd_direct(
    state: &api::ApiState,
    pid: &str,
    kind: &str,
    targets: Vec<PluginCommandTarget>,
    payload: Value,
    idem: &str,
) -> PluginOperationId {
    let p = state.store().get_plugin(pid).unwrap().unwrap();
    let auth = state
        .store()
        .issue_plugin_state_authority(&p, None)
        .unwrap();
    let op = api::submit_plugin_command_for_test(
        state,
        SubmitPluginCommand {
            role: PluginInvocationRole::Management,
            authority: auth,
            subject: PluginSubject::User(admin_id()),
            kind: kind.into(),
            targets,
            idempotency_key: idem.into(),
            request: CanonicalPluginRequest {
                method: "POST".into(),
                path: format!("/api/v1/plugins/{pid}/commands"),
                body_digest: [3; 32],
            },
            expected_revision: Some(p.revision),
            cause: PluginCommandCause::root("exec-probe"),
            payload: serde_json::to_vec(&payload).unwrap(),
        },
    )
    .unwrap()
    .operation;
    PluginOperationId::from_uuid(op.operation_id)
}

fn exec_mismatch_grants(site_1: SiteId, dl_1: DownloaderId) -> Vec<PluginGrant> {
    let mut grants = vec![PluginGrant {
        capability: "command:site.maintenance".into(),
        scope: PluginGrantScope::Site(site_1),
        sensitive: false,
    }];
    for cap in [
        "command:downloader.add",
        "command:downloader.prepare-cleanup",
        "command:downloader.delete-files",
    ] {
        grants.push(PluginGrant {
            capability: cap.into(),
            scope: PluginGrantScope::Object {
                kind: "downloader".into(),
                id: dl_1.to_string(),
            },
            sensitive: false,
        });
    }
    grants
}

#[tokio::test]
async fn execution_rejects_ctx_targets_mismatch_if_command_queued_with_mismatched_target() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let stage = dir.path().join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    let victim_dl2 = stage.join("exec-victim.mkv");
    let victim_ret2 = stage.join("exec-ret-victim.mkv");
    std::fs::write(&victim_dl2, b"keep").unwrap();
    std::fs::write(&victim_ret2, b"keep-ret").unwrap();
    let dl = Arc::new(MemoryDownloader::new(stage.clone()));
    let (env, site_1, dl_1) = env_with_admin_dl(dir, dl);
    let site_2 = env.insert_site("PT-Exec-2");
    let dl_2 = DownloaderId::new();

    let pid = "probe.exec.mismatch";
    let perms = [
        "command:site.maintenance",
        "command:downloader.add",
        "command:downloader.prepare-cleanup",
        "command:downloader.delete-files",
    ];
    setup_narrow_plugin(&env, pid, &perms, exec_mismatch_grants(site_1, dl_1)).await;

    let store = env.state.store();
    let tf_ret2 = seed_retained_task(
        store.as_ref(),
        pid,
        dl_2,
        site_1,
        &victim_ret2,
        "exec-ret2",
        "4444000011111111111111111111111111111111",
    );
    let (_, plan_rev) = seed_cleanup_plan_on_dl(
        store.as_ref(),
        pid,
        dl_2,
        site_1,
        &stage,
        &victim_dl2,
        "exec-plan-dl2",
    );
    let tok = fetch_cleanup_token(&env, pid, "exec-plan-dl2").await;

    for (kind, tgts, payload, idem) in [
        (
            "site.maintenance",
            vec![PluginCommandTarget::Site(site_1)],
            json!({ "site_id": site_2.to_string(), "action": "verify-session" }),
            "ex-site",
        ),
        (
            "downloader.add",
            vec![PluginCommandTarget::Downloader(dl_1)],
            json!({ "downloader_id": dl_2.to_string(), "download_ref": "https://pt.example/1.torrent" }),
            "ex-add",
        ),
        (
            "downloader.prepare-cleanup",
            vec![PluginCommandTarget::Downloader(dl_1)],
            json!({ "task_ids": [tf_ret2] }),
            "ex-prep",
        ),
        (
            "downloader.delete-files",
            vec![PluginCommandTarget::Downloader(dl_1)],
            json!({ "host_file_plan_handle": "exec-plan-dl2", "revision": plan_rev, "confirmation_token": tok }),
            "ex-df",
        ),
    ] {
        let op_id = queue_cmd_direct(&env.state, pid, kind, tgts, payload, idem);
        api::run_plugin_command_for_test(&env.state, op_id).unwrap();
        let op = store
            .get_plugin_operation(&PluginSubject::User(admin_id()), op_id.as_uuid())
            .unwrap()
            .unwrap();
        assert_eq!(
            op.state,
            PluginOperationState::Rejected,
            "kind={kind} op={op:?}"
        );
        assert_eq!(op.phase, "target-mismatch", "kind={kind} op={op:?}");
    }
    assert!(victim_dl2.exists());
    assert_eq!(
        store.get_boost_task_fact(&tf_ret2).unwrap().unwrap().state,
        BoostTaskState::RetainedOnly
    );
}

fn narrow_builtin_boost_grants(store: &Store, dl_1: DownloaderId, site_1: SiteId) {
    let p = store.get_plugin("builtin.boost").unwrap().unwrap();
    let dis = store
        .begin_plugin_lifecycle(&p.plugin_id, p.revision, uuid::Uuid::new_v4(), false)
        .unwrap();
    let dis = store
        .finalize_plugin_lifecycle(&store::plugins::PluginLifecycleObservation {
            plugin_id: dis.plugin_id.clone(),
            operation_id: dis.active_operation_id.unwrap(),
            revision: dis.revision,
            generation: dis.generation,
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: true,
        })
        .unwrap();
    let narrow = store
        .replace_plugin_grants(
            "builtin.boost",
            &dis.package_hash,
            dis.revision,
            &[
                PluginGrant {
                    capability: "command:boost.run".into(),
                    scope: PluginGrantScope::Object {
                        kind: "downloader".into(),
                        id: dl_1.to_string(),
                    },
                    sensitive: false,
                },
                PluginGrant {
                    capability: "command:boost.run".into(),
                    scope: PluginGrantScope::Site(site_1),
                    sensitive: false,
                },
            ],
        )
        .unwrap();
    let en = store
        .begin_plugin_lifecycle(
            &narrow.plugin_id,
            narrow.revision,
            uuid::Uuid::new_v4(),
            true,
        )
        .unwrap();
    store
        .finalize_plugin_lifecycle(&store::plugins::PluginLifecycleObservation {
            plugin_id: en.plugin_id.clone(),
            operation_id: en.active_operation_id.unwrap(),
            revision: en.revision,
            generation: en.generation,
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: true,
        })
        .unwrap();
}

#[tokio::test]
async fn boost_run_rejects_mismatched_narrow_targets() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let (env, site_1, dl_1) = env_with_dl(dir, dl);
    let site_2 = env.insert_site("PT-Boost-2");
    let dl_2 = DownloaderId::new();
    narrow_builtin_boost_grants(env.state.store().as_ref(), dl_1, site_1);

    let rev = boost_rev(&env);
    let tgts = json!([
        { "kind": "downloader", "id": dl_1.to_string() },
        { "kind": "site", "id": site_1.to_string() }
    ]);
    for (idem, payload) in [
        (
            "br-dl2",
            json!({ "downloader_id": dl_2.to_string(), "site_id": site_1.to_string() }),
        ),
        (
            "br-s2",
            json!({ "downloader_id": dl_1.to_string(), "site_id": site_2.to_string() }),
        ),
        (
            "br-empty-sites",
            json!({ "downloader_id": dl_1.to_string() }),
        ),
    ] {
        let (st, b) = post_cmd(
            &env,
            "builtin.boost",
            rev,
            idem,
            "boost.run",
            tgts.clone(),
            payload,
        )
        .await;
        assert_err(st, &b, StatusCode::FORBIDDEN, "plugin.forbidden");
    }

    let (st_ok, b_ok) = post_cmd(
        &env,
        "builtin.boost",
        rev,
        "br-ok",
        "boost.run",
        tgts,
        json!({ "downloader_id": dl_1.to_string(), "site_id": site_1.to_string(), "dry_run": true }),
    )
    .await;
    assert_ok(st_ok, &b_ok, StatusCode::ACCEPTED);
    let _ = send_boost_cmd;
}
