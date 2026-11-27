use std::time::Duration;

use domain::{DownloaderId, LibraryId, SiteId, SubscribeId};
use plugin_api::{CommandAction, CommandRequest, GuestRole, OwnedRevision, TaskControl};
use plugin_host::Invocation;
use serde_json::json;
use store::plugins::{
    PluginCommandTarget, PluginError, PluginGrant, PluginGrantScope, PluginStateKind,
};
use store::{
    AcceptDownloadOperationInput, BoostProtectionState, BoostTaskState, DownloadOwnershipState,
    DownloadProvenanceKind, RecordDownloadOwnershipInput, StoreError, UpsertBoostTaskFactInput,
};

use crate::support::plugin_http_support::{HttpEnv, pkg};

#[tokio::test]
async fn slice_d_r4_item3a_store_require_grant_semantic_site_and_trimmed_uuid_matching() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let store = env.state.store();

    let site_id = env.insert_site("R4-ScopeSite");
    let dl_id = DownloaderId::new();
    let lib_id = LibraryId::new();
    let sub_id = SubscribeId::new();
    store
        .insert_downloader(&store::DownloaderRow {
            id: dl_id,
            name: "R4-ScopeDL".into(),
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

    let pid = "org.example.r4-sem-grant";
    let caps = [
        "kv",
        "http:site-auth",
        "command:site.check-in",
        "command:downloader.control",
        "command:schedule.set",
    ];
    let zip = pkg::build_task_package_custom(pid, "1.0.0", 1, false, None, true, &caps);
    let site_upper = site_id.to_string().to_ascii_uppercase();
    let dl_upper = dl_id.to_string().to_ascii_uppercase();
    let grants = json!([
        {
            "capability": "kv",
            "scope": { "Object": { "kind": "site", "id": format!("  SITE:{site_upper}  ") } },
            "sensitive": false
        },
        {
            "capability": "http:site-auth",
            "scope": { "Site": site_id },
            "sensitive": false
        },
        {
            "capability": "command:site.check-in",
            "scope": { "Object": { "kind": "site", "id": format!(" site:{site_upper} ") } },
            "sensitive": false
        },
        {
            "capability": "command:downloader.control",
            "scope": { "Object": { "kind": "downloader", "id": format!("  DOWNLOADER:{dl_upper}  ") } },
            "sensitive": false
        }
    ]);
    let detail = env.install_pkg(&zip, None, grants).await;
    let rev_dis = detail["revision"].as_i64().unwrap();

    // Also add library & subscribe command grants via store replace_plugin_grants to test all 3 UUID kinds!
    let mut all_grants = store.get_plugin_grants(pid).unwrap();
    all_grants.push(PluginGrant {
        capability: "command:library.scan".into(),
        scope: PluginGrantScope::Object {
            kind: "library".into(),
            id: format!("  LIBRARY:{}  ", lib_id.to_string().to_ascii_uppercase()),
        },
        sensitive: false,
    });
    all_grants.push(PluginGrant {
        capability: "command:subscribe.run".into(),
        scope: PluginGrantScope::Object {
            kind: "subscribe".into(),
            id: format!("  subscribe:{}  ", sub_id.to_string().to_ascii_uppercase()),
        },
        sensitive: false,
    });
    let hash = detail["package_hash"].as_str().unwrap().to_string();
    let updated = store
        .replace_plugin_grants(pid, &hash, rev_dis, &all_grants)
        .unwrap();
    env.enable_plugin(pid, updated.revision, "r4-sem-en").await;

    let enabled = store.get_plugin(pid).unwrap().unwrap();
    let obs = store.observe_site(site_id).unwrap().unwrap();
    let auth = store
        .issue_plugin_state_authority(&enabled, Some(&obs))
        .unwrap();

    let entry = store
        .plugin_state_put(&auth, PluginStateKind::Kv, "sem-k", b"sem-v", 0)
        .unwrap();
    assert_eq!(entry.revision, 1);

    assert_store_command_targets(&store, &auth, site_id, dl_id, lib_id, sub_id);
    assert_ports_downloader_prefixed_grant_e2e(&env, pid, site_id, dl_id, &obs, enabled.generation);
}

fn assert_store_command_targets(
    store: &store::Store,
    auth: &store::plugins::PluginStateAuthority,
    site_id: SiteId,
    dl_id: DownloaderId,
    lib_id: LibraryId,
    sub_id: SubscribeId,
) {
    assert!(
        store
            .authorize_plugin_command(auth, "site.check-in", &[PluginCommandTarget::Site(site_id)])
            .is_ok()
    );
    assert!(
        store
            .authorize_plugin_command(
                auth,
                "downloader.control",
                &[PluginCommandTarget::Downloader(dl_id)]
            )
            .is_ok()
    );
    assert!(
        store
            .authorize_plugin_command(
                auth,
                "library.scan",
                &[PluginCommandTarget::Library(lib_id)]
            )
            .is_ok()
    );
    assert!(
        store
            .authorize_plugin_command(
                auth,
                "subscribe.run",
                &[PluginCommandTarget::Subscribe(sub_id)]
            )
            .is_ok()
    );
    assert!(matches!(
        store.authorize_plugin_command(
            auth,
            "library.scan",
            &[PluginCommandTarget::Library(LibraryId::new())]
        ),
        Err(StoreError::Plugin(PluginError::Denied))
    ));
}

fn seed_boost_task_fact(
    store: &store::Store,
    pid: &str,
    dl_id: DownloaderId,
    site_id: SiteId,
) -> i64 {
    let now = 1_700_000_000i64;
    let tag = format!("cm:plugin:{pid}:task:tf-r4-sem");
    let hash = "aa11bb22cc33dd44ee55ff6677889900aabbccdd".to_string();
    store
        .accept_download_operation(&AcceptDownloadOperationInput {
            operation_id: "op-tf-r4-sem".into(),
            idempotency_key: "ik-tf-r4-sem".into(),
            source_kind: "boost".into(),
            subscribe_id: None,
            plugin_id: Some(pid.into()),
            subject: pid.into(),
            cause_id: "test".into(),
            site_id: Some(site_id),
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            enclosure: "https://pt.example/r4-sem.torrent".into(),
            ownership_tag: tag.clone(),
            info_hash: Some(hash.clone()),
            bytes_digest: Some("dig-r4-sem".into()),
            verified_bytes: Some(512),
            request_digest: "req-r4-sem".into(),
            now_utc: now,
        })
        .unwrap();
    store
        .record_download_ownership(&RecordDownloadOwnershipInput {
            downloader_id: dl_id,
            ownership_tag: tag.clone(),
            info_hash: Some(hash.clone()),
            operation_id: "op-tf-r4-sem".into(),
            subscribe_id: None,
            plugin_id: Some(pid.into()),
            subject: pid.into(),
            site_id: Some(site_id),
            enclosure: "https://pt.example/r4-sem.torrent".into(),
            ownership_state: DownloadOwnershipState::Owned,
            provenance_kind: DownloadProvenanceKind::CreatedFresh,
            verified_bytes: Some(512),
            bytes_digest: Some("dig-r4-sem".into()),
            now_utc: now,
        })
        .unwrap();
    store
        .upsert_boost_task_fact(&UpsertBoostTaskFactInput {
            task_fact_id: "tf-r4-sem".into(),
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
        .get_boost_task_fact("tf-r4-sem")
        .unwrap()
        .unwrap()
        .revision
}

fn assert_ports_downloader_prefixed_grant_e2e(
    env: &HttpEnv,
    pid: &str,
    site_id: SiteId,
    dl_id: DownloaderId,
    obs: &store::SiteObservation,
    generation: i64,
) {
    let store = env.state.store();
    let fact_rev = seed_boost_task_fact(&store, pid, dl_id, site_id);

    let ports = env.state.plugins().host_ports();
    let mut inv = Invocation::new(pid, Duration::from_secs(5));
    inv.generation = u64::try_from(generation).unwrap();
    inv.role = GuestRole::Task;
    inv.site_id = Some(site_id.to_string());
    inv.credential_epoch = Some(obs.credential_epoch().storage_key().to_string());
    let _g = ports.begin_invocation(&inv).unwrap();

    let op_id_str = ports
        .commands_submit(
            &inv,
            CommandRequest {
                action: CommandAction::DownloaderControl {
                    target: OwnedRevision {
                        handle: "tf-r4-sem".into(),
                        revision: u64::try_from(fact_rev).unwrap(),
                    },
                    control: TaskControl::Pause,
                },
                idempotency_key: "r4-sem-dl-ctrl".into(),
                expected_revision: None,
            },
        )
        .unwrap();
    assert!(!op_id_str.is_empty());
}
