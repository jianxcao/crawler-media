#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use std::sync::Arc;

use axum::http::StatusCode;
use domain::{MediaKind, SubscribeId};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
use plugin_api::{GuestRole, HOOK_WORLD};
use plugin_host::Invocation;
use serde_json::{Value, json};
use store::plugin_events::{EmitOutboxEventInput, OutboxDb};
use store::{PendingDownload, StoreError};

use plugin_http_support::{ADMIN_PASS, HttpEnv, TOKEN, assert_ok, assert_paged, pkg, req_json};

const POLICY_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/policy.wasm");

struct EmptyFetcher;
impl Fetcher for EmptyFetcher {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Ok(json!({ "code": "0", "data": { "data": [] } }).to_string())
    }
}

async fn setup_policy_plugin(env: &HttpEnv, plugin_id: &str, priority: i32, hook_id: &str) {
    let schema = pkg::sample_schema(false, None);
    let cap = format!("hooks:{hook_id}");
    let manifest = format!(
        r#"manifest_version = 1
id = "{plugin_id}"
name = "Policy {plugin_id}"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["test"]
description = "M4.2 lifecycle test fixture"
config_schema = "config.schema.json"
state_schema = 1

[[components]]
id = "policy"
entry = "policy.wasm"
world = "{HOOK_WORLD}"
permissions = ["{cap}"]

[[components.hooks]]
id = "{hook_id}"
priority = {priority}
"#
    );
    let pkg_bytes = pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", schema.as_bytes()),
        ("policy.wasm", POLICY_WASM),
    ]);
    let inst = env
        .install_pkg(
            &pkg_bytes,
            None,
            json!([{ "capability": cap, "scope": "Global", "sensitive": false }]),
        )
        .await;
    let rev = inst["revision"].as_i64().unwrap();
    env.enable_plugin(plugin_id, rev, &format!("en-{plugin_id}"))
        .await;
}

async fn fetch_audit_actions(env: &HttpEnv, plugin_id: &str, action: &str) -> Vec<Value> {
    let (st, _, body) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{plugin_id}/audit?action={action}&limit=50"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let (items, _) = assert_paged(st, &body);
    items
}

fn build_transfer_test_env(
    dir: &tempfile::TempDir,
) -> (HttpEnv, Arc<MemoryDownloader>, std::path::PathBuf) {
    let store = Arc::new(api::Store::open(dir.path().join("data")).unwrap());
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let state = api::ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(EmptyFetcher),
        dl.clone(),
    )
    .unwrap();
    let app = api::router(state.clone());
    let env = HttpEnv {
        _dir: tempfile::tempdir_in("/tmp").unwrap(),
        state,
        app,
        member_token: String::new(),
    };
    let movie_lib = env
        .state
        .store()
        .default_library(MediaKind::Movie)
        .unwrap()
        .unwrap();
    let movie_root = movie_lib.root_paths[0].clone();
    (env, dl, movie_root)
}

#[tokio::test]
async fn subscribe_transfer_chains_transfer_rename_off_lease_and_records_audit() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let (env, dl, movie_root) = build_transfer_test_env(&dir);
    env.enable_dev_mode().await;
    setup_policy_plugin(&env, "org.example.tr1", 10, "transfer.rename").await;
    setup_policy_plugin(&env, "org.example.tr2", 20, "transfer.rename").await;

    let (st_sub, _, b_sub) = env
        .send(req_json(
            "POST",
            "/api/v1/subscriptions",
            Some(TOKEN),
            &[],
            Some(json!({
                "media": { "kind": "movie", "title": "Inception", "year": 2010 },
                "coverage": { "kind": "movie" },
                "fetch_mode": "search"
            })),
        ))
        .await;
    let sub_id: SubscribeId = assert_ok(st_sub, &b_sub, StatusCode::CREATED)["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let src_dir = dir.path().join("dl_done");
    std::fs::create_dir_all(&src_dir).unwrap();
    let src_file = src_dir.join("Inception.2010.1080p.mkv");
    std::fs::write(&src_file, b"video-data").unwrap();
    let t = domain::Torrent {
        site_id: domain::SiteId::new(),
        title: "Inception 2010 1080p".into(),
        enclosure: "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".into(),
        size_bytes: Some(1024),
        seeders: Some(10),
        free: false,
        hr: None,
        imdb_id: None,
        id: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        category: None,
        detail_url: None,
        poster_url: None,
        free_2x: false,
        free_until: None,
    };
    dl.map_enclosure(&t.enclosure, src_file.clone());

    env.state
        .store()
        .record_pending_submission(
            sub_id,
            80,
            &PendingDownload {
                torrent: t,
                release_override: None,
                downloader_id: None,
                submitted_at: Some(1000),
            },
        )
        .unwrap();

    let st = env.state.clone();
    tokio::task::spawn_blocking(move || api::worker::transfer_one(&st, sub_id))
        .await
        .unwrap()
        .unwrap();
    let sub_after = env.state.store().get_subscribe(sub_id).unwrap().unwrap();
    let rows = env
        .state
        .store()
        .file_coverage_for_media(sub_after.media_id)
        .unwrap();
    assert_eq!(rows.len(), 1);
    let rel = rows[0].file.path.strip_prefix(&movie_root).unwrap();
    assert!(
        rel.starts_with("renamed/renamed"),
        "expected chained renamed/renamed prefix, got {}",
        rel.display()
    );
    assert_eq!(
        fetch_audit_actions(&env, "org.example.tr1", "transfer.rename")
            .await
            .len(),
        1
    );
    assert_eq!(
        fetch_audit_actions(&env, "org.example.tr2", "transfer.rename")
            .await
            .len(),
        1
    );
}

#[tokio::test]
async fn transfer_rename_rejects_traversal_nested_root_and_error_with_fallback_audit() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let (env, dl, movie_root) = build_transfer_test_env(&dir);
    env.enable_dev_mode().await;

    // 1) Traversal plugin (`t2tr1`) + Error plugin (`t2tr4`)
    setup_policy_plugin(&env, "org.example.t2tr1", 10, "transfer.rename").await;
    setup_policy_plugin(&env, "org.example.t2tr4", 20, "transfer.rename").await;

    // 2) Nested child library root at `<movie_root>/renamed` so any `renamed/...` patch crosses root boundary
    let nested_root = movie_root.join("renamed");
    std::fs::create_dir_all(&nested_root).unwrap();
    env.state
        .store()
        .create_library(
            MediaKind::Movie,
            "Nested Child",
            &[nested_root.to_str().unwrap()],
            "everyone",
            false,
            &[],
        )
        .unwrap();
    setup_policy_plugin(&env, "org.example.trok", 30, "transfer.rename").await;

    let movie_lib = env
        .state
        .store()
        .default_library(MediaKind::Movie)
        .unwrap()
        .unwrap();
    let src = dir.path().join("Boundary.Guard.1080p.mkv");
    std::fs::write(&src, b"data").unwrap();
    let final_dest = api::plugins::hooks_runtime::run_transfer_rename(
        &env.state,
        &api::plugins::hooks_runtime::TransferRenameRequest {
            workflow_id: "wf-boundary-1".into(),
            library_id: movie_lib.id.parse().unwrap(),
            library_root: movie_root.clone(),
            media_id: domain::MediaId::new(),
            media_kind: MediaKind::Movie,
            subscribe_id: None,
            source_path: src,
            proposed_relative_path: "Inception (2010)/Inception (2010).mkv".into(),
        },
    );
    assert_eq!(
        final_dest,
        movie_root.join("Inception (2010)/Inception (2010).mkv")
    );
    let a1 = fetch_audit_actions(&env, "org.example.t2tr1", "transfer.rename").await;
    assert_eq!(a1[0]["state"], "rejected");
    let a2 = fetch_audit_actions(&env, "org.example.t2tr4", "transfer.rename").await;
    assert_eq!(a2[0]["state"], "failed");
    let a3 = fetch_audit_actions(&env, "org.example.trok", "transfer.rename").await;
    assert_eq!(a3[0]["state"], "rejected");
    let _ = dl;
}

#[tokio::test]
async fn watch_intake_and_unidentified_claim_run_transfer_rename_before_publish() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let (env, _dl, movie_root) = build_transfer_test_env(&dir);
    env.enable_dev_mode().await;
    setup_policy_plugin(&env, "org.example.watchclaim", 10, "transfer.rename").await;

    let intake = dir.path().join("intake");
    std::fs::create_dir_all(&intake).unwrap();
    std::fs::write(intake.join("The.Matrix.1999.1080p.BluRay.mkv"), b"vid").unwrap();
    env.state
        .store()
        .set_watch_intake(intake.to_str().unwrap())
        .unwrap();
    api::watch_intake_run_for_test(&env.state).unwrap();

    let all_rows = env.state.store().list_file_coverage().unwrap();
    assert_eq!(all_rows.len(), 1);
    let rel_watch = all_rows[0].file.path.strip_prefix(&movie_root).unwrap();
    assert!(
        rel_watch.starts_with("renamed"),
        "expected watch intake renamed prefix, got {}",
        rel_watch.display()
    );

    let parked_path = dir.path().join("mystery.clip.mkv");
    std::fs::write(&parked_path, b"clip").unwrap();
    env.state
        .store()
        .insert_unidentified(
            parked_path.to_str().unwrap().to_string(),
            domain::Confidence::Low,
        )
        .unwrap();
    let (st_claim, _, b_claim) = env
        .send(req_json(
            "POST",
            "/api/v1/unidentified/claim",
            Some(TOKEN),
            &[],
            Some(json!({
                "path": parked_path.to_str().unwrap(),
                "action": {
                    "type": "identify_and_transfer",
                    "kind": "movie",
                    "title": "Claimed Film",
                    "year": 2024
                }
            })),
        ))
        .await;
    let claimed = assert_ok(st_claim, &b_claim, StatusCode::OK);
    let claimed_path = std::path::PathBuf::from(claimed["path"].as_str().unwrap());
    let canonical_root = movie_root
        .canonicalize()
        .unwrap_or_else(|_| movie_root.clone());
    let rel_claim = claimed_path
        .strip_prefix(&canonical_root)
        .or_else(|_| claimed_path.strip_prefix(&movie_root))
        .unwrap();
    assert!(
        rel_claim.starts_with("renamed"),
        "expected claim renamed prefix, got {}",
        rel_claim.display()
    );
    assert_eq!(
        fetch_audit_actions(&env, "org.example.watchclaim", "transfer.rename")
            .await
            .len(),
        2
    );
}

#[tokio::test]
async fn library_removal_cancels_active_plugin_invocation_and_outbox_rollback_emits_zero() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let (env, _dl, _movie_root) = build_transfer_test_env(&dir);
    let extra_root = dir.path().join("extra_lib");
    std::fs::create_dir_all(&extra_root).unwrap();
    let library = env
        .state
        .store()
        .create_library(
            MediaKind::Movie,
            "Extra Remove",
            &[extra_root.to_str().unwrap()],
            "everyone",
            false,
            &[],
        )
        .unwrap();

    let mut inv = Invocation::new("org.example.active", std::time::Duration::from_secs(30));
    inv.library_id = Some(library.id.clone());
    inv.role = GuestRole::Filter;
    let _guard = env
        .state
        .plugins()
        .enter_library_invocation_for_test("org.example.active", 1, &inv)
        .unwrap();
    assert!(!inv.cancelled());

    let _op = api::library_lifecycle::start_library_removal(&env.state, &library.id).unwrap();
    assert!(
        inv.cancelled(),
        "ADR-0012 library removal must cancel active plugin invocation scoped to library"
    );

    let rb: Result<(), StoreError> = env.state.store().library_write(|lib| {
        lib.emit_outbox_event(&EmitOutboxEventInput {
            event_id: "ev-lib-rollback".into(),
            schema_version: 1,
            event_domain: "transfer".into(),
            event_kind: "transfer.completed".into(),
            entity_type: "media".into(),
            entity_id: "m-rb".into(),
            entity_sequence: None,
            entity_revision: 1,
            cause_id: "cause-rb".into(),
            subject: "system".into(),
            site_id: None,
            subscribe_id: None,
            library_id: Some(library.id.clone()),
            handles_json: "[]".into(),
            payload_json: "{}".into(),
            occurred_at: 1000,
            created_at: 1000,
        })?;
        Err(StoreError::Invalid("forced rollback".into()))
    });
    assert!(rb.is_err());
    assert!(
        env.state
            .store()
            .get_outbox_event(OutboxDb::Library, "ev-lib-rollback")
            .unwrap()
            .is_none()
    );
}
