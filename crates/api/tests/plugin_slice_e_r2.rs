#[path = "builtin_boost/support.rs"]
pub(crate) mod support;

use std::collections::BTreeMap;
use std::sync::Arc;

use api::{ApiState, PluginInvocationRole, Store, SubmitPluginCommand, router};
use axum::http::{StatusCode, header};
use domain::{
    Coverage, DownloaderId, FetchMode, Filter, FilterId, Media, MediaId, MediaKind,
    PluginOperationId, Site, SiteId, Subscribe, SubscribeId, Torrent,
};
use indexer::{IndexerError, RegistrySnapshot};
use plugin_package::build_deterministic_cmplugin_zip;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use store::plugins::{
    CanonicalPluginRequest, PluginCommandCause, PluginCommandTarget, PluginOperationState,
    PluginSubject,
};
use store::{AcceptDownloadOperationInput, BoostTaskState, Db};
use support::plugin_http_support::{
    ADMIN_PASS, HttpEnv, TOKEN, admin_id, assert_err, assert_ok,
    pkg::{self, TestSigningKey},
    req_json,
};
use support::{BoostTestDownloader, NoopFetcher, StubTorrentServer, poll_op, seed_boost_task};
use uuid::Uuid;

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn build_ui_pkg_ver(
    pid: &str,
    version: &str,
    remote: &str,
    caps: &[&str],
    worker_perms: &[&str],
) -> Vec<u8> {
    let js = format!("export const remote = '{remote}_{version}';\n");
    let asset_manifest = serde_json::to_vec(&json!({
        "version": 1,
        "files": [{ "path": "ui/assets/remoteEntry.js", "sha256": sha256_hex(js.as_bytes()), "size": js.len(), "content_type": "application/javascript" }]
    }))
    .unwrap();
    let comp_block = if worker_perms.is_empty() {
        String::new()
    } else {
        format!(
            "[[components]]\nid=\"worker\"\nentry=\"components/worker.wasm\"\nworld=\"{}\"\npermissions={}\n[[components.tasks]]\nid=\"sync\"\ninput_schema=\"schemas/task.schema.json\"\nschedule={{kind=\"interval\",seconds=3600}}\n",
            plugin_host::TASK_WORLD,
            serde_json::to_string(worker_perms).unwrap()
        )
    };
    let manifest = format!(
        "manifest_version=1\nid=\"{pid}\"\nname=\"SliceER2\"\nversion=\"{version}\"\napi=\"1.0.0\"\nlicense=\"MIT\"\nauthors=[\"T\"]\ndescription=\"r2\"\nconfig_schema=\"schemas/config.schema.json\"\nstate_schema=1\n{comp_block}[ui]\nkind=\"federation-react\"\ncontract=\"1.0.0\"\nreact=\"19.3.0\"\nreact_dom=\"19.3.0\"\nremote_name=\"{remote}\"\nentry=\"ui/assets/remoteEntry.js\"\nasset_manifest=\"ui/asset-manifest.json\"\ncapabilities={}\n[[ui.surfaces]]\nkind=\"config\"\nid=\"settings\"\nexpose=\"./Config\"\ntitle=\"Cfg\"\n[[ui.surfaces]]\nkind=\"page\"\nid=\"main\"\nexpose=\"./Page\"\ntitle=\"Main\"\n",
        serde_json::to_string(caps).unwrap()
    );
    let mut entries = BTreeMap::from([
        ("plugin.toml".into(), manifest.into_bytes()),
        (
            "schemas/config.schema.json".into(),
            br#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#
                .to_vec(),
        ),
        ("ui/asset-manifest.json".into(), asset_manifest),
        ("ui/assets/remoteEntry.js".into(), js.into_bytes()),
    ]);
    if !worker_perms.is_empty() {
        entries.insert(
            "schemas/task.schema.json".into(),
            br#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{}}"#
                .to_vec(),
        );
        entries.insert("components/worker.wasm".into(), pkg::task_wasm().to_vec());
    }
    build_deterministic_cmplugin_zip(&entries).unwrap()
}

async fn prepare_trust_http(env: &HttpEnv, pid: &str) -> Value {
    let (st, _, b) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/prepare"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    assert_ok(st, &b, StatusCode::OK)
}

async fn trust_ui_http(env: &HttpEnv, pid: &str) -> String {
    let prep = prepare_trust_http(env, pid).await;
    let hash = prep["package_hash"].as_str().unwrap().to_string();
    let reauth = env.reauth("plugin-ui-trust").await;
    let idem = format!("tg-{}", Uuid::new_v4().simple());
    let (st, _, b) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/grant"),
            Some(TOKEN),
            &[("if-match", "\"0\""), ("idempotency-key", &idem)],
            Some(json!({
                "preparation_id": prep["preparation_id"],
                "package_hash": prep["package_hash"],
                "consent_token": prep["consent_token"],
                "reauth_token": reauth,
            })),
        ))
        .await;
    assert_ok(st, &b, StatusCode::OK);
    hash
}

async fn login_admin_cookie(env: &HttpEnv) -> String {
    let req = req_json(
        "POST",
        "/api/v1/auth/login",
        None,
        &[],
        Some(json!({ "username": "admin", "password": ADMIN_PASS })),
    );
    let (_, hdrs, _) = env.send(req).await;
    let sc = hdrs.get(header::SET_COOKIE).unwrap().to_str().unwrap();
    sc.split(';')
        .next()
        .unwrap()
        .strip_prefix("mc_session=")
        .unwrap()
        .to_string()
}

fn queue_cmd_direct(
    state: &ApiState,
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
                body_digest: [7; 32],
            },
            expected_revision: Some(p.revision),
            cause: PluginCommandCause::root("r2-exec"),
            payload: serde_json::to_vec(&payload).unwrap(),
        },
    )
    .unwrap()
    .operation;
    PluginOperationId::from_uuid(op.operation_id)
}

#[tokio::test]
async fn slice_e_r2_item6_ui_trust_atomic_tokens_package_conflict_and_consent_stale() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(806);
    env.pin_trust_key(&key, "k-806").await;

    let (pid_a, pid_b) = ("org.example.uitrust.a", "org.example.uitrust.b");
    for (pid, rem) in [(pid_a, "uitrustARemote"), (pid_b, "uitrustBRemote")] {
        let pkg = build_ui_pkg_ver(pid, "1.0.0", rem, &[], &[]);
        env.install_pkg(&pkg, Some(&key.sign_package(&pkg)), json!([]))
            .await;
    }
    let (prep_a, prep_b) = (
        prepare_trust_http(&env, pid_a).await,
        prepare_trust_http(&env, pid_b).await,
    );
    let shared_reauth = env.reauth("plugin-ui-trust").await;
    let mk_grant = |pid: &str, prep: &Value, idem: &str, reauth: &str| {
        req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/grant"),
            Some(TOKEN),
            &[("if-match", "\"0\""), ("idempotency-key", idem)],
            Some(json!({
                "preparation_id": prep["preparation_id"],
                "package_hash": prep["package_hash"],
                "consent_token": prep["consent_token"],
                "reauth_token": reauth,
            })),
        )
    };
    let ((st_a, _, b_a), (st_b, _, b_b)) = {
        let store_bg = env.state.store();
        let (tx, rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            store_bg
                .app_write(|_| {
                    let _ = tx.send(());
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    Ok(())
                })
                .unwrap();
        });
        rx.recv().unwrap();
        let res = tokio::join!(
            env.send(mk_grant(pid_a, &prep_a, "uit-a-race", &shared_reauth)),
            env.send(mk_grant(pid_b, &prep_b, "uit-b-race", &shared_reauth)),
        );
        holder.join().unwrap();
        res
    };
    let ok_cnt = usize::from(st_a == StatusCode::OK) + usize::from(st_b == StatusCode::OK);
    assert_eq!(ok_cnt, 1, "a=({st_a},{b_a}), b=({st_b},{b_b})");
    let (loser_pid, loser_prep, loser_st, loser_body) = if st_a == StatusCode::OK {
        (pid_b, &prep_b, st_b, &b_b)
    } else {
        (pid_a, &prep_a, st_a, &b_a)
    };
    assert_err(
        loser_st,
        loser_body,
        StatusCode::FORBIDDEN,
        "plugin.reauth_invalid",
    );
    assert!(
        env.state
            .store()
            .get_ui_trust(loser_pid, loser_prep["package_hash"].as_str().unwrap())
            .unwrap()
            .is_none()
    );
    let fresh_reauth = env.reauth("plugin-ui-trust").await;
    let (st_l2, _, b_l2) = env
        .send(mk_grant(
            loser_pid,
            loser_prep,
            "uit-loser-ok",
            &fresh_reauth,
        ))
        .await;
    assert_ok(st_l2, &b_l2, StatusCode::OK);
}

fn insert_dl(store: &Store) -> DownloaderId {
    let dl_id = DownloaderId::new();
    store
        .insert_downloader(&store::DownloaderRow {
            id: dl_id,
            name: "DL".into(),
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
    dl_id
}

#[tokio::test]
async fn slice_e_r2_item7_downloader_add_requires_covering_site_grant() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(807);
    env.pin_trust_key(&key, "k-807").await;
    let (site_a, site_b) = (env.insert_site("PT-A"), env.insert_site("PT-B"));
    let dl_id = insert_dl(env.state.store().as_ref());

    let pid = "org.example.dladd.r2";
    let pkg_bytes = build_ui_pkg_ver(
        pid,
        "1.0.0",
        "dladdRemote",
        &[],
        &["command:downloader.add", "read:sites"],
    );
    env.install_pkg(
        &pkg_bytes,
        Some(&key.sign_package(&pkg_bytes)),
        json!([
            { "capability": "command:downloader.add", "scope": "Global", "sensitive": false },
            { "capability": "read:sites", "scope": { "Site": site_a.to_string() }, "sensitive": false }
        ]),
    )
    .await;
    let rev = env.get_plugin_rev(pid).await;
    env.enable_plugin(pid, rev, "en-dladd").await;
    let rev = env.get_plugin_rev(pid).await;

    let send_add = |idem: &'static str, sid: String| {
        req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/commands"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", idem),
            ],
            Some(json!({
                "kind": "downloader.add",
                "payload": { "downloader_id": dl_id.to_string(), "site_id": sid, "download_ref": "https://pt.example/1.torrent" }
            })),
        )
    };
    let (st_bad, _, b_bad) = env.send(send_add("dl-bad", "not-uuid".into())).await;
    assert_err(
        st_bad,
        &b_bad,
        StatusCode::BAD_REQUEST,
        "plugin.invalid_input",
    );

    let (st_unauth, _, b_unauth) = env.send(send_add("dl-unauth", site_b.to_string())).await;
    assert_err(
        st_unauth,
        &b_unauth,
        StatusCode::FORBIDDEN,
        "plugin.forbidden",
    );

    let op_id = queue_cmd_direct(
        &env.state,
        pid,
        "downloader.add",
        vec![PluginCommandTarget::Downloader(dl_id)],
        json!({ "downloader_id": dl_id.to_string(), "site_id": site_b.to_string(), "download_ref": "https://pt.example/1.torrent" }),
        "dl-exec-unauth",
    );
    api::run_plugin_command_for_test(&env.state, op_id).unwrap();
    let op = env
        .state
        .store()
        .get_plugin_operation(&PluginSubject::User(admin_id()), op_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(
        (op.state, op.phase.as_str()),
        (PluginOperationState::Rejected, "site-unauthorized")
    );

    let (st_ok, _, _) = env.send(send_add("dl-ok", site_a.to_string())).await;
    assert_eq!(st_ok, StatusCode::ACCEPTED);
}

struct StubIdx(String, indexer::ProviderDescriptor);
impl indexer::IndexerProvider for StubIdx {
    fn descriptor(&self) -> &indexer::ProviderDescriptor {
        &self.1
    }
    fn search(
        &self,
        _: &indexer::ProviderCallContext<'_>,
        site: &Site,
        _: &indexer::SearchQuery<'_>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        Ok(vec![Torrent {
            site_id: site.id,
            id: Some("c-0".into()),
            title: "Free.0".into(),
            enclosure: self.0.clone(),
            size_bytes: Some(2048),
            seeders: Some(10),
            leechers: Some(1),
            snatched: None,
            upload_time: None,
            poster_url: None,
            free: true,
            hr: Some(false),
            imdb_id: None,
            detail_url: None,
            category: None,
        }])
    }
    fn rss(
        &self,
        _: &indexer::ProviderCallContext<'_>,
        _: &Site,
        _: Option<&str>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        Ok(vec![])
    }
    fn resolve_download(
        &self,
        _: &indexer::ProviderCallContext<'_>,
        _: &Site,
        _: &Torrent,
    ) -> Result<Option<indexer::ResolvedDownload>, IndexerError> {
        Ok(None)
    }
}

fn setup_item8_boost_env() -> (HttpEnv, SiteId, SiteId, DownloaderId, StubTorrentServer) {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let srv = StubTorrentServer::start_dynamic(2048);
    let dl = Arc::new(BoostTestDownloader::new(dir.path().join("stage")));
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let desc = indexer::ProviderDescriptor {
        id: "r2_boost_idx".into(),
        name: "R2Idx".into(),
        kind: "builtin".into(),
        source: indexer::ProviderSource::Builtin,
        auth_types: vec![indexer::AuthType::Cookie],
        capabilities: vec![indexer::ProviderCapability::Search],
        availability: indexer::ProviderAvailability::Enabled,
        hidden_from_catalog: false,
    };
    let reg = indexer::RegistryCandidate::from_builtin()
        .unwrap()
        .add_indexer_provider(Arc::new(StubIdx(srv.url("cand-a.torrent"), desc)))
        .unwrap();
    let state = ApiState::new_arc(
        store,
        TOKEN.into(),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl,
    )
    .unwrap()
    .with_plugin_http_policy_override(
        net_guard::TargetPolicy::production().with_test_loopback_socket(srv.addr()),
    );
    state.indexer().publish(reg).unwrap();
    let env = HttpEnv {
        _dir: dir,
        app: router(state.clone()),
        state,
        member_token: String::new(),
    };
    let (site_a, site_b) = (env.insert_site("PT-A"), env.insert_site("PT-B"));
    let dl_id = insert_dl(env.state.store().as_ref());
    env.state
        .store()
        .with_writer(Db::App, |conn| {
            conn.execute("UPDATE sites SET profile_id = 'r2_boost_idx'", [])?;
            Ok(())
        })
        .unwrap();
    (env, site_a, site_b, dl_id, srv)
}

#[tokio::test]
async fn slice_e_r2_item8_boost_cycle_uses_downloader_wide_snapshot() {
    let (env, site_a, site_b, dl_id, _srv) = setup_item8_boost_env();
    seed_boost_task(
        env.state.store().as_ref(),
        dl_id,
        site_b,
        "other-site-tag",
        "bbbb000011111111111111111111111111111111",
        BoostTaskState::Seeding,
        10_000,
        0,
        None,
        None,
    );
    support::save_boost_config(
        &env.state,
        json!({
            "sites": [site_a.to_string()],
            "downloader_id": dl_id.to_string(),
            "max_active": 1,
            "max_adds_per_round": 2,
            "min_free_disk_bytes": 0,
            "dry_run": false,
        }),
    );
    let rev = env.get_plugin_rev("builtin.boost").await;

    let (st_run, _, b_run) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/builtin.boost/commands",
            Some(TOKEN),
            &[("if-match", &format!("\"{rev}\"")), ("idempotency-key", "b8-run")],
            Some(json!({
                "kind": "boost.run",
                "payload": { "site_id": site_a.to_string(), "downloader_id": dl_id.to_string(), "dry_run": false }
            })),
        ))
        .await;
    let op_id = assert_ok(st_run, &b_run, StatusCode::ACCEPTED)["operation_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(poll_op(&env, &op_id).await["state"], "succeeded");
    let snap = env
        .state
        .store()
        .get_boost_snapshot_for_plugin("builtin.boost", &[], &[dl_id])
        .unwrap();
    assert_eq!(
        snap.tasks.len(),
        1,
        "must not add when downloader-wide max_active=1 is full"
    );
}

#[tokio::test]
async fn slice_e_r2_item9_indexer_search_targets_fence_and_sites_validation() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(809);
    env.pin_trust_key(&key, "k-809").await;
    let (site_a, site_b) = (env.insert_site("Idx-A"), env.insert_site("Idx-B"));

    let pid = "org.example.idxsearch.r2";
    let pkg_bytes = build_ui_pkg_ver(
        pid,
        "1.0.0",
        "idxSearchR2Remote",
        &["read:sites", "command:indexer.search"],
        &[],
    );
    env.install_pkg(
        &pkg_bytes,
        Some(&key.sign_package(&pkg_bytes)),
        json!([{ "capability": "command:indexer.search", "scope": "Global", "sensitive": false }]),
    )
    .await;
    let rev = env.get_plugin_rev(pid).await;
    env.enable_plugin(pid, rev, "en-idx9").await;
    let _ = trust_ui_http(&env, pid).await;

    let (_, _, bh) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui/query"),
            Some(TOKEN),
            &[],
            Some(json!({ "name": "handles.resolve", "input": { "kind": "site", "id": site_a } })),
        ))
        .await;
    let h_a = assert_ok(StatusCode::OK, &bh, StatusCode::OK)["handle"]
        .as_str()
        .unwrap()
        .to_string();

    let rev = env.get_plugin_rev(pid).await;
    let post_srch = |idem: &'static str, payload: Value| {
        req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/commands"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", idem),
            ],
            Some(json!({ "kind": "indexer.search", "payload": payload })),
        )
    };
    let (st_empty, _, b_empty) = env
        .send(post_srch(
            "idx9-empty",
            json!({ "sites": [], "keyword": "Dune" }),
        ))
        .await;
    assert_eq!(st_empty, StatusCode::BAD_REQUEST, "body={b_empty}");
    assert!(
        b_empty["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("sites must not be empty")
    );

    let (st_both, _, b_both) = env
        .send(post_srch(
            "idx9-both",
            json!({ "sites": [h_a.clone()], "site_handle": h_a, "keyword": "Dune" }),
        ))
        .await;
    assert_eq!(st_both, StatusCode::BAD_REQUEST, "body={b_both}");

    let op_id = queue_cmd_direct(
        &env.state,
        pid,
        "indexer.search",
        vec![PluginCommandTarget::Site(site_a)],
        json!({ "site_id": site_b.to_string(), "keyword": "Dune" }),
        "idx9-exec-fence",
    );
    api::run_plugin_command_for_test(&env.state, op_id).unwrap();
    let op = env
        .state
        .store()
        .get_plugin_operation(&PluginSubject::User(admin_id()), op_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(
        (op.state, op.phase.as_str()),
        (PluginOperationState::Rejected, "target-mismatch")
    );
}

fn seed_item10_data(env: &HttpEnv) -> (SiteId, DownloaderId, MediaId) {
    let (site_a, site_b) = (env.insert_site("Q-A"), env.insert_site("Q-B"));
    let (dl_a, dl_b) = (DownloaderId::new(), DownloaderId::new());
    for (did, op_id, sid) in [(dl_a, "op-dl-a", site_a), (dl_b, "op-dl-b", site_b)] {
        env.state
            .store()
            .accept_download_operation(&AcceptDownloadOperationInput {
                operation_id: op_id.into(),
                idempotency_key: format!("idem-{op_id}"),
                source_kind: "plugin".into(),
                subscribe_id: None,
                plugin_id: None,
                subject: "admin".into(),
                cause_id: "c1".into(),
                site_id: Some(sid),
                downloader_id: Some(did),
                endpoint_fingerprint: None,
                enclosure: "https://pt.example/1.torrent".into(),
                ownership_tag: format!("tag-{op_id}"),
                info_hash: Some("1111111111111111111111111111111111111111".into()),
                bytes_digest: None,
                verified_bytes: Some(512),
                request_digest: format!("dig-{op_id}"),
                now_utc: 1_700_000_000,
            })
            .unwrap();
    }
    let filter_id = FilterId::new();
    env.state
        .store()
        .insert_filter(&Filter {
            id: filter_id,
            name: "F".into(),
            atoms: vec![],
            keep_old_versions: false,
        })
        .unwrap();
    let (media_a, media_b) = (MediaId::new(), MediaId::new());
    for mid in [media_a, media_b] {
        env.state
            .store()
            .insert_media(&Media {
                id: mid,
                kind: MediaKind::Movie,
                title: "M".into(),
                year: Some(2025),
                original_title: None,
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
                aliases: vec![],
            })
            .unwrap();
        env.state
            .store()
            .insert_subscribe(&Subscribe {
                id: SubscribeId::new(),
                user_id: admin_id(),
                media_id: mid,
                coverage: Coverage::Movie,
                fetch_mode: FetchMode::Search,
                filter_id,
                wash_cut: false,
                wash_cut_filter_id: None,
                keep_old_versions: false,
                full_season_pack: false,
                downloader_id: None,
                library_id: None,
                tracking_state: "active".into(),
                follow_future: false,
                search_interval_secs: 1800,
            })
            .unwrap();
    }
    (site_a, dl_a, media_a)
}

#[tokio::test]
async fn slice_e_r2_item10_ui_queries_bundle_sha_fail_closed_and_brush_attribution() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(810);
    env.pin_trust_key(&key, "k-810").await;
    let (site_a, dl_a, media_a) = seed_item10_data(&env);

    let pid = "org.example.uiq.r2";
    let pkg_bytes = build_ui_pkg_ver(
        pid,
        "1.0.0",
        "uiqR2Remote",
        &["read:sites", "read:downloads", "read:subscribes"],
        &[],
    );
    env.install_pkg(&pkg_bytes, Some(&key.sign_package(&pkg_bytes)), json!([]))
        .await;
    let rev = env.get_plugin_rev(pid).await;
    env.enable_plugin(pid, rev, "en-uiq").await;
    let pkg_hash = trust_ui_http(&env, pid).await;
    let gen_str = env
        .state
        .store()
        .get_plugin(pid)
        .unwrap()
        .unwrap()
        .generation
        .to_string();
    let attr = [
        ("x-plugin-package", pkg_hash.as_str()),
        ("x-plugin-generation", gen_str.as_str()),
    ];
    let ui_q = |name: &'static str, input: Value| {
        req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui/query"),
            Some(TOKEN),
            &attr,
            Some(json!({ "name": name, "input": input })),
        )
    };

    let (st_dl, _, b_dl) = env
        .send(ui_q(
            "downloads.list",
            json!({ "site_id": site_a, "downloader_id": dl_a }),
        ))
        .await;
    let dl_page = assert_ok(st_dl, &b_dl, StatusCode::OK);
    assert_eq!(dl_page["items"].as_array().unwrap().len(), 1);
    assert_eq!(dl_page["items"][0]["site_id"], site_a.to_string());
    assert_eq!(dl_page["items"][0]["revision"], "1");

    let (st_sub, _, b_sub) = env
        .send(ui_q("subscribes.list", json!({ "media_id": media_a })))
        .await;
    let sub_page = assert_ok(st_sub, &b_sub, StatusCode::OK);
    assert_eq!(sub_page["items"].as_array().unwrap().len(), 1);
    assert_eq!(sub_page["items"][0]["media_id"], media_a.to_string());

    let (st_hr, _, b_hr) = env
        .send(ui_q(
            "handles.resolve",
            json!({ "kind": "site", "id": site_a, "expected_revision": "1" }),
        ))
        .await;
    assert_ok(st_hr, &b_hr, StatusCode::OK);

    let (st_bs, _, b_bs) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}/brush-state"),
            Some(TOKEN),
            &attr,
            None,
        ))
        .await;
    assert_err(
        st_bs,
        &b_bs,
        StatusCode::FORBIDDEN,
        "plugin.capability_denied",
    );

    env.state
        .store()
        .with_writer(Db::App, |conn| {
            conn.execute(
                "UPDATE plugin_ui_trust SET bundle_sha256 = '   ' WHERE plugin_id = ?1",
                rusqlite::params![pid],
            )?;
            Ok(())
        })
        .unwrap();
    let cookie = format!("mc_session={}", login_admin_cookie(&env).await);
    let asset_uri = format!("/api/v1/plugin-ui-assets/{pid}/{pkg_hash}/ui/assets/remoteEntry.js");
    let (st_asset, _, b_asset) = env
        .send(req_json(
            "GET",
            &asset_uri,
            None,
            &[("cookie", &cookie)],
            None,
        ))
        .await;
    assert_err(
        st_asset,
        &b_asset,
        StatusCode::FORBIDDEN,
        "plugin.ui_asset_corrupted",
    );
}
