use std::time::Duration;

use axum::http::StatusCode;
use domain::{DownloaderId, SiteId};
use plugin_api::{
    CommandAction, CommandRequest, DownloadOptions, GuestRole, MaintenanceAction, OwnedRevision,
    PageRequest, TaskControl,
};
use plugin_host::Invocation;
use serde_json::json;
use store::{
    AcceptDownloadOperationInput, BoostProtectionState, BoostTaskState, DownloadOwnershipState,
    DownloadProvenanceKind, RecordDownloadOwnershipInput, UpsertBoostTaskFactInput,
};

use crate::send_write;
use crate::support::StubTorrentServer;
use crate::support::plugin_http_support::{HttpEnv, assert_err, assert_ok, pkg};

const IDX_WASM: &[u8] = include_bytes!("../../../plugin-host/tests/fixtures/indexer_fixture.wasm");

fn build_padded_kind_zip(pid: &str) -> Vec<u8> {
    let cfg_schema = pkg::sample_schema(false, None);
    let site_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#;
    let task_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#;
    let manifest = format!(
        r#"manifest_version = 1
id = "{pid}"
name = "R6 Trimmed Object Kind"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test Author"]
description = "Consistent kind.trim() across Object scopes"
config_schema = "schemas/config.schema.json"
state_schema = 1
credential_binding = "invocation-site"

[[components]]
id = "idx-main"
entry = "components/idx.wasm"
world = "crawler:plugin/indexer-plugin@1.0.0"
permissions = ["indexer:provide", "read:sites", "kv"]

[[components.indexers]]
id = "fixture"
name = "Trimmed Indexer"
kind = "torznab"
capabilities = ["search", "rss", "detail", "resolve-download", "user-stats", "categories"]
site_schema = "schemas/site.schema.json"

[[components]]
id = "task-main"
entry = "components/task.wasm"
world = "crawler:plugin/task-plugin@1.0.0"
permissions = [
    "read:sites",
    "read:boost",
    "kv",
    "command:site.check-in",
    "command:downloader.control",
    "command:downloader.add"
]

[[components.tasks]]
id = "sync"
input_schema = "schemas/task.schema.json"
[components.tasks.schedule]
kind = "interval"
seconds = 300
"#
    );
    pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("schemas/config.schema.json", cfg_schema.as_bytes()),
        ("schemas/site.schema.json", site_schema.as_bytes()),
        ("schemas/task.schema.json", task_schema.as_bytes()),
        ("components/idx.wasm", IDX_WASM),
        ("components/task.wasm", pkg::task_wasm()),
    ])
}

#[tokio::test]
async fn slice_d_r6_item4_object_kind_trimmed_in_install_validation_ports_registry_and_commands() {
    let srv = StubTorrentServer::start_dynamic(1024);
    let mut env = HttpEnv::new().await;
    env.state = env.state.with_plugin_http_policy_override(
        net_guard::TargetPolicy::production().with_test_loopback_socket(srv.addr()),
    );
    env.app = api::router(env.state.clone());
    env.enable_dev_mode().await;

    let site_id = env.insert_site("R6-TrimSite");
    let dl_id = env.insert_downloader("R6-TrimDl");

    let pid = "org.example.r6-kind-trim";
    let zip = build_padded_kind_zip(pid);
    let up = env.upload_pkg(&zip, None, "r6-kt-up").await;
    let upload_id = up["upload_id"].as_str().unwrap();
    let (st_p, b_p) = send_write(
        &env,
        "POST",
        "/api/v1/plugin-installations/prepare",
        0,
        "r6-kt-prep",
        json!({ "upload_id": upload_id }),
    )
    .await;
    let prep = assert_ok(st_p, &b_p, StatusCode::ACCEPTED)["result"].clone();

    // 1. Nonexistent site / downloader with whitespace-padded `kind` must fail `validate_grant_targets`!
    for (idem, bad_grant) in [
        (
            "r6-kt-bad-site",
            json!({ "capability": "read:sites", "scope": { "Object": { "kind": " site ", "id": SiteId::new().to_string() } }, "sensitive": false }),
        ),
        (
            "r6-kt-bad-dl",
            json!({ "capability": "command:downloader.control", "scope": { "Object": { "kind": " downloader ", "id": DownloaderId::new().to_string() } }, "sensitive": false }),
        ),
    ] {
        let (st_bad, b_bad) = send_write(
            &env,
            "POST",
            "/api/v1/plugin-installations",
            0,
            idem,
            json!({
                "preparation_id": prep["preparation_id"],
                "consent_token": prep["consent_token"],
                "grants": [bad_grant],
            }),
        )
        .await;
        assert_err(
            st_bad,
            &b_bad,
            StatusCode::BAD_REQUEST,
            "plugin.scope_invalid",
        );
    }

    // 2. Seed persisted `Object { kind: " site " | " downloader " | " pool " }` rows and enable!
    let base_grants = json!([
        { "capability": "indexer:provide", "scope": { "Object": { "kind": "site", "id": site_id.to_string() } }, "sensitive": false },
        { "capability": "read:sites", "scope": { "Object": { "kind": "site", "id": site_id.to_string() } }, "sensitive": false },
        { "capability": "kv", "scope": { "Object": { "kind": "site", "id": site_id.to_string() } }, "sensitive": false },
        { "capability": "read:boost", "scope": { "Object": { "kind": "pool", "id": "default" } }, "sensitive": false },
        { "capability": "command:site.check-in", "scope": { "Object": { "kind": "site", "id": site_id.to_string() } }, "sensitive": false },
        { "capability": "command:downloader.control", "scope": { "Object": { "kind": "downloader", "id": dl_id.to_string() } }, "sensitive": false },
        { "capability": "command:downloader.add", "scope": { "Object": { "kind": "downloader", "id": dl_id.to_string() } }, "sensitive": false }
    ]);
    let (st_inst, b_inst) = send_write(
        &env,
        "POST",
        "/api/v1/plugin-installations",
        0,
        "r6-kt-inst",
        json!({
            "preparation_id": prep["preparation_id"],
            "consent_token": prep["consent_token"],
            "grants": base_grants,
        }),
    )
    .await;
    assert_ok(st_inst, &b_inst, StatusCode::ACCEPTED);
    env.state
        .store()
        .with_writer(store::Db::App, |c| {
            for (from, to) in [
                ("\"kind\":\"site\"", "\"kind\":\" site \""),
                ("\"kind\":\"downloader\"", "\"kind\":\" downloader \""),
                ("\"kind\":\"pool\"", "\"kind\":\" pool \""),
            ] {
                c.execute(
                    "UPDATE plugin_grants SET object_scope = REPLACE(object_scope, ?1, ?2) WHERE plugin_id = ?3",
                    rusqlite::params![from, to, pid],
                )?;
            }
            Ok(())
        })
        .unwrap();
    let rev = env.get_plugin_rev(pid).await;
    env.enable_plugin(pid, rev, "r6-kt-en").await;

    assert_registry_ports_and_commands_honor_trimmed_kind(&env, pid, site_id, dl_id, &srv);
}

fn assert_registry_ports_and_commands_honor_trimmed_kind(
    env: &HttpEnv,
    pid: &str,
    site_id: SiteId,
    dl_id: DownloaderId,
    srv: &StubTorrentServer,
) {
    let store = env.state.store();
    let expected_prov = format!("plugin:{pid}/fixture");
    assert!(
        env.state
            .indexer()
            .snapshot()
            .indexer_descriptors()
            .any(|d| d.id == expected_prov),
        "registry_sync must publish indexer provider when grant has Object kind = ' site '"
    );

    let enabled = store.get_plugin(pid).unwrap().unwrap();
    let epoch = store
        .observe_site(site_id)
        .unwrap()
        .unwrap()
        .credential_epoch()
        .storage_key()
        .to_string();
    let ports = env.state.plugins().host_ports();

    // Indexer provider invocation (`ensure_provider_role_grant`, `grant_matches_site`, `site_list`, `site_get`).
    let mut inv_idx = Invocation::new(pid, Duration::from_secs(5));
    inv_idx.generation = u64::try_from(enabled.generation).unwrap();
    inv_idx.role = GuestRole::Indexer;
    inv_idx.provider_id = Some("fixture".into());
    inv_idx.site_id = Some(site_id.to_string());
    inv_idx.credential_epoch = Some(epoch.clone());
    let g_idx = ports.begin_invocation(&inv_idx).unwrap();
    assert!(ports.authorized(&inv_idx, "kv"));
    assert_eq!(
        ports
            .site_list(
                &inv_idx,
                PageRequest {
                    limit: 10,
                    cursor: None
                }
            )
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(
        ports.site_get(&inv_idx, &site_id.to_string()).unwrap().id,
        site_id.to_string()
    );
    drop(g_idx);

    assert_trimmed_task_commands(env, pid, enabled.generation, site_id, dl_id, &epoch, srv);
}

fn assert_trimmed_task_commands(
    env: &HttpEnv,
    pid: &str,
    generation: i64,
    site_id: SiteId,
    dl_id: DownloaderId,
    epoch: &str,
    srv: &StubTorrentServer,
) {
    let store = env.state.store();
    let ports = env.state.plugins().host_ports();
    let fact_rev = seed_task_fact(&store, pid, dl_id, site_id);
    let mut inv_task = Invocation::new(pid, Duration::from_secs(5));
    inv_task.generation = u64::try_from(generation).unwrap();
    inv_task.role = GuestRole::Task;
    inv_task.provider_id = Some("task-main".into());
    inv_task.site_id = Some(site_id.to_string());
    inv_task.credential_epoch = Some(epoch.into());
    let g_task = ports.begin_invocation(&inv_task).unwrap();

    assert!(ports.boost_snapshot(&inv_task, &[], &[]).is_ok());
    assert!(
        ports
            .commands_submit(
                &inv_task,
                CommandRequest {
                    action: CommandAction::SiteMaintenance {
                        site_handle: site_id.to_string(),
                        action: MaintenanceAction::CheckIn,
                    },
                    idempotency_key: "r6-kt-cmd-site".into(),
                    expected_revision: None,
                },
            )
            .is_ok()
    );
    assert!(
        ports
            .commands_submit(
                &inv_task,
                CommandRequest {
                    action: CommandAction::DownloaderControl {
                        target: OwnedRevision {
                            handle: "tf-r6-kt".into(),
                            revision: u64::try_from(fact_rev).unwrap(),
                        },
                        control: TaskControl::Pause,
                    },
                    idempotency_key: "r6-kt-cmd-dl-ctrl".into(),
                    expected_revision: None,
                },
            )
            .is_ok()
    );
    assert!(
        ports
            .commands_submit(
                &inv_task,
                CommandRequest {
                    action: CommandAction::DownloaderAdd {
                        downloader_id: dl_id.to_string(),
                        download_ref: srv.url("r6-kt.torrent"),
                        options: DownloadOptions {
                            relative_directory: None,
                            category: None,
                            tags: vec![],
                            paused: false,
                            upload_limit: None,
                            download_limit: None,
                            sequential: false,
                            first_last_piece: false,
                        },
                    },
                    idempotency_key: "r6-kt-cmd-dl-add".into(),
                    expected_revision: None,
                },
            )
            .is_ok()
    );
    drop(g_task);
}

fn seed_task_fact(store: &store::Store, pid: &str, dl_id: DownloaderId, site_id: SiteId) -> i64 {
    let now = 1_700_000_000i64;
    let tag = format!("cm:plugin:{pid}:task:tf-r6-kt");
    let hash = "11223344556677889900aabbccddeeff00112233".to_string();
    store
        .accept_download_operation(&AcceptDownloadOperationInput {
            operation_id: "op-tf-r6-kt".into(),
            idempotency_key: "ik-tf-r6-kt".into(),
            source_kind: "boost".into(),
            subscribe_id: None,
            plugin_id: Some(pid.into()),
            subject: pid.into(),
            cause_id: "test".into(),
            site_id: Some(site_id),
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            enclosure: "https://pt.example/r6-kt.torrent".into(),
            ownership_tag: tag.clone(),
            info_hash: Some(hash.clone()),
            bytes_digest: Some("dig-r6-kt".into()),
            verified_bytes: Some(512),
            request_digest: "req-r6-kt".into(),
            now_utc: now,
        })
        .unwrap();
    store
        .record_download_ownership(&RecordDownloadOwnershipInput {
            downloader_id: dl_id,
            ownership_tag: tag.clone(),
            info_hash: Some(hash.clone()),
            operation_id: "op-tf-r6-kt".into(),
            subscribe_id: None,
            plugin_id: Some(pid.into()),
            subject: pid.into(),
            site_id: Some(site_id),
            enclosure: "https://pt.example/r6-kt.torrent".into(),
            ownership_state: DownloadOwnershipState::Owned,
            provenance_kind: DownloadProvenanceKind::CreatedFresh,
            verified_bytes: Some(512),
            bytes_digest: Some("dig-r6-kt".into()),
            now_utc: now,
        })
        .unwrap();
    store
        .upsert_boost_task_fact(&UpsertBoostTaskFactInput {
            task_fact_id: "tf-r6-kt".into(),
            plugin_id: pid.into(),
            quota_key: format!("{pid}:{dl_id}"),
            downloader_id: dl_id,
            ownership_tag: tag,
            info_hash: hash,
            site_id: Some(site_id),
            state: BoostTaskState::Seeding,
            occupied_bytes: 512,
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
        .unwrap();
    store
        .get_boost_task_fact("tf-r6-kt")
        .unwrap()
        .unwrap()
        .revision
}
