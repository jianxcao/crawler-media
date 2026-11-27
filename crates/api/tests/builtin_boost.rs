#[path = "builtin_boost/support.rs"]
mod support;

use std::sync::atomic::Ordering;
use std::sync::{Arc, Barrier};

use api::{
    ApiDownloadService, DownloadOutcome, DownloadSubmitter, FrozenDownloadPlan, Store, router,
};
use axum::http::StatusCode;
use downloader::{DiskSpaceStatus, SubmissionPurpose};
use serde_json::json;
use store::plugins::{
    PluginGrant, PluginGrantScope, PluginLifecycleObservation, PluginOperationState,
    PluginVersionMetadata,
};
use store::{BoostReservationStage, BoostTaskState, PrepareBoostCleanupPlanInput};
use support::plugin_http_support::{HttpEnv, TOKEN, req_json};
use support::*;

const TASK_FIXTURE_WASM: &[u8] =
    include_bytes!("../../plugin-host/tests/fixtures/task_fixture.wasm");

#[test]
fn quota_accounting_and_concurrent_reservation_cas() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(BoostTestDownloader::new(dir.path().join("stage")));
    let srv = StubTorrentServer::start_dynamic(2500);
    let (state, site_id, dl_id) = setup_state(dir.path(), dl, Some(&srv));
    save_boost_config(&state, json!({ "budget_bytes": 10_000 }));

    seed_boost_task(
        state.store().as_ref(),
        dl_id,
        site_id,
        "seed-active",
        "1111111111111111111111111111111111111111",
        BoostTaskState::Seeding,
        4_000,
        0,
        Some(false),
        None,
    );
    let ret_id = seed_boost_task(
        state.store().as_ref(),
        dl_id,
        site_id,
        "seed-retained",
        "2222222222222222222222222222222222222222",
        BoostTaskState::Seeding,
        3_000,
        0,
        Some(false),
        None,
    );
    state
        .store()
        .mark_boost_task_retained(&ret_id, 1, 3_000, 1_700_000_010)
        .unwrap();

    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for i in 0..2 {
        let st = state.clone();
        let b = barrier.clone();
        let url = srv.url(&format!("{i}.torrent"));
        handles.push(std::thread::spawn(move || {
            b.wait();
            ApiDownloadService::new(st).submit_sync(
                boost_plan(site_id, dl_id, &url, &format!("Race-{i}")),
                boost_ctx(&format!("race-{i}")),
            )
        }));
    }
    let outcomes: Vec<DownloadOutcome> = handles
        .into_iter()
        .map(|h| h.join().unwrap().unwrap())
        .collect();
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| matches!(o, DownloadOutcome::Submitted { .. }))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| matches!(o, DownloadOutcome::Rejected { .. }))
            .count(),
        1
    );

    let snap = state
        .store()
        .get_boost_snapshot_for_plugin("builtin.boost", &[], &[])
        .unwrap();
    let used: i64 = snap.tasks.iter().map(|t| t.occupied_bytes).sum();
    let retained: i64 = snap.tasks.iter().map(|t| t.retained_bytes).sum();
    assert_eq!(used + retained, 9_500);
    assert!(used + retained <= 10_000);
}

#[test]
fn unknown_size_magnet_rejected_and_unknown_outcome_holds_reservation_across_restart() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(BoostTestDownloader::new(dir.path().join("stage")));
    let srv = StubTorrentServer::start(minimal_torrent_bytes("Unknown.Outcome.2026", 4096));
    let (state, site_id, dl_id) = setup_state(dir.path(), dl.clone(), Some(&srv));
    save_boost_config(&state, json!({ "budget_bytes": 20_000 }));

    let magnet = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567";
    let mag_out = ApiDownloadService::new(state.clone()).submit_sync(
        boost_plan(site_id, dl_id, magnet, "MagBoost"),
        boost_ctx("mag-1"),
    );
    assert!(
        matches!(
            mag_out,
            Err(api::DownloadSubmitError::Fetch(_)) | Ok(DownloadOutcome::Rejected { .. })
        ),
        "Boost must reject unknown-size magnet before download: {mag_out:?}"
    );

    dl.unknown_on_submit.store(true, Ordering::SeqCst);
    let url = srv.url("u.torrent");
    let unk_out = ApiDownloadService::new(state.clone())
        .submit_sync(
            boost_plan(site_id, dl_id, &url, "UnknownOutcome"),
            boost_ctx("unk-1"),
        )
        .unwrap();
    assert!(matches!(unk_out, DownloadOutcome::Unknown { .. }));
    drop(state);

    let reopened = Store::open(dir.path().join("data")).unwrap();
    let snap = reopened
        .get_boost_snapshot_for_plugin("builtin.boost", &[], &[])
        .unwrap();
    assert_eq!(snap.reservations.len(), 1);
    assert_eq!(
        snap.reservations[0].stage,
        BoostReservationStage::UnknownHeld
    );
    assert_eq!(snap.reservations[0].reserved_bytes, 4096);
}

#[tokio::test]
async fn disk_space_pause_and_download_before_gate_boost_vs_abstain() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(BoostTestDownloader::new(dir.path().join("stage")));
    let srv = StubTorrentServer::start(Vec::new());
    let (state, site_id, dl_id) = setup_state(dir.path(), dl.clone(), Some(&srv));

    // Unconfigured builtin.boost MUST abstain on Manual downloads even when disk unavailable.
    dl.set_disk(DiskSpaceStatus::Unavailable, None);
    let manual_plan = FrozenDownloadPlan {
        purpose: SubmissionPurpose::Manual,
        budget_key: None,
        tags: Vec::new(),
        ..boost_plan(site_id, dl_id, &srv.url("m.torrent"), "ManualSafe")
    };
    let manual_res = ApiDownloadService::new(state.clone())
        .submit(manual_plan, boost_ctx("manual-1"))
        .await
        .unwrap();
    assert!(matches!(manual_res, DownloadOutcome::Submitted { .. }));

    save_boost_config(
        &state,
        json!({ "budget_bytes": 100_000, "min_free_disk_bytes": 50_000 }),
    );
    for (status, avail) in [
        (DiskSpaceStatus::Unavailable, None),
        (DiskSpaceStatus::Unknown, None),
        (DiskSpaceStatus::Available, Some(10_000)),
    ] {
        dl.set_disk(status, avail);
        let res = ApiDownloadService::new(state.clone())
            .submit(
                boost_plan(
                    site_id,
                    dl_id,
                    &srv.url("d.torrent"),
                    &format!("Disk-{status:?}"),
                ),
                boost_ctx(&format!("disk-{status:?}")),
            )
            .await
            .unwrap();
        assert!(matches!(res, DownloadOutcome::Deferred { .. }));
    }

    dl.set_disk(DiskSpaceStatus::Available, Some(500_000));
    let env = HttpEnv {
        _dir: dir,
        state: state.clone(),
        app: router(state.clone()),
        member_token: String::new(),
    };
    let rev = env
        .state
        .store()
        .get_plugin("builtin.boost")
        .unwrap()
        .unwrap()
        .revision;
    let (st, _, body) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/builtin.boost/commands",
            Some(TOKEN),
            &[
                ("If-Match", &format!("\"{rev}\"")),
                ("Idempotency-Key", "dry-run-1"),
            ],
            Some(json!({
                "kind": "boost.run",
                "payload": { "site_id": site_id.to_string(), "downloader_id": dl_id.to_string(), "dry_run": true }
            })),
        ))
        .await;
    assert_eq!(st, StatusCode::ACCEPTED);
    let op_id = body["data"]["operation_id"].as_str().unwrap();
    let op = poll_op(&env, op_id).await;
    assert_eq!(op["state"], "succeeded");
    assert_eq!(op["result"]["added"], 0);

    save_boost_config(
        &env.state,
        json!({ "budget_bytes": 100_000, "min_free_disk_bytes": 1000, "max_adds_per_hour": 2 }),
    );
    for idx in 1..=3 {
        let out = ApiDownloadService::new(env.state.clone())
            .submit(
                boost_plan(
                    site_id,
                    dl_id,
                    &srv.url(&format!("cap-{idx}.torrent")),
                    &format!("Cap-{idx}"),
                ),
                boost_ctx(&format!("cap-{idx}")),
            )
            .await
            .unwrap();
        if idx <= 2 {
            assert!(matches!(out, DownloadOutcome::Submitted { .. }));
        } else {
            assert!(matches!(out, DownloadOutcome::Rejected { .. }));
        }
    }
}

fn seed_expired_plan_task(
    store: &Store,
    dl_id: domain::DownloaderId,
    site_id: domain::SiteId,
    stage: &std::path::Path,
) -> String {
    let exp_tf = seed_boost_task(
        store,
        dl_id,
        site_id,
        "exp-plan",
        "dddd111111111111111111111111111111111111",
        BoostTaskState::Seeding,
        2048,
        0,
        Some(false),
        None,
    );
    let stale_file = stage.join("StaleCleanup.mkv");
    std::fs::write(&stale_file, b"gone").unwrap();
    let ret = store
        .mark_boost_task_retained_with_paths(
            &exp_tf,
            1,
            2048,
            Some(&json!({ "paths": [stale_file.to_string_lossy()] }).to_string()),
            1_700_000_020,
        )
        .unwrap();
    store
        .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: exp_tf.clone(),
            expected_revision: ret.revision,
            cleanup_plan_handle: "expired-handle-1".into(),
            cleanup_plan_json: json!({ "paths": [stale_file.to_string_lossy()] }).to_string(),
            ttl_secs: 1,
            now_utc: 100,
        })
        .unwrap();
    exp_tf
}

#[tokio::test]
async fn protections_block_cleanup_and_expired_plan_reprepares() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(BoostTestDownloader::new(dir.path().join("stage")));
    let (state, site_id, dl_id) = setup_state(dir.path(), dl.clone(), None);
    let stage = dir.path().join("stage");
    std::fs::create_dir_all(&stage).unwrap();

    let hr_unknown_id = seed_boost_task(
        state.store().as_ref(),
        dl_id,
        site_id,
        "hr-unk",
        "aaaa111111111111111111111111111111111111",
        BoostTaskState::Seeding,
        1024,
        0,
        None,
        None,
    );
    let hold_id = seed_boost_task(
        state.store().as_ref(),
        dl_id,
        site_id,
        "hold-act",
        "bbbb111111111111111111111111111111111111",
        BoostTaskState::Seeding,
        1024,
        0,
        Some(false),
        Some(i64::MAX / 2),
    );
    let lib_file = stage.join("InLibrary.mkv");
    std::fs::write(&lib_file, b"keep").unwrap();
    let lib_tf = seed_boost_task(
        state.store().as_ref(),
        dl_id,
        site_id,
        "lib-ref",
        "cccc111111111111111111111111111111111111",
        BoostTaskState::Seeding,
        1024,
        0,
        Some(false),
        None,
    );
    seed_library_file_entry(state.store().as_ref(), &lib_file);
    state
        .store()
        .mark_boost_task_retained_with_paths(
            &lib_tf,
            1,
            1024,
            Some(&json!({ "paths": [lib_file.to_string_lossy()] }).to_string()),
            1_700_000_020,
        )
        .unwrap();

    let exp_tf = seed_expired_plan_task(state.store().as_ref(), dl_id, site_id, &stage);
    let env = HttpEnv {
        _dir: dir,
        state: state.clone(),
        app: router(state.clone()),
        member_token: String::new(),
    };
    let (st, _, body) = env
        .send(req_json(
            "GET",
            &format!(
                "/api/v1/plugins/builtin.boost/brush-cleanup-preview?task_ids={hr_unknown_id},{hold_id},{lib_tf}"
            ),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let preview = support::plugin_http_support::assert_ok(st, &body, StatusCode::OK);
    let items = preview["tasks"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert!(items.iter().all(|t| t["eligible"] == false));

    let rev = env
        .state
        .store()
        .get_plugin("builtin.boost")
        .unwrap()
        .unwrap()
        .revision;
    let (st_p, _, b_p) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/builtin.boost/commands",
            Some(TOKEN),
            &[
                ("If-Match", &format!("\"{rev}\"")),
                ("Idempotency-Key", "reprep-exp-1"),
            ],
            Some(json!({
                "kind": "downloader.prepare-cleanup",
                "payload": { "task_ids": [exp_tf] }
            })),
        ))
        .await;
    assert_eq!(st_p, StatusCode::ACCEPTED);
    let op_id = b_p["data"]["operation_id"].as_str().unwrap();
    let op = poll_op(&env, op_id).await;
    assert_eq!(op["state"], "succeeded");
    assert_ne!(op["result"]["plan_handle"], "expired-handle-1");
}

#[test]
fn wasm_task_fixture_exercises_real_boost_snapshot_and_command_permission_gate() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(BoostTestDownloader::new(dir.path().join("stage")));
    let (state, _, _) = setup_state(dir.path(), dl, None);
    let rel = "plugins/pkg/org.example.boost.wasm/v1/component.wasm";
    let path = state.store().data_dir().join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, TASK_FIXTURE_WASM).unwrap();

    let p = state
        .store()
        .register_plugin_version(
            &PluginVersionMetadata {
                plugin_id: "org.example.boost.wasm".into(),
                package_hash: "boost-wasm-h1".into(),
                version: "1.0.0".into(),
                manifest_json: "{}".into(),
                config_schema_json: "{}".into(),
                state_schema: "1".into(),
                artifact_path: rel.into(),
                signing_key_id: None,
                signature_status: "development".into(),
                ui_metadata_ref: None,
                credential_bound: false,
            },
            0,
        )
        .unwrap();
    let p = state
        .store()
        .replace_plugin_grants(
            &p.plugin_id,
            &p.package_hash,
            p.revision,
            &[PluginGrant {
                capability: "read:boost".into(),
                scope: PluginGrantScope::Global,
                sensitive: false,
            }],
        )
        .unwrap();
    let snap = state
        .store()
        .begin_plugin_lifecycle(&p.plugin_id, p.revision, uuid::Uuid::new_v4(), true)
        .unwrap();
    let enabled = state
        .store()
        .finalize_plugin_lifecycle(&PluginLifecycleObservation {
            plugin_id: p.plugin_id.clone(),
            operation_id: snap.active_operation_id.unwrap(),
            revision: snap.revision,
            generation: snap.generation,
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: true,
        })
        .unwrap();
    let host = state.plugins().wasm_host().unwrap();
    let ports = state.plugins().host_ports();
    let compiled = host
        .load_task(TASK_FIXTURE_WASM, plugin_host::TASK_WORLD)
        .unwrap();

    let mut inv = plugin_host::Invocation::new(
        enabled.plugin_id.clone(),
        std::time::Duration::from_secs(10),
    );
    inv.generation = u64::try_from(enabled.generation).unwrap();
    inv.role = plugin_api::GuestRole::Task;
    let _guard = ports.begin_invocation(&inv).unwrap();

    let snap_err = host
        .invoke_task(
            &compiled,
            inv.clone(),
            plugin_api::TaskInput {
                task_id: "boost-probe".into(),
                validated_input: "boost-snapshot".into(),
                job_id: domain::JobId::new().to_string(),
                attempt: 1,
            },
        )
        .unwrap_err()
        .to_string();
    assert!(
        snap_err.contains("invalid downloader filter UUID") || snap_err.contains("InvalidArgument"),
        "expected real StoreHostPorts boost_snapshot validation, got: {snap_err}"
    );

    let deny_err = host
        .invoke_task(
            &compiled,
            inv,
            plugin_api::TaskInput {
                task_id: "boost-probe".into(),
                validated_input: "downloader-commands".into(),
                job_id: domain::JobId::new().to_string(),
                attempt: 1,
            },
        )
        .unwrap_err()
        .to_string();
    assert!(
        deny_err.contains("PermissionDenied")
            || deny_err.contains("permission denied")
            || deny_err.contains("denied"),
        "expected PermissionDenied from WASM commands::submit before supports_kind, got: {deny_err}"
    );
    let _ = PluginOperationState::Succeeded;
}

fn install_dev_dl_add_plugin(state: &api::ApiState, pid: &str, dl_id: domain::DownloaderId) -> i64 {
    let rel = format!("plugins/pkg/{pid}/v1/task.wasm");
    let path = state.store().data_dir().join(&rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, TASK_FIXTURE_WASM).unwrap();
    let p = state
        .store()
        .register_plugin_version(
            &PluginVersionMetadata {
                plugin_id: pid.into(),
                package_hash: format!("{pid}-h1"),
                version: "1.0.0".into(),
                manifest_json: "{}".into(),
                config_schema_json: "{}".into(),
                state_schema: "1".into(),
                artifact_path: rel,
                signing_key_id: None,
                signature_status: "development".into(),
                ui_metadata_ref: None,
                credential_bound: false,
            },
            0,
        )
        .unwrap();
    let p = state
        .store()
        .replace_plugin_grants(
            &p.plugin_id,
            &p.package_hash,
            p.revision,
            &[PluginGrant {
                capability: "command:downloader.add".into(),
                scope: PluginGrantScope::Object {
                    kind: "downloader".into(),
                    id: dl_id.to_string(),
                },
                sensitive: false,
            }],
        )
        .unwrap();
    let snap = state
        .store()
        .begin_plugin_lifecycle(&p.plugin_id, p.revision, uuid::Uuid::new_v4(), true)
        .unwrap();
    state
        .store()
        .finalize_plugin_lifecycle(&PluginLifecycleObservation {
            plugin_id: p.plugin_id.clone(),
            operation_id: snap.active_operation_id.unwrap(),
            revision: snap.revision,
            generation: snap.generation,
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: true,
        })
        .unwrap()
        .revision
}

#[tokio::test]
async fn plugin_downloader_add_enforces_payload_budget_advertised_size_and_disk_free() {
    use downloader::Downloader;

    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(BoostTestDownloader::new(dir.path().join("stage")));
    let srv = StubTorrentServer::start_dynamic(20_000_000);
    let (state, _, dl_id) = setup_state(dir.path(), dl.clone(), Some(&srv));
    let pid = "org.example.dlbudget";
    let rev = install_dev_dl_add_plugin(&state, pid, dl_id);
    let env = HttpEnv {
        _dir: dir,
        state: state.clone(),
        app: router(state.clone()),
        member_token: String::new(),
    };
    let tgts = json!([{ "kind": "downloader", "id": dl_id.to_string() }]);

    let (st1, _, b1) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/commands"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", "dlb-1"),
            ],
            Some(json!({
                "kind": "downloader.add",
                "targets": tgts,
                "payload": {
                    "downloader_id": dl_id.to_string(),
                    "download_ref": srv.url("over-budget.torrent"),
                    "budget_bytes": 1_000_000,
                }
            })),
        ))
        .await;
    assert_eq!(st1, StatusCode::ACCEPTED);
    let op1 = poll_op(&env, b1["data"]["operation_id"].as_str().unwrap()).await;
    assert_eq!(
        op1["state"], "rejected",
        "over budget_bytes must reject: {op1:?}"
    );

    let (st2, _, b2) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/commands"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", "dlb-2"),
            ],
            Some(json!({
                "kind": "downloader.add",
                "targets": tgts,
                "payload": {
                    "downloader_id": dl_id.to_string(),
                    "download_ref": srv.url("dishonest.torrent"),
                    "advertised_size_bytes": 1024,
                    "budget_bytes": 50_000_000,
                }
            })),
        ))
        .await;
    assert_eq!(st2, StatusCode::ACCEPTED);
    let op2 = poll_op(&env, b2["data"]["operation_id"].as_str().unwrap()).await;
    assert_eq!(
        op2["state"], "rejected",
        "dishonest size must reject: {op2:?}"
    );

    dl.set_disk(DiskSpaceStatus::Available, Some(100_000));
    let (st3, _, b3) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/commands"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", "dlb-3"),
            ],
            Some(json!({
                "kind": "downloader.add",
                "targets": tgts,
                "payload": {
                    "downloader_id": dl_id.to_string(),
                    "download_ref": srv.url("no-disk.torrent"),
                    "budget_bytes": 50_000_000,
                }
            })),
        ))
        .await;
    assert_eq!(st3, StatusCode::ACCEPTED);
    let op3 = poll_op(&env, b3["data"]["operation_id"].as_str().unwrap()).await;
    assert_eq!(
        op3["state"], "rejected",
        "low disk_free_space must reject: {op3:?}"
    );
    assert!(dl.task_snapshots().unwrap().is_empty());
}
