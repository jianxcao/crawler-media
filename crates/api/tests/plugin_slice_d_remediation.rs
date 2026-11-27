#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use api::plugins::PrepareInstallInput;
use api::plugins::hooks_runtime::{TransferRenameRequest, run_transfer_rename};
use api::{ApiState, Store, router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::{
    DownloaderId, LibraryId, MaintenanceVia, MediaId, MediaKind, Site, SiteId, SubscribeId, UserId,
};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
use plugin_api::{
    CommandAction, CommandRequest, DownloadAfterEvent, ErrorCode, GuestRole, HOOK_WORLD,
    OwnedRevision, SiteCheckinAfterEvent, TaskControl, TransferCompletedEvent,
};
use plugin_host::Invocation;
use plugin_http_support::{
    ADMIN_PASS, HttpEnv, TOKEN, assert_err, assert_ok, pkg, req_json, req_multipart,
};
use serde_json::{Value, json};
use store::download_operations::{
    AcceptDownloadOperationInput, BoostProtectionState, BoostTaskState, DownloadOwnershipState,
    DownloadProvenanceKind, PrepareBoostCleanupPlanInput, RecordDownloadOwnershipInput,
    UpsertBoostTaskFactInput,
};
use store::plugin_events::{EmitOutboxEventInput, OutboxDb, OutboxDeliveryStatus};
use store::plugins::CanonicalPluginRequest;
use uuid::Uuid;

const POLICY_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/policy.wasm");

fn g_global(cap: &str) -> Value {
    json!({ "capability": cap, "scope": "Global", "sensitive": false })
}

fn g_site(cap: &str, sid: SiteId) -> Value {
    json!({ "capability": cap, "scope": { "Site": sid.to_string() }, "sensitive": cap == "http:site-auth" })
}

fn g_obj(cap: &str, kind: &str, id: impl ToString) -> Value {
    json!({ "capability": cap, "scope": { "Object": { "kind": kind, "id": id.to_string() } }, "sensitive": false })
}

async fn get_ok(env: &HttpEnv, uri: &str) -> Value {
    let (st, _, b) = env.send(req_json("GET", uri, Some(TOKEN), &[], None)).await;
    assert_ok(st, &b, StatusCode::OK)
}

async fn post_ok(env: &HttpEnv, uri: &str, body: Value) -> Value {
    let (st, _, b) = env
        .send(req_json("POST", uri, Some(TOKEN), &[], Some(body)))
        .await;
    assert_ok(st, &b, StatusCode::OK)
}

struct MockMteamFetcher(Vec<(&'static str, &'static str)>);

impl Fetcher for MockMteamFetcher {
    fn fetch(&self, _req: &FetchRequest) -> Result<String, IndexerError> {
        let rows: Vec<Value> = self
            .0
            .iter()
            .map(|(id, name)| {
                json!({"id": id, "name": name, "size": "2048", "seeders": "10", "leechers": "1", "createdDate": "2024-03-01 12:00:00"})
            })
            .collect();
        Ok(json!({ "code": "0", "data": { "data": rows } }).to_string())
    }
}

fn build_policy_zip(id: &str, cred_bound: bool, hooks_toml: &str, perms: &[&str]) -> Vec<u8> {
    let perms_str = perms
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let cred = if cred_bound {
        "credential_binding = \"invocation-site\"\n"
    } else {
        ""
    };
    let schema = pkg::sample_schema(false, None);
    let manifest = format!(
        "manifest_version = 1\nid = \"{id}\"\nname = \"P {id}\"\nversion = \"1.0.0\"\napi = \"1.0.0\"\nlicense = \"MIT\"\nauthors = [\"t\"]\ndescription = \"d\"\nconfig_schema = \"config.schema.json\"\nstate_schema = 1\n{cred}[[components]]\nid = \"policy\"\nentry = \"policy.wasm\"\nworld = \"{HOOK_WORLD}\"\npermissions = [{perms_str}]\n\n{hooks_toml}\n"
    );
    pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", schema.as_bytes()),
        ("policy.wasm", POLICY_WASM),
    ])
}

async fn install_and_enable(env: &HttpEnv, id: &str, zip: &[u8], grants: Value) {
    let inst = env.install_pkg(zip, None, grants).await;
    let rev = inst["revision"].as_i64().unwrap();
    env.enable_plugin(id, rev, &format!("en-{id}")).await;
}

fn insert_mteam_site(store: &Store, name: &str) -> SiteId {
    let id = SiteId::new();
    store
        .insert_site(&Site {
            id,
            name: name.into(),
            url: format!("https://{name}.example/"),
            profile_id: "mteam".into(),
            cookie: None,
            api_key: Some("k-secret".into()),
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: Some(60),
            cdp_url: None,
            downloader_id: None,
            enabled: true,
            maintenance_via: MaintenanceVia::Http,
            browser_search: false,
            timezone: "UTC".into(),
        })
        .unwrap();
    id
}

#[allow(clippy::too_many_arguments)]
fn emit_evt(
    store: &Store,
    db: OutboxDb,
    id: &str,
    kind: &str,
    site_id: Option<String>,
    lib_id: Option<String>,
    payload: &impl serde::Serialize,
    now_ms: i64,
) {
    let domain_name = kind.split('.').next().unwrap_or("site");
    store
        .emit_outbox_event(
            db,
            &EmitOutboxEventInput {
                event_id: id.into(),
                schema_version: 1,
                event_domain: domain_name.into(),
                event_kind: kind.into(),
                entity_type: domain_name.into(),
                entity_id: id.into(),
                entity_sequence: None,
                entity_revision: 1,
                cause_id: format!("c-{id}"),
                subject: "system".into(),
                site_id,
                subscribe_id: None,
                library_id: lib_id,
                handles_json: "[]".into(),
                payload_json: serde_json::to_string(payload).unwrap(),
                occurred_at: now_ms,
                created_at: now_ms,
            },
        )
        .unwrap();
}

fn new_search_env() -> HttpEnv {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let fetcher = Arc::new(MockMteamFetcher(vec![
        ("1", "A"),
        ("2", "Keep.Movie.2024.1080p"),
    ]));
    let state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        fetcher,
        dl,
    )
    .unwrap();
    HttpEnv {
        _dir: dir,
        app: router(state.clone()),
        state,
        member_token: String::new(),
    }
}

#[tokio::test]
async fn defect_1ab_component_permissions_and_hook_grant_scope_enforced() {
    let env = new_search_env();
    env.enable_dev_mode().await;
    let (site_a, site_b) = (
        insert_mteam_site(&env.state.store(), "sitea"),
        insert_mteam_site(&env.state.store(), "siteb"),
    );
    let (lib_a, lib_b, sub_a, sub_b) = (
        LibraryId::new(),
        LibraryId::new(),
        SubscribeId::new(),
        SubscribeId::new(),
    );

    let schema = pkg::sample_schema(false, None);
    let pb_manifest = format!(
        "manifest_version = 1\nid = \"org.example.piggyback\"\nname = \"PB\"\nversion = \"1.0.0\"\napi = \"1.0.0\"\nlicense = \"MIT\"\nauthors = [\"t\"]\ndescription = \"pb\"\nconfig_schema = \"config.schema.json\"\nstate_schema = 1\n[[components]]\nid = \"holder\"\nentry = \"holder.wasm\"\nworld = \"{HOOK_WORLD}\"\npermissions = [\"hooks:search.before\", \"hooks:download.after\", \"events:download.after\"]\n[[components.hooks]]\nid = \"subscribe.score\"\npriority = 10\n[[components]]\nid = \"rider\"\nentry = \"rider.wasm\"\nworld = \"{HOOK_WORLD}\"\npermissions = []\n[[components.hooks]]\nid = \"search.before\"\npriority = 10\n[[components.hooks]]\nid = \"download.after\"\npriority = 10\n"
    );
    let pb_zip = pkg::build_stored_zip(&[
        ("plugin.toml", pb_manifest.as_bytes()),
        ("config.schema.json", schema.as_bytes()),
        ("holder.wasm", POLICY_WASM),
        ("rider.wasm", POLICY_WASM),
    ]);
    let pb_grants = json!([
        g_global("hooks:search.before"),
        g_global("hooks:download.after"),
        g_global("events:download.after")
    ]);
    install_and_enable(&env, "org.example.piggyback", &pb_zip, pb_grants).await;

    let hooks_toml = "[[components.hooks]]\nid = \"search.before\"\npriority = 10\n[[components.hooks]]\nid = \"search.site.after\"\npriority = 10\n[[components.hooks]]\nid = \"transfer.rename\"\npriority = 10\n";
    let sh_zip = build_policy_zip(
        "org.example.scoped-hook",
        false,
        hooks_toml,
        &[
            "hooks:search.before",
            "hooks:search.site.after",
            "hooks:transfer.rename",
        ],
    );
    let sh_grants = json!([
        g_site("hooks:search.before", site_a),
        g_site("hooks:search.site.after", site_a),
        g_obj("hooks:transfer.rename", "library", lib_a),
        g_obj("hooks:transfer.rename", "subscribe", sub_a),
    ]);
    install_and_enable(&env, "org.example.scoped-hook", &sh_zip, sh_grants).await;

    let data = get_ok(&env, "/api/v1/search/torrents?keyword=reject-me").await;
    let a_sites: Vec<&str> = data["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["title"].as_str() == Some("A"))
        .filter_map(|i| i["site_id"].as_str())
        .collect();
    assert_eq!(a_sites, vec![site_b.to_string()]);

    let now_ms = store::unix_now_ms().unwrap();
    let dl_p = DownloadAfterEvent {
        operation_id: "op-pb".into(),
        downloader_id: None,
        info_hash: None,
        outcome: "submitted".into(),
        verified_total_bytes: Some(128),
    };
    emit_evt(
        &env.state.store(),
        OutboxDb::Subscribe,
        "ev-pb-1",
        "download.after",
        None,
        None,
        &dl_p,
        now_ms,
    );
    api::drain_plugin_events_for_test(&env.state, now_ms).await;
    let pb_rec = env.state.store().get_delivery_receipt(
        OutboxDb::Subscribe,
        "ev-pb-1",
        "org.example.piggyback",
    );
    assert!(pb_rec.unwrap().is_none());

    let root = env._dir.path().join("libroot");
    std::fs::create_dir_all(&root).unwrap();
    let mk_req = |lib: LibraryId, sub: Option<SubscribeId>| TransferRenameRequest {
        workflow_id: "wf-tr".into(),
        library_id: lib,
        library_root: root.clone(),
        media_id: MediaId::new(),
        media_kind: MediaKind::Movie,
        subscribe_id: sub,
        source_path: PathBuf::from("/downloads/Film.mkv"),
        proposed_relative_path: PathBuf::from("Film.mkv"),
    };
    assert_eq!(
        run_transfer_rename(&env.state, &mk_req(lib_b, Some(sub_b))),
        root.join("Film.mkv")
    );
    for (lib, sub) in [(lib_a, Some(sub_b)), (lib_b, Some(sub_a))] {
        assert_eq!(
            run_transfer_rename(&env.state, &mk_req(lib, sub)),
            root.join("renamed/Film.mkv")
        );
    }
}

#[tokio::test]
async fn defect_1c_outbox_events_enforce_grant_scope_per_event_without_scope_blind_cache() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let (site_a, site_b) = (env.insert_site("EvtSiteA"), env.insert_site("EvtSiteB"));
    let (lib_a, lib_b) = (LibraryId::new(), LibraryId::new());

    let zip_s = build_policy_zip(
        "org.example.evt-site",
        false,
        "[[components.hooks]]\nid = \"site.checkin.after\"\npriority = 10\n",
        &["hooks:site.checkin.after", "events:site.checkin.after"],
    );
    install_and_enable(
        &env,
        "org.example.evt-site",
        &zip_s,
        json!([
            g_site("hooks:site.checkin.after", site_a),
            g_site("events:site.checkin.after", site_a)
        ]),
    )
    .await;

    let zip_l = build_policy_zip(
        "org.example.evt-lib",
        false,
        "[[components.hooks]]\nid = \"transfer.completed\"\npriority = 10\n",
        &["hooks:transfer.completed", "events:transfer.completed"],
    );
    install_and_enable(
        &env,
        "org.example.evt-lib",
        &zip_l,
        json!([
            g_obj("hooks:transfer.completed", "library", lib_a),
            g_global("events:transfer.completed")
        ]),
    )
    .await;

    let now_ms = store::unix_now_ms().unwrap();
    for (ev_id, sid) in [("ev-sc-b", site_b), ("ev-sc-a", site_a)] {
        let p = SiteCheckinAfterEvent {
            site_id: sid.to_string(),
            action: "checkin".into(),
            state: "succeeded".into(),
            window_key: "2026-11-24".into(),
        };
        emit_evt(
            &env.state.store(),
            OutboxDb::App,
            ev_id,
            "site.checkin.after",
            Some(sid.to_string()),
            None,
            &p,
            now_ms,
        );
    }
    for (ev_id, lid) in [("ev-tc-b", lib_b), ("ev-tc-a", lib_a)] {
        let p = TransferCompletedEvent {
            media_id: "m-x".into(),
            library_id: lid.to_string(),
            relative_path: "F.mkv".into(),
            file_count: 1,
            total_bytes: 100,
        };
        emit_evt(
            &env.state.store(),
            OutboxDb::Library,
            ev_id,
            "transfer.completed",
            None,
            Some(lid.to_string()),
            &p,
            now_ms,
        );
    }

    api::drain_plugin_events_for_test(&env.state, now_ms).await;
    let st = env.state.store();
    for (db, ok_id, skip_id, pid) in [
        (OutboxDb::App, "ev-sc-a", "ev-sc-b", "org.example.evt-site"),
        (
            OutboxDb::Library,
            "ev-tc-a",
            "ev-tc-b",
            "org.example.evt-lib",
        ),
    ] {
        let ok = st.get_delivery_receipt(db, ok_id, pid).unwrap().unwrap();
        assert_eq!(ok.status, OutboxDeliveryStatus::Delivered);
        assert!(st.get_delivery_receipt(db, skip_id, pid).unwrap().is_none());
    }
}

#[tokio::test]
async fn defect_3_tmp_ttl_guard_and_dev_sideload_write_gate_and_artifact_gc() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let data_dir: PathBuf = env.state.store().data_dir().to_path_buf();
    let fresh_up_tmp = data_dir.join("plugins/uploads/.tmp-fresh-upload");
    let fresh_pkg_tmp = data_dir.join("plugins/packages/.tmp-abcdef-fresh");
    std::fs::create_dir_all(&fresh_up_tmp).unwrap();
    std::fs::create_dir_all(&fresh_pkg_tmp).unwrap();

    let pid = "org.example.dev-hardened";
    let mk_dev =
        |desc: &str| pkg::build_task_package_custom(pid, "1.0.0", 1, false, Some(desc), false, &[]);
    let req_dev = |bytes: Vec<u8>| {
        Request::builder()
            .method("POST")
            .uri("/api/v1/plugins/dev")
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("content-type", "application/octet-stream")
            .body(Body::from(bytes))
            .unwrap()
    };
    let (st1, _, b1) = env.send(req_dev(mk_dev("v1"))).await;
    let d1 = assert_ok(st1, &b1, StatusCode::CREATED);
    let old_pkg_dir = data_dir
        .join("plugins/packages")
        .join(d1["package_hash"].as_str().unwrap());
    assert!(old_pkg_dir.exists());

    let gate = env.state.write_gate();
    {
        let _closed = gate.close();
        let (st_busy, _, b_busy) = env.send(req_dev(mk_dev("v2-blocked"))).await;
        assert_err(
            st_busy,
            &b_busy,
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
        );
    }

    let (st2, _, b2) = env.send(req_dev(mk_dev("v2"))).await;
    let d2 = assert_ok(st2, &b2, StatusCode::OK);
    assert!(d2["generation"].as_i64().unwrap() > d1["generation"].as_i64().unwrap());
    assert!(
        !old_pkg_dir.exists(),
        "replaced dev artifact should be GC'd"
    );
    assert!(fresh_up_tmp.exists(), "fresh uploads/.tmp-* must survive");
    assert!(fresh_pkg_tmp.exists(), "fresh packages/.tmp-* must survive");
}

#[tokio::test]
async fn defect_4_stage_upload_write_gate_reauth_cleanup_and_actor_binding() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg_bytes = pkg::build_task_package("org.example.up-guard", "1.0.0", 1, false);
    let uploads_dir = env.state.store().data_dir().join("plugins/uploads");
    let mk_up_req = |idem: &str, uid: &str, reauth: &str| {
        req_multipart(
            "/api/v1/plugin-uploads",
            Some(TOKEN),
            &[("if-match", "\"0\""), ("idempotency-key", idem)],
            "----Bnd",
            &[
                ("upload_id", None, uid.as_bytes()),
                ("file", Some("p.cmplugin"), &pkg_bytes),
                ("reauth_token", None, reauth.as_bytes()),
            ],
        )
    };

    let reauth1 = env.reauth("plugin:upload").await;
    let blocked_id = Uuid::new_v4().to_string();
    let gate = env.state.write_gate();
    {
        let _closed = gate.close();
        let (st, _, body) = env.send(mk_up_req("up-gated", &blocked_id, &reauth1)).await;
        assert_err(st, &body, StatusCode::SERVICE_UNAVAILABLE, "runtime.busy");
        assert!(!uploads_dir.join(&blocked_id).exists());
    }

    let (ok_id, reuse_id) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
    let ((st_a, _, b_a), (st_b, _, b_b)) = tokio::join!(
        env.send(mk_up_req("up-ok-1", &ok_id, &reauth1)),
        env.send(mk_up_req("up-reuse-1", &reuse_id, &reauth1)),
    );
    let (win_id, lose_id) = if st_a == StatusCode::CREATED {
        assert_ok(st_a, &b_a, StatusCode::CREATED);
        assert_err(st_b, &b_b, StatusCode::FORBIDDEN, "plugin.reauth_invalid");
        (&ok_id, &reuse_id)
    } else {
        assert_ok(st_b, &b_b, StatusCode::CREATED);
        assert_err(st_a, &b_a, StatusCode::FORBIDDEN, "plugin.reauth_invalid");
        (&reuse_id, &ok_id)
    };
    assert!(uploads_dir.join(win_id).exists());
    assert!(!uploads_dir.join(lose_id).exists());

    let prep_in = PrepareInstallInput {
        actor_user_id: UserId::new(),
        upload_id: Some(Uuid::from_str(win_id).unwrap()),
        repo_id: None,
        plugin_id: None,
        version: None,
        idempotency_key: "prep-other-admin".into(),
        request: CanonicalPluginRequest {
            method: "POST".into(),
            path: "/plugin-installations/prepare".into(),
            body_digest: [7; 32],
        },
    };
    let err = env
        .state
        .plugins()
        .prepare_install(&env.state, prep_in)
        .await
        .unwrap_err();
    assert_eq!(err.status_and_code(), (403, "plugin.forbidden"));
}

#[tokio::test]
async fn defect_5_download_after_propagates_site_id_and_cred_bound_skips_siteless_events() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let site_a = env.insert_site("DlEventSite");
    let zip = build_policy_zip(
        "org.example.cb-events",
        true,
        "[[components.hooks]]\nid = \"download.after\"\npriority = 10\n[[components.hooks]]\nid = \"transfer.completed\"\npriority = 10\n",
        &[
            "http:site-auth",
            "hooks:download.after",
            "events:download.after",
            "hooks:transfer.completed",
            "events:transfer.completed",
        ],
    );
    let grants = json!([
        g_site("http:site-auth", site_a),
        g_global("hooks:download.after"),
        g_global("events:download.after"),
        g_global("hooks:transfer.completed"),
        g_global("events:transfer.completed"),
    ]);
    install_and_enable(&env, "org.example.cb-events", &zip, grants).await;

    let mag_a = "magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let mag_b = "magnet:?xt=urn:btih:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    post_ok(
        &env,
        "/api/v1/downloaders/submit",
        json!({ "download_url": mag_a, "title": "With.Site.1080p", "site_id": site_a.to_string() }),
    )
    .await;
    post_ok(
        &env,
        "/api/v1/downloaders/submit",
        json!({ "download_url": mag_b, "title": "No.Site.1080p" }),
    )
    .await;

    let now_ms = store::unix_now_ms().unwrap();
    let siteless_dl = DownloadAfterEvent {
        operation_id: "op-siteless".into(),
        downloader_id: None,
        info_hash: None,
        outcome: "submitted".into(),
        verified_total_bytes: Some(64),
    };
    emit_evt(
        &env.state.store(),
        OutboxDb::Subscribe,
        "ev-siteless-dl",
        "download.after",
        None,
        None,
        &siteless_dl,
        now_ms,
    );
    let tc_p = TransferCompletedEvent {
        media_id: "m-1".into(),
        library_id: "lib-1".into(),
        relative_path: "F.mkv".into(),
        file_count: 1,
        total_bytes: 64,
    };
    emit_evt(
        &env.state.store(),
        OutboxDb::Library,
        "ev-siteless-tc",
        "transfer.completed",
        None,
        Some("lib-1".into()),
        &tc_p,
        now_ms,
    );

    api::drain_plugin_events_for_test(&env.state, now_ms).await;
    let ok_ev = get_ok(
        &env,
        "/api/v1/plugins/org.example.cb-events/events?status=delivered",
    )
    .await;
    let dl_ev = get_ok(
        &env,
        "/api/v1/plugins/org.example.cb-events/events?status=dead_letter",
    )
    .await;
    assert_eq!(ok_ev["items"].as_array().unwrap().len(), 1);
    assert_eq!(dl_ev["items"].as_array().unwrap().len(), 0);
}

fn seed_boost_task_for(store: &Store, pid: &str, dl: DownloaderId, site: SiteId, id: &str) {
    let now = 1_700_000_000i64;
    let tag = format!("tag-{id}");
    let op_id = format!("op-{id}");
    let hash = format!(
        "{:0>40x}",
        id.bytes()
            .fold(0u64, |a, b| a.wrapping_mul(31).wrapping_add(u64::from(b)))
    );
    store
        .accept_download_operation(&AcceptDownloadOperationInput {
            operation_id: op_id.clone(),
            idempotency_key: format!("ik-{id}"),
            source_kind: "boost".into(),
            subscribe_id: None,
            plugin_id: Some(pid.into()),
            subject: pid.into(),
            cause_id: "test".into(),
            site_id: Some(site),
            downloader_id: Some(dl),
            endpoint_fingerprint: None,
            enclosure: format!("https://pt.example/{id}.torrent"),
            ownership_tag: tag.clone(),
            info_hash: Some(hash.clone()),
            bytes_digest: Some(format!("dig-{id}")),
            verified_bytes: Some(512),
            request_digest: format!("req-{id}"),
            now_utc: now,
        })
        .unwrap();
    store
        .record_download_ownership(&RecordDownloadOwnershipInput {
            downloader_id: dl,
            ownership_tag: tag.clone(),
            info_hash: Some(hash.clone()),
            operation_id: op_id,
            subscribe_id: None,
            plugin_id: Some(pid.into()),
            subject: pid.into(),
            site_id: Some(site),
            enclosure: format!("https://pt.example/{id}.torrent"),
            ownership_state: DownloadOwnershipState::Owned,
            provenance_kind: DownloadProvenanceKind::CreatedFresh,
            verified_bytes: Some(512),
            bytes_digest: Some(format!("dig-{id}")),
            now_utc: now,
        })
        .unwrap();
    store
        .upsert_boost_task_fact(&UpsertBoostTaskFactInput {
            task_fact_id: id.into(),
            plugin_id: pid.into(),
            quota_key: format!("{pid}:{dl}"),
            downloader_id: dl,
            ownership_tag: tag,
            info_hash: hash,
            site_id: Some(site),
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
    store.mark_boost_task_retained(id, 1, 512, now).unwrap();
    store
        .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: id.into(),
            expected_revision: 2,
            cleanup_plan_handle: format!("plan-{id}"),
            cleanup_plan_json: r#"{"files":["a.mkv"]}"#.into(),
            ttl_secs: 600,
            now_utc: now,
        })
        .unwrap();
}

#[tokio::test]
async fn defect_7_commands_submit_scopes_boost_handles_to_plugin_and_bound_site() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let (site_a, site_b) = (env.insert_site("BoostSiteA"), env.insert_site("BoostSiteB"));
    let (dl_ok, dl_other) = (
        env.insert_downloader("BoostDLOk"),
        env.insert_downloader("BoostDLOther"),
    );
    let (pid_a, pid_b) = ("org.example.boost-owner-a", "org.example.boost-owner-b");
    let caps = [
        "http:site-auth",
        "command:downloader.control",
        "command:downloader.prepare-cleanup",
        "command:downloader.delete-files",
    ];
    let zip_a = pkg::build_task_package_custom(pid_a, "1.0.0", 1, false, None, true, &caps);
    let grants = json!([
        g_site("http:site-auth", site_a),
        g_obj("command:downloader.control", "downloader", dl_ok),
        g_obj("command:downloader.prepare-cleanup", "downloader", dl_ok),
        g_obj("command:downloader.delete-files", "downloader", dl_ok),
    ]);
    install_and_enable(&env, pid_a, &zip_a, grants).await;

    let st = env.state.store();
    seed_boost_task_for(&st, pid_b, dl_other, site_a, "tf-foreign");
    seed_boost_task_for(&st, pid_a, dl_ok, site_b, "tf-other-site");
    seed_boost_task_for(&st, pid_a, dl_ok, site_a, "tf-own-site");

    let obs_a = st.observe_site(site_a).unwrap().unwrap();
    let ports = env.state.plugins().host_ports();
    let mut inv = Invocation::new(pid_a, Duration::from_secs(5));
    inv.generation = 1;
    inv.role = GuestRole::Task;
    inv.site_id = Some(site_a.to_string());
    inv.credential_epoch = Some(obs_a.credential_epoch().storage_key().to_string());
    let _g = ports.begin_invocation(&inv).unwrap();

    let rev = |h: &str| OwnedRevision {
        handle: h.into(),
        revision: 3,
    };
    let ctrl = |h: &str| CommandAction::DownloaderControl {
        target: rev(h),
        control: TaskControl::Pause,
    };
    let del_f = |h: &str, p: &str| CommandAction::DownloaderDeleteFiles {
        target: rev(h),
        plan_handle: p.into(),
    };
    let prep_c = |h: &str| CommandAction::DownloaderPrepareCleanup {
        targets: vec![rev(h)],
        reason: "q".into(),
    };
    let submit = |k: &str, action| {
        ports.commands_submit(
            &inv,
            CommandRequest {
                action,
                idempotency_key: k.into(),
                expected_revision: None,
            },
        )
    };
    for (k, bad) in [
        ("foreign-ctrl", ctrl("tf-foreign")),
        ("foreign-plan", del_f("tf-foreign", "plan-tf-foreign")),
        ("cross-site-prep", prep_c("tf-other-site")),
        (
            "cross-site-plan",
            del_f("tf-own-site", "plan-tf-other-site"),
        ),
    ] {
        assert_eq!(
            submit(k, bad).unwrap_err().code,
            ErrorCode::NotFound,
            "on {k}"
        );
    }
    assert!(Uuid::parse_str(&submit("own-site-ctrl", ctrl("tf-own-site")).unwrap()).is_ok());
}
