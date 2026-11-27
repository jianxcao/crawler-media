#[path = "builtin_boost/support.rs"]
pub(crate) mod support;

#[path = "plugin_slice_e_remediation/attribution_and_trust.rs"]
mod attribution_and_trust;
#[path = "plugin_slice_e_remediation/builtins_and_budget.rs"]
mod builtins_and_budget;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode, header};
use domain::{DownloaderId, Media, MediaId, MediaKind, SiteId};
use downloader::MemoryDownloader;
use indexer::RegistrySnapshot;
use plugin_package::build_deterministic_cmplugin_zip;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use store::plugins::{PluginGrant, PluginLifecycleObservation};
use store::{BoostTaskState, PrepareBoostCleanupPlanInput};
use support::plugin_http_support::{
    ADMIN_PASS, HttpEnv, TOKEN, assert_err, assert_ok,
    pkg::TestSigningKey,
    plugins_fixture::{install_cm_boost, seed_cm_boost_task},
    req_json,
};
use support::{NoopFetcher, poll_op};
use tower::ServiceExt;
use uuid::Uuid;

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(crate) fn build_ui_pkg(pid: &str, remote: &str, caps: &[&str], with_task: bool) -> Vec<u8> {
    let js = format!("export const remote = '{remote}';\n");
    let asset_manifest = serde_json::to_vec(&json!({
        "version": 1,
        "files": [{ "path": "ui/assets/remoteEntry.js", "sha256": sha256_hex(js.as_bytes()), "size": js.len(), "content_type": "application/javascript" }]
    }))
    .unwrap();
    let task_block = if with_task {
        format!(
            "[[components]]\nid=\"worker\"\nentry=\"components/worker.wasm\"\nworld=\"{}\"\npermissions=[\"kv\"]\n[[components.tasks]]\nid=\"sync\"\ninput_schema=\"schemas/task.schema.json\"\nschedule={{kind=\"interval\",seconds=3600}}\n",
            plugin_host::TASK_WORLD
        )
    } else {
        String::new()
    };
    let manifest = format!(
        "manifest_version=1\nid=\"{pid}\"\nname=\"SliceE\"\nversion=\"1.0.0\"\napi=\"1.0.0\"\nlicense=\"MIT\"\nauthors=[\"T\"]\ndescription=\"fix\"\nconfig_schema=\"schemas/config.schema.json\"\nstate_schema=1\n{task_block}[ui]\nkind=\"federation-react\"\ncontract=\"1.0.0\"\nreact=\"19.3.0\"\nreact_dom=\"19.3.0\"\nremote_name=\"{remote}\"\nentry=\"ui/assets/remoteEntry.js\"\nasset_manifest=\"ui/asset-manifest.json\"\ncapabilities={}\n[[ui.surfaces]]\nkind=\"config\"\nid=\"settings\"\nexpose=\"./Config\"\ntitle=\"Cfg\"\n[[ui.surfaces]]\nkind=\"page\"\nid=\"main\"\nexpose=\"./Page\"\ntitle=\"Main\"\n",
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
    if with_task {
        entries.insert(
            "schemas/task.schema.json".into(),
            br#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{}}"#
                .to_vec(),
        );
        entries.insert(
            "components/worker.wasm".into(),
            support::plugin_http_support::pkg::task_wasm().to_vec(),
        );
    }
    build_deterministic_cmplugin_zip(&entries).unwrap()
}

pub(crate) async fn grant_ui_trust_http(env: &HttpEnv, pid: &str) -> Value {
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
    let idem = format!("ug-{}", Uuid::new_v4());
    let (st_g, _, b_g) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/grant"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", &idem),
            ],
            Some(json!({
                "preparation_id": prep["preparation_id"],
                "package_hash": prep["package_hash"],
                "consent_token": prep["consent_token"],
                "reauth_token": reauth,
            })),
        ))
        .await;
    assert_ok(st_g, &b_g, StatusCode::OK)
}

pub(crate) async fn login_admin_cookie(env: &HttpEnv) -> String {
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/login")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "username": "admin", "password": ADMIN_PASS }).to_string(),
        ))
        .unwrap();
    let resp = env.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let sc = resp
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    sc.split(';')
        .next()
        .unwrap()
        .strip_prefix("mc_session=")
        .unwrap()
        .to_string()
}

pub(crate) async fn send_raw(
    env: &HttpEnv,
    req: Request<Body>,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let resp = env.app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (status, headers, bytes)
}

pub(crate) fn publish_real_library_movie(
    store: &Store,
    root_dir: &Path,
    file_path: &Path,
) -> (String, MediaId) {
    let lib = store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[root_dir.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let root = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|r| r.library_id.as_str() == lib.id.as_str())
        .unwrap();
    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Movie,
            title: "Café Movie".into(),
            year: Some(2025),
            original_title: None,
            tmdb_id: Some("438631".into()),
            douban_id: Some("26954859".into()),
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
            aliases: vec![],
        })
        .unwrap();
    let pkey = store::path_key::canonical_existing_key(&file_path.to_string_lossy());
    store
        .with_writer(store::Db::Library, |conn| {
            conn.execute(
                "INSERT INTO library_files (file_id, library_id, root_id, media_id, kind, path_key, display_path, presence_state, publication_state, file_revision, source_version, source_path, quality_source, confidence)
                 VALUES (?1, ?2, ?3, ?4, 'movie', ?5, ?5, 'present', 'published', 1, 'v1', ?5, 'release', 'high')",
                rusqlite::params![Uuid::new_v4().to_string(), lib.id, root.id, media_id.to_string(), pkey],
            )?;
            Ok(())
        })
        .unwrap();
    (lib.id, media_id)
}

pub(crate) fn flip_plugin_enabled(
    store: &Store,
    pid: &str,
    rev: i64,
    enabled: bool,
) -> store::plugins::PluginRecord {
    let step = store
        .begin_plugin_lifecycle(pid, rev, Uuid::new_v4(), enabled)
        .unwrap();
    store
        .finalize_plugin_lifecycle(&PluginLifecycleObservation {
            plugin_id: step.plugin_id,
            operation_id: step.active_operation_id.unwrap(),
            revision: step.revision,
            generation: step.generation,
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: true,
        })
        .unwrap()
}

pub(crate) fn replace_plugin_grants(store: &Store, pid: &str, grants: &[PluginGrant]) {
    let cur = store.get_plugin(pid).unwrap().unwrap();
    let mut cur = flip_plugin_enabled(store, pid, cur.revision, false);
    let existing = store.get_plugin_grants(pid).unwrap();
    if !existing.is_empty() {
        cur = store
            .revoke_plugin_permissions(pid, &cur.package_hash, cur.revision, &existing, None)
            .unwrap();
    }
    if !grants.is_empty() {
        cur = store
            .grant_plugin_permissions(pid, &cur.package_hash, cur.revision, grants)
            .unwrap();
    }
    let _ = flip_plugin_enabled(store, pid, cur.revision, true);
}

#[cfg(unix)]
#[tokio::test]
async fn symlinked_and_nfd_library_or_sibling_paths_block_boost_cleanup() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let (real_lib, real_shared, stage) = (
        dir.path().join("real_lib"),
        dir.path().join("real_shared"),
        dir.path().join("stage"),
    );
    for d in [&real_lib, &real_shared, &stage] {
        std::fs::create_dir_all(d).unwrap();
    }
    let (lib_file, shared_file) = (
        real_lib.join("Caf\u{00e9}.mkv"),
        real_shared.join("Shared.mkv"),
    );
    std::fs::write(&lib_file, b"published-movie").unwrap();
    std::fs::write(&shared_file, b"shared-sibling").unwrap();
    let (sym_lib, sym_a, sym_b) = (
        stage.join("sym_lib"),
        stage.join("sym_a"),
        stage.join("sym_b"),
    );
    std::os::unix::fs::symlink(&real_lib, &sym_lib).unwrap();
    std::os::unix::fs::symlink(&real_shared, &sym_a).unwrap();
    std::os::unix::fs::symlink(&real_shared, &sym_b).unwrap();

    let dl = Arc::new(MemoryDownloader::new(stage.clone()));
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl,
    )
    .unwrap();
    let env = HttpEnv {
        _dir: dir,
        app: router(state.clone()),
        state,
        member_token: String::new(),
    };
    install_cm_boost(&env, true, false, true).await;
    let (site_id, dl_id) = (env.insert_site("PT-Sym"), DownloaderId::new());
    env.state
        .store()
        .insert_downloader(&store::DownloaderRow {
            id: dl_id,
            name: "SymDL".into(),
            kind: "qbittorrent".into(),
            url: "http://127.0.0.1:9".into(),
            username: None,
            password: None,
            api_key: None,
            category: None,
            path_maps: vec![downloader::PathMap {
                from: "/dl".into(),
                to: real_shared.clone(),
            }],
            is_default: true,
            enabled: true,
        })
        .unwrap();
    publish_real_library_movie(env.state.store().as_ref(), &real_lib, &lib_file);

    let (tf_lib, tf_sib_b) = seed_symlinked_boost_tasks(
        env.state.store().as_ref(),
        dl_id,
        site_id,
        &stage,
        &real_shared,
        &sym_lib,
        &sym_a,
        &sym_b,
    );
    assert_symlink_protections_enforced(&env, &tf_lib, &tf_sib_b, &lib_file, &shared_file).await;
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn seed_symlinked_boost_tasks(
    store: &Store,
    dl_id: DownloaderId,
    site_id: SiteId,
    stage: &Path,
    real_shared: &Path,
    sym_lib: &Path,
    sym_a: &Path,
    sym_b: &Path,
) -> (String, String) {
    let nfd_sym_path = format!("{}/Cafe\u{0301}.mkv", sym_lib.display());
    let tf_lib = seed_cm_boost_task(
        store,
        dl_id,
        site_id,
        "sym-lib",
        "1111000011111111111111111111111111111111",
        BoostTaskState::Seeding,
        128,
        0,
        Some(false),
        None,
    );
    let lib_plan = json!({ "save_path": stage, "paths": [nfd_sym_path] }).to_string();
    store
        .mark_boost_task_retained_with_paths(&tf_lib, 1, 128, Some(&lib_plan), 1_700_000_010)
        .unwrap();

    let tf_sib_a = seed_cm_boost_task(
        store,
        dl_id,
        site_id,
        "sym-sib-a",
        "2222000011111111111111111111111111111111",
        BoostTaskState::Seeding,
        64,
        0,
        Some(false),
        None,
    );
    store
        .mark_boost_task_retained_with_paths(
            &tf_sib_a,
            1,
            64,
            Some(&json!({ "paths": [sym_a.join("Shared.mkv")] }).to_string()),
            1_700_000_010,
        )
        .unwrap();

    let tf_sib_b = seed_cm_boost_task(
        store,
        dl_id,
        site_id,
        "sym-sib-b",
        "3333000011111111111111111111111111111111",
        BoostTaskState::Seeding,
        64,
        0,
        Some(false),
        None,
    );
    let sib_b_plan = json!({
        "save_path": real_shared,
        "allowed_roots": [real_shared],
        "paths": [sym_b.join("Shared.mkv")]
    })
    .to_string();
    let ret_b = store
        .mark_boost_task_retained_with_paths(&tf_sib_b, 1, 64, Some(&sib_b_plan), 1_700_000_010)
        .unwrap();
    store
        .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: tf_sib_b.clone(),
            expected_revision: ret_b.revision,
            cleanup_plan_handle: "plan-sym-sib".into(),
            cleanup_plan_json: sib_b_plan,
            ttl_secs: 600,
            now_utc: api::job_loop::unix_now(),
        })
        .unwrap();
    (tf_lib, tf_sib_b)
}

#[cfg(unix)]
async fn assert_symlink_protections_enforced(
    env: &HttpEnv,
    tf_lib: &str,
    tf_sib_b: &str,
    lib_file: &Path,
    shared_file: &Path,
) {
    let (st_bs, _, b_bs) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/cm.boost/brush-state",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let bs = assert_ok(st_bs, &b_bs, StatusCode::OK);
    let lib_task = bs["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["task_id"] == tf_lib)
        .unwrap();
    assert_eq!(
        lib_task["library_referenced"], true,
        "symlink+NFD library file must be protected: {lib_task}"
    );

    let (st_prev, _, b_prev) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/cm.boost/brush-cleanup-preview?task_ids={tf_lib},{tf_sib_b}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let prev = assert_ok(st_prev, &b_prev, StatusCode::OK);
    assert_eq!(prev["eligible_count"], 0);
    assert_eq!(prev["blocked_count"], 2);

    let (st_plan, _, b_plan) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/cm.boost/brush-cleanup-preview?plan_handle=plan-sym-sib",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_err(
        st_plan,
        &b_plan,
        StatusCode::CONFLICT,
        "plugin.cleanup_blocked",
    );

    let rev = env.get_plugin_rev("cm.boost").await;
    let (st_prep, _, b_prep) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            Some(TOKEN),
            &[
                ("If-Match", &format!("\"{rev}\"")),
                ("Idempotency-Key", "prep-sym-lib"),
            ],
            Some(json!({ "kind": "downloader.prepare-cleanup", "payload": { "task_ids": [tf_lib] } })),
        ))
        .await;
    let prep_op = assert_ok(st_prep, &b_prep, StatusCode::ACCEPTED);
    let done = poll_op(env, prep_op["operation_id"].as_str().unwrap()).await;
    assert_eq!(done["state"], "rejected");
    assert_eq!(done["phase"], "protection-denied:library-referenced");
    assert!(lib_file.exists() && shared_file.exists());
}

#[tokio::test]
async fn ui_attributed_indexer_search_rejects_site_ids_bypass_and_delete_files_accepts_string_revision()
 {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let stage = dir.path().join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    let victim = stage.join("StringRev.mkv");
    std::fs::write(&victim, b"delete-me").unwrap();
    let dl = Arc::new(MemoryDownloader::new(stage.clone()));
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl,
    )
    .unwrap();
    let env = HttpEnv {
        _dir: dir,
        app: router(state.clone()),
        state,
        member_token: String::new(),
    };
    install_cm_boost(&env, true, false, true).await;
    let (site_id, dl_id) = (env.insert_site("PT-Bypass"), DownloaderId::new());
    env.state
        .store()
        .insert_downloader(&store::DownloaderRow {
            id: dl_id,
            name: "StrRevDL".into(),
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

    let key = TestSigningKey::generate(701);
    env.pin_trust_key(&key, "k-701").await;
    let pid = "org.example.searchbypass";
    let pkg = build_ui_pkg(
        pid,
        "searchBypassRemote",
        &["read:sites", "command:indexer.search"],
        false,
    );
    let pkg_hash = sha256_hex(&pkg);
    env.install_pkg(
        &pkg,
        Some(&key.sign_package(&pkg)),
        json!([{ "capability": "command:indexer.search", "scope": "Global", "sensitive": false }]),
    )
    .await;
    let rev = env.get_plugin_rev(pid).await;
    env.enable_plugin(pid, rev, "en-sb").await;
    grant_ui_trust_http(&env, pid).await;

    for (idem, payload) in [
        (
            "bypass-site-ids",
            json!({ "site_ids": [site_id.to_string()], "keyword": "Dune" }),
        ),
        ("bypass-no-handle", json!({ "keyword": "Dune" })),
    ] {
        let (st, _, body) = env
            .send(req_json(
                "POST",
                &format!("/api/v1/plugins/{pid}/commands"),
                Some(TOKEN),
                &[
                    ("idempotency-key", idem),
                    ("x-plugin-package", &pkg_hash),
                    ("x-plugin-generation", "1"),
                ],
                Some(json!({
                    "kind": "indexer.search",
                    "targets": [{ "kind": "global" }],
                    "payload": payload
                })),
            ))
            .await;
        assert!(
            matches!(st, StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN),
            "expected 400/403 for {idem}, got {st}: {body}"
        );
    }

    assert_delete_files_string_revision(&env, dl_id, site_id, &stage, &victim).await;
}

async fn assert_delete_files_string_revision(
    env: &HttpEnv,
    dl_id: DownloaderId,
    site_id: SiteId,
    stage: &Path,
    victim: &Path,
) {
    let tf_id = seed_cm_boost_task(
        env.state.store().as_ref(),
        dl_id,
        site_id,
        "str-rev",
        "4444000011111111111111111111111111111111",
        BoostTaskState::Seeding,
        64,
        0,
        Some(false),
        None,
    );
    let pjson =
        json!({ "save_path": stage, "allowed_roots": [stage], "paths": [victim] }).to_string();
    let ret = env
        .state
        .store()
        .mark_boost_task_retained_with_paths(&tf_id, 1, 64, Some(&pjson), 1_700_000_010)
        .unwrap();
    let planned = env
        .state
        .store()
        .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: tf_id,
            expected_revision: ret.revision,
            cleanup_plan_handle: "plan-str-rev".into(),
            cleanup_plan_json: pjson,
            ttl_secs: 600,
            now_utc: api::job_loop::unix_now(),
        })
        .unwrap();
    let (st_prev, _, b_prev) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/cm.boost/brush-cleanup-preview?plan_handle=plan-str-rev",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let tok = assert_ok(st_prev, &b_prev, StatusCode::OK)["confirmation_token"]
        .as_str()
        .unwrap()
        .to_string();
    let bp = env.state.store().get_plugin("cm.boost").unwrap().unwrap();
    let (st_df, _, b_df) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            Some(TOKEN),
            &[
                ("Idempotency-Key", "df-string-rev"),
                ("X-Plugin-Package", &bp.package_hash),
                ("X-Plugin-Generation", &bp.generation.to_string()),
            ],
            Some(json!({
                "kind": "downloader.delete-files",
                "payload": {
                    "host_file_plan_handle": "plan-str-rev",
                    "revision": planned.revision.to_string(),
                    "reason": "host-cleanup-confirmed",
                    "confirmation_token": tok
                }
            })),
        ))
        .await;
    let df_op = assert_ok(st_df, &b_df, StatusCode::ACCEPTED);
    let done = poll_op(env, df_op["operation_id"].as_str().unwrap()).await;
    assert_eq!(done["state"], "succeeded");
    assert!(!victim.exists());
}
