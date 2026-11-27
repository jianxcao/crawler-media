#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use std::collections::BTreeMap;

use axum::http::StatusCode;
use plugin_http_support::{
    HttpEnv, TOKEN, assert_err, assert_ok,
    pkg::{TestSigningKey, task_wasm},
    req_json,
};
use plugin_package::build_deterministic_cmplugin_zip;
use serde_json::json;
use sha2::{Digest, Sha256};
use store::plugins::{PluginCommandOutcome, PluginEffectObservation, PluginOperationState};
use uuid::Uuid;

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn build_ui_pkg(
    plugin_id: &str,
    remote_name: &str,
    ui_caps: &[&str],
    comp_perms: Option<&[&str]>,
    css_asset: Option<&str>,
) -> Vec<u8> {
    let remote_js = b"export const remote = true;\n";
    let mut files = vec![json!({
        "path": "ui/assets/remoteEntry.js",
        "sha256": sha256_hex(remote_js),
        "size": remote_js.len(),
        "content_type": "application/javascript"
    })];
    let mut entries = BTreeMap::new();
    entries.insert("ui/assets/remoteEntry.js".into(), remote_js.to_vec());
    if let Some(css_path) = css_asset {
        let css_bytes = b".cm-remediation { color: inherit; }\n";
        files.push(json!({
            "path": css_path,
            "sha256": sha256_hex(css_bytes),
            "size": css_bytes.len(),
            "content_type": "text/css"
        }));
        entries.insert(css_path.to_string(), css_bytes.to_vec());
    }
    let asset_manifest = serde_json::to_vec(&json!({ "version": 1, "files": files })).unwrap();
    let comp_toml = if let Some(perms) = comp_perms {
        let perms_json = serde_json::to_string(perms).unwrap();
        entries.insert("components/worker.wasm".into(), task_wasm().to_vec());
        entries.insert(
            "schemas/task.schema.json".into(),
            br#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#.to_vec(),
        );
        format!(
            r#"
[[components]]
id = "worker"
entry = "components/worker.wasm"
world = "crawler:plugin/task-plugin@1.0.0"
permissions = {perms_json}

[[components.tasks]]
id = "sync"
input_schema = "schemas/task.schema.json"
[components.tasks.schedule]
kind = "interval"
seconds = 300
"#
        )
    } else {
        String::new()
    };
    let ui_caps_json = serde_json::to_string(ui_caps).unwrap();
    let manifest = format!(
        r#"manifest_version = 1
id = "{plugin_id}"
name = "UI Remediation Plugin"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test"]
description = "UI review remediation fixture"
config_schema = "schemas/config.schema.json"
state_schema = 1
{comp_toml}
[ui]
kind = "federation-react"
contract = "1.0.0"
react = "19.3.0"
react_dom = "19.3.0"
remote_name = "{remote_name}"
entry = "ui/assets/remoteEntry.js"
asset_manifest = "ui/asset-manifest.json"
capabilities = {ui_caps_json}

[[ui.surfaces]]
kind = "config"
id = "settings"
expose = "./Config"
title = "Settings"

[[ui.surfaces]]
kind = "page"
id = "main"
expose = "./Page"
title = "Main"
"#
    );
    entries.insert("plugin.toml".into(), manifest.into_bytes());
    entries.insert(
        "schemas/config.schema.json".into(),
        br#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#.to_vec(),
    );
    entries.insert("ui/asset-manifest.json".into(), asset_manifest);
    build_deterministic_cmplugin_zip(&entries).expect("zip")
}

async fn grant_ui_trust(env: &HttpEnv, pid: &str) {
    let (st_p, _, b_p) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/prepare"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    let prep = assert_ok(st_p, &b_p, StatusCode::OK);
    let rev = prep["trust_revision"].as_i64().unwrap_or(0);
    let reauth = env.reauth("plugin-ui-trust").await;
    let (st_g, _, b_g) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/grant"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", &format!("ug-{}", Uuid::new_v4())),
            ],
            Some(json!({
                "preparation_id": prep["preparation_id"],
                "package_hash": prep["package_hash"],
                "consent_token": prep["consent_token"],
                "reauth_token": reauth,
            })),
        ))
        .await;
    assert_ok(st_g, &b_g, StatusCode::OK);
}

#[tokio::test]
async fn disabled_trusted_plugin_allows_config_options_while_sites_list_returns_403_and_exposes_styles()
 {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(701);
    env.pin_trust_key(&key, "k-701").await;
    let site_id = env.insert_site("Disabled Config Site");

    let pid = "org.example.disabledcfg";
    let pkg = build_ui_pkg(
        pid,
        "disabledCfgRemote",
        &["read:sites", "read:downloaders"],
        None,
        Some("ui/assets/style-8f9a1b.css"),
    );
    let pkg_hash = sha256_hex(&pkg);
    env.install_pkg(&pkg, Some(&key.sign_package(&pkg)), json!([]))
        .await;
    grant_ui_trust(&env, pid).await;

    // Descriptor exposes discovered .css paths from asset-manifest.json
    let (st_desc, _, b_desc) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}/ui"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let desc = assert_ok(st_desc, &b_desc, StatusCode::OK);
    assert_eq!(desc["styles"], json!(["ui/assets/style-8f9a1b.css"]));
    let gen_str = desc["generation"].as_i64().unwrap().to_string();

    // Operational query sites.list still returns 403 plugin.not_enabled when disabled
    let (st_sites, _, b_sites) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui/query"),
            Some(TOKEN),
            &[
                ("x-plugin-package", &pkg_hash),
                ("x-plugin-generation", &gen_str),
            ],
            Some(json!({ "name": "sites.list", "input": {} })),
        ))
        .await;
    assert_err(
        st_sites,
        &b_sites,
        StatusCode::FORBIDDEN,
        "plugin.not_enabled",
    );

    // config.options succeeds on disabled UI-trusted plugin with sanitized items
    let (st_opt, _, b_opt) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui/query"),
            Some(TOKEN),
            &[
                ("x-plugin-package", &pkg_hash),
                ("x-plugin-generation", &gen_str),
            ],
            Some(json!({ "name": "config.options", "input": { "kind": "sites", "limit": 20 } })),
        ))
        .await;
    let opt = assert_ok(st_opt, &b_opt, StatusCode::OK);
    assert!(!opt.to_string().contains("site-secret-cookie"));
    assert!(opt["next_cursor"].is_null());
    assert!(
        opt["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == site_id.to_string())
    );

    // Missing read:subscribes|read:catalog capability for filters -> 403
    let (st_forb, _, b_forb) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui/query"),
            Some(TOKEN),
            &[
                ("x-plugin-package", &pkg_hash),
                ("x-plugin-generation", &gen_str),
            ],
            Some(json!({ "name": "config.options", "input": { "kind": "filters" } })),
        ))
        .await;
    assert_err(
        st_forb,
        &b_forb,
        StatusCode::FORBIDDEN,
        "plugin.capability_denied",
    );
}

#[tokio::test]
async fn ui_attributed_writes_and_cleanup_preview_reject_missing_ui_capabilities() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(702);
    env.pin_trust_key(&key, "k-702").await;
    let site_id = env.insert_site("Cap Guard Site");

    let pid = "org.example.uicapguard";
    let comp_perms = &[
        "kv",
        "command:site.maintenance",
        "command:downloader.delete-files",
    ];
    // [ui].capabilities intentionally lacks command:* and task:run:sync!
    let pkg = build_ui_pkg(
        pid,
        "uiCapGuardRemote",
        &["read:sites"],
        Some(comp_perms),
        None,
    );
    let pkg_hash = sha256_hex(&pkg);
    let grants = json!([
        { "capability": "kv", "scope": "Global", "sensitive": false },
        { "capability": "command:site.maintenance", "scope": "Global", "sensitive": false },
        { "capability": "command:downloader.delete-files", "scope": "Global", "sensitive": true },
    ]);
    env.install_pkg(&pkg, Some(&key.sign_package(&pkg)), grants)
        .await;
    let rev = env.get_plugin_rev(pid).await;
    env.enable_plugin(pid, rev, "en-capguard").await;
    grant_ui_trust(&env, pid).await;

    let attr_headers = [
        ("x-plugin-package", pkg_hash.as_str()),
        ("x-plugin-generation", "1"),
    ];

    // 1. UI-attributed POST /commands rejected with 403 when [ui].capabilities lacks command:site.maintenance
    let (st_cmd, _, b_cmd) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/commands"),
            Some(TOKEN),
            &[
                attr_headers[0],
                attr_headers[1],
                ("idempotency-key", "cap-cmd-1"),
            ],
            Some(json!({
                "kind": "site.maintenance",
                "payload": { "site_id": site_id, "action": "check-in" }
            })),
        ))
        .await;
    assert_err(
        st_cmd,
        &b_cmd,
        StatusCode::FORBIDDEN,
        "plugin.capability_denied",
    );

    // 2. UI-attributed POST /tasks/:id/run rejected with 403 when [ui].capabilities lacks task:run:sync
    let (st_run, _, b_run) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/tasks/sync/run"),
            Some(TOKEN),
            &[
                attr_headers[0],
                attr_headers[1],
                ("if-match", "\"0\""),
                ("idempotency-key", "cap-task-1"),
            ],
            Some(json!({ "input": {} })),
        ))
        .await;
    assert_err(
        st_run,
        &b_run,
        StatusCode::FORBIDDEN,
        "plugin.capability_denied",
    );

    // 3. UI-attributed GET /brush-cleanup-preview rejected with 403 when [ui].capabilities lacks command:downloader.delete-files
    let (st_prev, _, b_prev) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}/brush-cleanup-preview?task_ids=t1"),
            Some(TOKEN),
            &attr_headers,
            None,
        ))
        .await;
    assert_err(
        st_prev,
        &b_prev,
        StatusCode::FORBIDDEN,
        "plugin.capability_denied",
    );
}

#[tokio::test]
async fn ui_attributed_commands_require_opaque_handles_over_raw_ids() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(703);
    env.pin_trust_key(&key, "k-703").await;
    let site_id = env.insert_site("Handle Guard Site");

    let pid = "org.example.uihandleguard";
    let pkg = build_ui_pkg(
        pid,
        "uiHandleGuardRemote",
        &[
            "read:sites",
            "command:site.maintenance",
            "command:downloader.delete-task",
            "command:downloader.prepare-cleanup",
            "command:indexer.search",
        ],
        None,
        None,
    );
    let pkg_hash = sha256_hex(&pkg);
    let grants = json!([
        { "capability": "command:site.maintenance", "scope": "Global", "sensitive": false },
        { "capability": "command:downloader.delete-task", "scope": "Global", "sensitive": false },
        { "capability": "command:downloader.prepare-cleanup", "scope": "Global", "sensitive": false },
        { "capability": "command:indexer.search", "scope": "Global", "sensitive": false },
    ]);
    env.install_pkg(&pkg, Some(&key.sign_package(&pkg)), grants)
        .await;
    let rev = env.get_plugin_rev(pid).await;
    env.enable_plugin(pid, rev, "en-hdlguard").await;
    grant_ui_trust(&env, pid).await;

    for (idx, (kind, raw_payload)) in [
        (
            "site.maintenance",
            json!({ "site_id": site_id, "action": "check-in" }),
        ),
        ("downloader.delete-task", json!({ "task_id": "raw-task-1" })),
        (
            "downloader.prepare-cleanup",
            json!({ "task_ids": ["raw-task-1"] }),
        ),
        (
            "indexer.search",
            json!({ "site_id": site_id, "keyword": "Dune" }),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let (st, _, body) = env
            .send(req_json(
                "POST",
                &format!("/api/v1/plugins/{pid}/commands"),
                Some(TOKEN),
                &[
                    ("x-plugin-package", &pkg_hash),
                    ("x-plugin-generation", "1"),
                    ("idempotency-key", &format!("raw-hdl-{idx}")),
                ],
                Some(json!({ "kind": kind, "payload": raw_payload })),
            ))
            .await;
        assert_err(st, &body, StatusCode::BAD_REQUEST, "plugin.invalid_handle");
    }
}

async fn setup_indexer_plugin(env: &HttpEnv, seed: u64, pid: &str) -> String {
    let key = TestSigningKey::generate(seed);
    env.pin_trust_key(&key, &format!("k-{seed}")).await;
    let pkg = build_ui_pkg(
        pid,
        "idxRedactRemote",
        &[
            "read:sites",
            "read:catalog",
            "read:boost",
            "command:indexer.search",
        ],
        None,
        None,
    );
    let pkg_hash = sha256_hex(&pkg);
    let grants = json!([
        { "capability": "command:indexer.search", "scope": "Global", "sensitive": false }
    ]);
    env.install_pkg(&pkg, Some(&key.sign_package(&pkg)), grants)
        .await;
    let rev = env.get_plugin_rev(pid).await;
    env.enable_plugin(pid, rev, "en-idxr").await;
    grant_ui_trust(env, pid).await;
    pkg_hash
}

fn settle_search_op_with_passkey(env: &HttpEnv, site_id: domain::SiteId, op_id: Uuid, op_rev: i64) {
    let claim = env
        .state
        .store()
        .claim_plugin_command(op_id, op_rev, Some(1), 0, 1_700_000_000)
        .unwrap();
    let raw_result = serde_json::to_vec(&json!({
        "site_id": site_id.to_string(),
        "keyword": "Blade Runner",
        "items": [{
            "title": "Blade Runner 2049 2160p",
            "size_bytes": 8_589_934_592u64,
            "seeders": 42,
            "free": true,
            "hr": false,
            "category": "Movies",
            "enclosure": "https://pt.example/download.php?id=555&passkey=SUPER_SECRET_PASSKEY_777"
        }]
    }))
    .unwrap();
    env.state
        .store()
        .record_plugin_command_outcome(
            &claim,
            &PluginCommandOutcome {
                state: PluginOperationState::Succeeded,
                phase: "done".into(),
                next_run_at: None,
                result: Some(raw_result),
                effect: PluginEffectObservation::NoEffect,
            },
        )
        .unwrap();
}

#[tokio::test]
async fn indexer_cached_returns_409_while_pending_redacts_passkey_and_resolves_handles_strictly() {
    let env = HttpEnv::new().await;
    let site_id = env.insert_site("Indexer Redact Site");
    let pid = "org.example.idxredact";
    let pkg_hash = setup_indexer_plugin(&env, 704, pid).await;
    let attr = [
        ("x-plugin-package", pkg_hash.as_str()),
        ("x-plugin-generation", "1"),
    ];

    let (st_miss, _, b_miss) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui/query"),
            Some(TOKEN),
            &attr,
            Some(json!({ "name": "handles.resolve", "input": { "kind": "owned_download", "id": "nonexistent-task" } })),
        ))
        .await;
    assert_err(
        st_miss,
        &b_miss,
        StatusCode::NOT_FOUND,
        "plugin.task_missing",
    );

    let (st_sh, _, b_sh) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui/query"),
            Some(TOKEN),
            &attr,
            Some(json!({ "name": "handles.resolve", "input": { "kind": "site", "id": site_id } })),
        ))
        .await;
    let site_handle = assert_ok(st_sh, &b_sh, StatusCode::OK)["handle"]
        .as_str()
        .unwrap()
        .to_string();

    let (st_sub, _, b_sub) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/commands"),
            Some(TOKEN),
            &[attr[0], attr[1], ("idempotency-key", "idx-sdk-search-1")],
            Some(json!({
                "kind": "indexer.search",
                "payload": { "sites": [site_handle], "query": "Blade Runner" }
            })),
        ))
        .await;
    let op_view = assert_ok(st_sub, &b_sub, StatusCode::ACCEPTED);
    let op_id = Uuid::parse_str(op_view["operation_id"].as_str().unwrap()).unwrap();
    let op_rev = op_view["revision"].as_i64().unwrap();

    // While queued -> 409 plugin.operation_not_ready
    let (st_pend, _, b_pend) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui/query"),
            Some(TOKEN),
            &attr,
            Some(json!({ "name": "indexer.cached", "input": { "search_operation_id": op_id } })),
        ))
        .await;
    assert_err(
        st_pend,
        &b_pend,
        StatusCode::CONFLICT,
        "plugin.operation_not_ready",
    );

    settle_search_op_with_passkey(&env, site_id, op_id, op_rev);

    let (st_done, _, b_done) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui/query"),
            Some(TOKEN),
            &attr,
            Some(json!({ "name": "indexer.cached", "input": { "search_operation_id": op_id } })),
        ))
        .await;
    let cached = assert_ok(st_done, &b_done, StatusCode::OK);
    let dumped = cached.to_string();
    assert!(!dumped.contains("SUPER_SECRET_PASSKEY_777"));
    assert!(!dumped.contains("enclosure"));
    let first = &cached["items"].as_array().unwrap()[0];
    assert!(first["handle"].as_str().unwrap().starts_with("uh1."));
    assert!(first["labels"].as_array().unwrap().contains(&json!("free")));
}

#[tokio::test]
async fn builtin_boost_config_schema_exposes_canonical_downloader_id_only() {
    let env = HttpEnv::new().await;
    plugin_http_support::plugins_fixture::install_cm_boost(&env, false, false, false).await;
    let (st, _, body) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/cm.boost/config-schema",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let view = assert_ok(st, &body, StatusCode::OK);
    let props = view["schema"]["properties"].as_object().unwrap();
    assert!(props.contains_key("downloader_id"));
    assert!(!props.contains_key("downloader"));
}

fn seed_selection_movie_file(env: &HttpEnv) -> (String, domain::LedgerId, i64) {
    use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource};
    use store::test_support::{FixtureFile, publish_fixture_file};

    let root = env._dir.path().join("movies-sel");
    std::fs::create_dir_all(&root).unwrap();
    let lib = env
        .state
        .store()
        .create_library(
            MediaKind::Movie,
            "MoviesSel",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Arrival".into(),
        year: Some(2016),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    env.state.store().insert_media(&media).unwrap();
    let published = publish_fixture_file(
        &env.state.store(),
        &FixtureFile {
            file_id: Some(LedgerId::new()),
            media_id: media.id,
            path: root.join("arrival.mkv").display().to_string(),
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            ..Default::default()
        },
    )
    .unwrap();
    (lib.id, published.file_id, published.version.revision)
}

#[tokio::test]
async fn files_selection_ui_query_mints_verified_handle_and_checks_revisions() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(710);
    env.pin_trust_key(&key, "k-710").await;
    let pid = "org.example.filesel";
    let pkg = build_ui_pkg(pid, "fileSelRemote", &["read:library"], None, None);
    let pkg_hash = sha256_hex(&pkg);
    let installed = env
        .install_pkg(&pkg, Some(&key.sign_package(&pkg)), json!([]))
        .await;
    env.enable_plugin(pid, installed["revision"].as_i64().unwrap(), "en-fsel")
        .await;
    grant_ui_trust(&env, pid).await;
    let (st_desc, _, b_desc) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}/ui"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let gen_str = assert_ok(st_desc, &b_desc, StatusCode::OK)["generation"]
        .as_i64()
        .unwrap()
        .to_string();
    let (lib_id, file_id, file_rev) = seed_selection_movie_file(&env);
    let attr = [
        ("x-plugin-package", pkg_hash.as_str()),
        ("x-plugin-generation", gen_str.as_str()),
    ];

    let (st_ok, _, b_ok) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui/query"),
            Some(TOKEN),
            &attr,
            Some(json!({
                "name": "files.selection",
                "input": {
                    "library_id": lib_id,
                    "file_ids": [file_id.to_string()],
                    "expected_revisions": [file_rev.to_string()]
                }
            })),
        ))
        .await;
    let sel = assert_ok(st_ok, &b_ok, StatusCode::OK);
    assert!(sel["handle"].as_str().unwrap().starts_with("uh1."));
    assert!(!sel["expires_at"].as_str().unwrap().is_empty());

    let (st_conf, _, b_conf) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui/query"),
            Some(TOKEN),
            &attr,
            Some(json!({
                "name": "files.selection",
                "input": {
                    "library_id": lib_id,
                    "file_ids": [file_id.to_string()],
                    "expected_revisions": ["999"]
                }
            })),
        ))
        .await;
    assert_err(
        st_conf,
        &b_conf,
        StatusCode::CONFLICT,
        "plugin.revision_conflict",
    );

    let (st_empty_rev, _, b_empty_rev) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui/query"),
            Some(TOKEN),
            &attr,
            Some(json!({
                "name": "files.selection",
                "input": {
                    "library_id": lib_id,
                    "file_ids": [file_id.to_string()],
                    "expected_revisions": []
                }
            })),
        ))
        .await;
    assert_err(
        st_empty_rev,
        &b_empty_rev,
        StatusCode::BAD_REQUEST,
        "plugin.invalid_input",
    );
}

#[tokio::test]
async fn draining_plugin_ui_descriptor_marks_config_surface_unmountable_and_read_only() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(711);
    env.pin_trust_key(&key, "k-711").await;
    let pid = "org.example.drainui";
    let pkg = build_ui_pkg(pid, "drainUiRemote", &["read:sites"], None, None);
    let installed = env
        .install_pkg(&pkg, Some(&key.sign_package(&pkg)), json!([]))
        .await;
    env.enable_plugin(pid, installed["revision"].as_i64().unwrap(), "en-drain")
        .await;
    grant_ui_trust(&env, pid).await;

    let rev = env.get_plugin_rev(pid).await;
    env.state
        .store()
        .begin_plugin_lifecycle(pid, rev, Uuid::new_v4(), false)
        .unwrap();

    let (st_ui, _, b_ui) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}/ui"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let desc = assert_ok(st_ui, &b_ui, StatusCode::OK);
    assert_eq!(desc["lifecycle"], "draining");
    let cfg_surface = desc["surfaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["kind"] == "config")
        .unwrap();
    assert_eq!(cfg_surface["mountable"], false);
    assert_eq!(cfg_surface["editable"], false);
}
