use std::path::PathBuf;

use api::plugins::entrypoints::build_hook_pipeline_for_test;
use api::plugins::hooks_runtime::{TransferRenameRequest, run_transfer_rename};
use domain::{LibraryId, MediaId, MediaKind, SubscribeId};
use plugin_api::{HOOK_WORLD, HookContext, TransferRename};
use serde_json::json;
use store::plugins::{PluginGrant, PluginGrantScope};

use crate::support::plugin_http_support::{HttpEnv, pkg};

const POLICY_WASM: &[u8] = include_bytes!("../../../plugin-host/tests/fixtures/policy.wasm");

fn build_rename_hook_zip(pid: &str) -> Vec<u8> {
    let schema = pkg::sample_schema(false, None);
    let manifest = format!(
        r#"manifest_version = 1
id = "{pid}"
name = "Hook Scope Norm"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["t"]
description = "d"
config_schema = "config.schema.json"
state_schema = 1

[[components]]
id = "policy"
entry = "policy.wasm"
world = "{HOOK_WORLD}"
permissions = ["hooks:transfer.rename"]

[[components.hooks]]
id = "transfer.rename"
priority = 10
"#
    );
    pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", schema.as_bytes()),
        ("policy.wasm", POLICY_WASM),
    ])
}

#[tokio::test]
async fn slice_d_r5_item4_hook_object_grants_normalize_library_and_subscribe_scopes() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let store = env.state.store();

    let (lib_a, lib_b) = (LibraryId::new(), LibraryId::new());
    let (sub_a, sub_b) = (SubscribeId::new(), SubscribeId::new());

    let pid = "org.example.r5-hook-norm";
    let zip = build_rename_hook_zip(pid);
    let detail = env.install_pkg(&zip, None, json!([])).await;
    let hash = detail["package_hash"].as_str().unwrap().to_string();
    let rev = detail["revision"].as_i64().unwrap();

    let grants = vec![
        PluginGrant {
            capability: "hooks:transfer.rename".into(),
            scope: PluginGrantScope::Object {
                kind: "library".into(),
                id: format!("  LIBRARY:{}  ", lib_a.to_string().to_ascii_uppercase()),
            },
            sensitive: false,
        },
        PluginGrant {
            capability: "hooks:transfer.rename".into(),
            scope: PluginGrantScope::Object {
                kind: "subscribe".into(),
                id: format!("  SuBsCrIbE:{}  ", sub_a.as_uuid().simple()),
            },
            sensitive: false,
        },
        PluginGrant {
            capability: "hooks:transfer.rename".into(),
            scope: PluginGrantScope::Object {
                kind: "library".into(),
                id: "  LIBRARY:CUSTOM-SLUG  ".into(),
            },
            sensitive: false,
        },
    ];
    let updated = store
        .replace_plugin_grants(pid, &hash, rev, &grants)
        .unwrap();
    env.enable_plugin(pid, updated.revision, "r5-hn-en").await;

    let root = env._dir.path().join("libroot");
    std::fs::create_dir_all(&root).unwrap();
    let mk_req = |lib: LibraryId, sub: Option<SubscribeId>| TransferRenameRequest {
        workflow_id: "wf-r5-tr".into(),
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
    assert_eq!(
        run_transfer_rename(&env.state, &mk_req(lib_a, Some(sub_b))),
        root.join("renamed/Film.mkv")
    );
    assert_eq!(
        run_transfer_rename(&env.state, &mk_req(lib_b, Some(sub_a))),
        root.join("renamed/Film.mkv")
    );

    assert_non_uuid_and_empty_scope_matching(&env);
}

fn assert_non_uuid_and_empty_scope_matching(env: &HttpEnv) {
    let pipeline = build_hook_pipeline_for_test(&env.state, "transfer.rename");
    let mut tr = TransferRename {
        media_id: MediaId::new().to_string(),
        library_id: "library:custom-slug".into(),
        source_path: "/downloads/Slug.mkv".into(),
        proposed_relative_path: "Slug.mkv".into(),
        is_movie: true,
    };
    let ctx_slug = HookContext {
        invocation_id: "inv-slug".into(),
        cause_id: "cause-slug".into(),
        plugin_id: String::new(),
        generation: 1,
        config_revision: 1,
        subject: "system".into(),
        site_id: None,
        subscribe_id: None,
        library_id: Some("library:custom-slug".into()),
        deadline_ms: 500,
        cancelled: false,
    };
    let _ = pipeline
        .filter_transfer_rename_with_trace("wf-slug", &ctx_slug, &mut tr)
        .unwrap();
    assert_eq!(tr.proposed_relative_path, "renamed/Slug.mkv");

    let mut tr_empty = TransferRename {
        proposed_relative_path: "Unchanged.mkv".into(),
        ..tr
    };
    let ctx_empty = HookContext {
        library_id: Some("library:   ".into()),
        ..ctx_slug
    };
    let _ = pipeline
        .filter_transfer_rename_with_trace("wf-empty", &ctx_empty, &mut tr_empty)
        .unwrap();
    assert_eq!(tr_empty.proposed_relative_path, "Unchanged.mkv");
}
