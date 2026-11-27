#[path = "builtin_boost/support.rs"]
pub(crate) mod support;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::{DownloaderId, MaintenanceVia, Site, SiteId};
use downloader::{Downloader, MemoryDownloader, SubmitOptions, SubmitPayload, TaskSnapshot};
use indexer::{
    Browser, BrowserConfig, PluginBrowserError, PluginBrowserPage, PluginNavResponse,
    RegistrySnapshot,
};
use plugin_api::{BrowserPurpose, ErrorCode, ErrorDetail, GuestRole, PluginTrust};
use plugin_host::Invocation;
use serde_json::{Value, json};
use store::download_operations::{
    AcceptDownloadOperationInput, BoostProtectionState, BoostTaskState, DownloadOwnershipState,
    DownloadProvenanceKind, RecordDownloadOwnershipInput, UpsertBoostTaskFactInput,
};
use support::NoopFetcher;
use support::plugin_http_support::{
    ADMIN_PASS, HttpEnv, TOKEN, assert_ok,
    pkg::{TestSigningKey, build_stored_zip},
    plugins_fixture, req_json,
};
use uuid::Uuid;

const INDEXER_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/indexer_fixture.wasm");

struct DummyPage;
impl PluginBrowserPage for DummyPage {
    fn set_cookies(&mut self, _: &str, _: &str) -> Result<(), PluginBrowserError> {
        Ok(())
    }
    fn navigate(&mut self, _: &str, _: &str) -> Result<PluginNavResponse, PluginBrowserError> {
        Ok(PluginNavResponse {
            status: 200,
            headers: vec![],
            body: b"<html></html>".to_vec(),
        })
    }
    fn eval_json(&mut self, _: &str) -> Result<Value, PluginBrowserError> {
        Ok(json!(true))
    }
    fn cookie_header(&mut self, _: &str) -> Result<String, PluginBrowserError> {
        Ok(String::new())
    }
    fn close_verified(&mut self) -> Result<(), PluginBrowserError> {
        Ok(())
    }
}

fn make_boost_r10_env() -> (HttpEnv, Arc<MemoryDownloader>) {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl_root = dir.path().join("dl_default");
    std::fs::create_dir_all(&dl_root).unwrap();
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let env_dl = Arc::new(MemoryDownloader::new(&dl_root));
    let state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        env_dl.clone() as Arc<dyn Downloader>,
    )
    .unwrap();
    let env = HttpEnv {
        _dir: dir,
        app: router(state.clone()),
        state,
        member_token: String::new(),
    };
    (env, env_dl)
}

fn seed_nil_boost_task(store: &Store, site_id: SiteId, hash: &str) {
    let nil_dl = DownloaderId::from_uuid(Uuid::nil());
    let tag = "cm-op:op-r10-nil";
    store
        .accept_download_operation(&AcceptDownloadOperationInput {
            operation_id: "op-r10-nil".into(),
            idempotency_key: "ik-op-r10-nil".into(),
            source_kind: "boost".into(),
            subscribe_id: None,
            plugin_id: Some("cm.boost".into()),
            subject: "system".into(),
            cause_id: "c-r10".into(),
            site_id: Some(site_id),
            downloader_id: Some(nil_dl),
            endpoint_fingerprint: None,
            enclosure: format!("magnet:?xt=urn:btih:{hash}&dn=Boost.Nil"),
            ownership_tag: tag.into(),
            info_hash: Some(hash.into()),
            bytes_digest: None,
            verified_bytes: Some(1024),
            request_digest: "rd-r10-nil".into(),
            now_utc: 100,
        })
        .unwrap();
    store
        .record_download_ownership(&RecordDownloadOwnershipInput {
            downloader_id: nil_dl,
            ownership_tag: tag.into(),
            info_hash: Some(hash.into()),
            bytes_digest: None,
            operation_id: "op-r10-nil".into(),
            subscribe_id: None,
            plugin_id: Some("cm.boost".into()),
            subject: "system".into(),
            site_id: Some(site_id),
            enclosure: format!("magnet:?xt=urn:btih:{hash}&dn=Boost.Nil"),
            ownership_state: DownloadOwnershipState::Owned,
            provenance_kind: DownloadProvenanceKind::CreatedFresh,
            verified_bytes: Some(1024),
            now_utc: 100,
        })
        .unwrap();
    store
        .upsert_boost_task_fact(&UpsertBoostTaskFactInput {
            task_fact_id: "tf-op-r10-nil".into(),
            plugin_id: "cm.boost".into(),
            downloader_id: nil_dl,
            site_id: Some(site_id),
            quota_key: format!("site:{site_id}"),
            ownership_tag: tag.into(),
            info_hash: hash.into(),
            state: BoostTaskState::Seeding,
            occupied_bytes: 1024,
            retained_bytes: 0,
            hr_active: Some(false),
            hr_until: None,
            hold_until: None,
            free_until: None,
            protection_state: BoostProtectionState::Eligible,
            protection_reason: None,
            observed_at: 100,
            expected_revision: None,
            now_utc: 100,
        })
        .unwrap();
}

#[tokio::test]
async fn boost_snapshot_nil_downloader_preserves_live_metrics_and_disabled_default_reports_unavailable()
 {
    let (env, env_dl) = make_boost_r10_env();
    plugins_fixture::install_cm_boost(&env, false, false, false).await;
    let site_id = env.insert_site("R10-Boost-NilSite");
    let hash = "ffff6666ffff6666ffff6666ffff6666ffff6666";
    env_dl
        .submit_verified(
            &SubmitPayload::Magnet {
                uri: format!("magnet:?xt=urn:btih:{hash}&dn=Boost.Nil"),
                info_hash: hash.into(),
            },
            &SubmitOptions {
                ownership_tag: "cm-op:op-r10-nil".into(),
                ..Default::default()
            },
        )
        .unwrap();
    env_dl.push_snapshot(TaskSnapshot {
        tag: "crawler-media".into(),
        name: "Boost.Nil".into(),
        progress: 1.0,
        state: "uploading".into(),
        size_bytes: 1024,
        downloaded_bytes: 1024,
        uploaded_bytes: 99_000,
        download_speed: 0,
        upload_speed: 500,
        info_hash: hash.into(),
        owned_tags: vec!["cm-op:op-r10-nil".into()],
        ..Default::default()
    });
    seed_nil_boost_task(env.state.store().as_ref(), site_id, hash);

    let nil_str = Uuid::nil().to_string();
    let (st, _, b) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/cm.boost/brush-state?downloader_id={nil_str}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let bs = assert_ok(st, &b, StatusCode::OK);
    assert_eq!(
        bs["tasks"][0]["uploaded_bytes"], 99_000,
        "explicit nil downloader_id must still collect live task_snapshots: {bs}"
    );

    // Insert a DB default downloader with enabled=false: brush-state without ?downloader_id must report unavailable
    let dis_def = DownloaderId::new();
    env.state
        .store()
        .insert_downloader(&store::DownloaderRow {
            id: dis_def,
            name: "Disabled-Default".into(),
            kind: "qbittorrent".into(),
            url: "http://127.0.0.1:9".into(),
            username: None,
            password: None,
            api_key: None,
            category: None,
            path_maps: vec![],
            is_default: true,
            enabled: false,
        })
        .unwrap();
    let (st2, _, b2) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/cm.boost/brush-state",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let bs2 = assert_ok(st2, &b2, StatusCode::OK);
    assert_eq!(
        bs2["disk"]["status"], "unavailable",
        "disabled default DB downloader must not fall back to env downloader: {bs2}"
    );
}

#[tokio::test]
async fn downloader_add_plugin_command_rejects_disabled_and_missing_downloaders() {
    let (env, _env_dl) = make_boost_r10_env();
    plugins_fixture::install_cm_boost(&env, false, false, false).await;
    let dis_dl = DownloaderId::new();
    env.state
        .store()
        .insert_downloader(&store::DownloaderRow {
            id: dis_dl,
            name: "Disabled-Add-DL".into(),
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

    let rev = env.get_plugin_rev("cm.boost").await;
    let hash = "aaaa7777aaaa7777aaaa7777aaaa7777aaaa7777";
    let (st_cmd, _, b_cmd) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", "r10-dl-add-disabled"),
            ],
            Some(json!({
                "kind": "downloader.add",
                "payload": {
                    "downloader_id": dis_dl.to_string(),
                    "download_url": format!("magnet:?xt=urn:btih:{hash}&dn=Disabled.Add")
                }
            })),
        ))
        .await;
    assert!(
        st_cmd == StatusCode::CONFLICT || st_cmd == StatusCode::BAD_REQUEST,
        "downloader.add targeting disabled downloader must be rejected upfront, got {st_cmd}: {b_cmd}"
    );
}

#[tokio::test]
async fn browser_adapter_blocks_teredo_zero_net_and_private_hostnames_on_self_listen() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(93);
    env.pin_trust_key(&key, "r10-bkey").await;
    let pid = "org.example.r10-browser-ssrf";
    let manifest = format!(
        "manifest_version = 1\nid = \"{pid}\"\nname = \"R10B\"\nversion = \"1.0.0\"\napi = \"1.0.0\"\nlicense = \"MIT\"\nauthors = [\"t\"]\ndescription = \"d\"\nconfig_schema = \"config.schema.json\"\nstate_schema = 1\ncredential_binding = \"invocation-site\"\n[[components]]\nid = \"fixture\"\nentry = \"indexer.wasm\"\nworld = \"crawler:plugin/indexer-plugin@1.0.0\"\npermissions = [\"http\", \"http:site-auth\", \"read:sites\", \"browser:search\", \"indexer:provide\"]\n[[components.indexers]]\nid = \"fixture\"\nname = \"Fixture\"\nkind = \"pt\"\ncapabilities = [\"search\", \"rss\", \"detail\", \"resolve-download\", \"user-stats\", \"categories\"]\nsite_schema = \"site.schema.json\"\n"
    );
    let schema = r#"{"type":"object","additionalProperties":false,"properties":{}}"#;
    let pkg_bytes = build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", schema.as_bytes()),
        ("site.schema.json", schema.as_bytes()),
        ("indexer.wasm", INDEXER_WASM),
    ]);
    let sig = key.sign_package(&pkg_bytes);

    let store = env.state.store();
    store.put_setting("cdp.enabled", "true").unwrap();
    let teredo_site = SiteId::new();
    let docker_self_site = SiteId::new();
    for (sid, name, url) in [
        (
            teredo_site,
            "TeredoZero",
            "http://[2001:0000:4136:e378:8000:63bf:ffff:ffff]:8080/",
        ),
        (
            docker_self_site,
            "DockerSelf",
            "http://host.docker.internal:18765/",
        ),
    ] {
        store
            .insert_site(&Site {
                id: sid,
                name: name.into(),
                url: url.into(),
                profile_id: format!("plugin:{pid}/fixture"),
                cookie: Some("c=1".into()),
                api_key: None,
                rss_url: None,
                proxy: None,
                rate_limit_per_minute: None,
                cdp_url: Some("http://127.0.0.1:9222".into()),
                downloader_id: None,
                enabled: true,
                maintenance_via: MaintenanceVia::Http,
                browser_search: true,
                timezone: "UTC".into(),
            })
            .unwrap();
    }

    let mut grants = vec![
        json!({"capability": "http", "scope": "Global", "sensitive": false}),
        json!({"capability": "read:sites", "scope": "Global", "sensitive": false}),
    ];
    for sid in [teredo_site, docker_self_site] {
        for cap in ["http:site-auth", "browser:search", "indexer:provide"] {
            grants.push(
                json!({"capability": cap, "scope": {"Site": sid.to_string()}, "sensitive": true}),
            );
        }
    }
    let inst = env
        .install_pkg(&pkg_bytes, Some(&sig), Value::Array(grants))
        .await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "r10-bs-en")
        .await;

    let browser = Arc::new(
        Browser::new(BrowserConfig {
            enabled: true,
            endpoint: Some("http://127.0.0.1:9222".into()),
            managed_endpoint: None,
        })
        .with_min_work(Duration::from_millis(10))
        .with_queue_cap(Duration::from_secs(2))
        .with_plugin_opener(|_| Ok(Box::new(DummyPage))),
    );
    let state_t = env
        .state
        .clone()
        .with_browser(browser)
        .with_self_listen(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 18765))
        .with_admin_trusted_plugins([pid]);
    let ports = state_t.plugins().host_ports();

    for sid in [teredo_site, docker_self_site] {
        let ep = ports
            .site_credential_epoch(&sid.to_string())
            .unwrap()
            .unwrap();
        let mut inv = Invocation::new(pid, Duration::from_secs(5));
        inv.generation = 1;
        inv.role = GuestRole::Indexer;
        inv.site_id = Some(sid.to_string());
        inv.credential_epoch = Some(ep);
        inv.trust = PluginTrust::AdminTrusted;
        let _g = ports.begin_invocation(&inv).unwrap();
        let err = ports
            .browser_open(&inv, BrowserPurpose::Search)
            .err()
            .expect("must block Teredo zero-net and host.docker.internal self_listen");
        assert_eq!(err.code, ErrorCode::PermissionDenied);
        assert_eq!(err.detail, Some(ErrorDetail::BlockedAddress));
    }
}
