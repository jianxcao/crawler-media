pub(crate) mod config;
pub(crate) mod cycle;
pub(crate) mod gate;
pub(crate) mod preview;
pub(crate) mod snapshot;

pub(crate) use cycle::{execute_boost_run_command, run_builtin_boost_task};
pub(crate) use gate::BuiltinBoostDownloadGate;
pub(crate) use preview::{CleanupPreviewQuery, compute_plan_digest, preview_brush_cleanup};
pub(crate) use snapshot::{
    SnapshotQuery, collect_boost_snapshot_sync, expand_path_set_off_lock,
    expand_protection_sets_off_lock, extract_plan_paths, get_brush_state_http, path_is_in_set,
};

use plugin_package::{
    ComponentManifest, HookManifest, PluginManifest, TaskManifest, TaskScheduleManifest,
};
use serde_json::json;
use store::plugins::{PluginGrant, PluginGrantScope, PluginTaskDefinition, PluginVersionMetadata};

use super::builtins::ensure_builtin_plugin;
use super::tasks::sync_task_binding;
use crate::management::ApiState;

pub const BUILTIN_BOOST_PLUGIN_ID: &str = "builtin.boost";
pub const BUILTIN_BOOST_TASK_ID: &str = "traffic-cycle";

fn boost_config_schema_json() -> String {
    json!({
        "type": "object",
        "properties": {
            "sites": {
                "type": "array",
                "items": { "type": "string", "x-ui": { "widget": "site" } },
                "maxItems": 16,
                "uniqueItems": true,
                "default": []
            },
            "downloader_id": {
                "type": "string",
                "default": "",
                "x-ui": { "widget": "downloader" }
            },
            "budget_bytes": {
                "type": "integer",
                "minimum": 0,
                "x-ui": { "widget": "bytes" }
            },
            "min_free_disk_bytes": {
                "type": "integer",
                "minimum": 0,
                "default": 0,
                "x-ui": { "widget": "bytes" }
            },
            "hold_days": {
                "type": "integer",
                "minimum": 0,
                "maximum": 365,
                "default": 0
            },
            "max_active": {
                "type": "integer",
                "minimum": 1,
                "maximum": 200,
                "default": 20
            },
            "max_adds_per_hour": {
                "type": "integer",
                "minimum": 1,
                "maximum": 100,
                "default": 10
            },
            "max_adds_per_round": {
                "type": "integer",
                "minimum": 1,
                "maximum": 100,
                "default": 20
            },
            "dry_run": {
                "type": "boolean",
                "default": true
            },
            "auto_cleanup_files": {
                "type": "boolean",
                "default": false
            },
            "min_seeders": {
                "type": "integer",
                "minimum": 0,
                "default": 1
            },
            "free_only": {
                "type": "boolean",
                "default": true
            },
            "exclude_hr": {
                "type": "boolean",
                "default": true
            },
            "enforce_budget_on_all": {
                "type": "boolean",
                "default": false
            }
        },
        "additionalProperties": false
    })
    .to_string()
}

fn boost_task_input_schema_json() -> String {
    json!({
        "type": "object",
        "properties": {
            "site_id": { "type": "string" },
            "downloader_id": { "type": "string" },
            "dry_run": { "type": "boolean" }
        },
        "additionalProperties": false
    })
    .to_string()
}

pub(crate) fn builtin_boost_manifest_json() -> String {
    let manifest = PluginManifest {
        manifest_version: 1,
        id: BUILTIN_BOOST_PLUGIN_ID.into(),
        name: "PT Boost Orchestrator".into(),
        version: "1.0.0".into(),
        api: plugin_api::API_VERSION.into(),
        license: "Apache-2.0".into(),
        authors: vec!["crawler-media".into()],
        description: "Built-in PT ratio boost quota and cleanup orchestrator".into(),
        config_schema: boost_config_schema_json(),
        state_schema: 1,
        credential_binding: None,
        http: Vec::new(),
        sites: Vec::new(),
        limits: None,
        components: vec![
            ComponentManifest {
                id: "builtin".into(),
                entry: "builtin".into(),
                world: plugin_host::TASK_WORLD.into(),
                permissions: vec![
                    "kv".into(),
                    "read:sites".into(),
                    "read:downloads".into(),
                    "read:boost".into(),
                    "command:indexer.search".into(),
                    "command:indexer.rss".into(),
                    "command:downloader.add".into(),
                    "command:downloader.control".into(),
                    "command:downloader.delete-task".into(),
                    "command:downloader.prepare-cleanup".into(),
                    "command:downloader.delete-files".into(),
                    "command:boost.run".into(),
                ],
                hooks: Vec::new(),
                tasks: vec![TaskManifest {
                    id: BUILTIN_BOOST_TASK_ID.into(),
                    input_schema: boost_task_input_schema_json(),
                    schedule: TaskScheduleManifest {
                        kind: "interval".into(),
                        seconds: 1800,
                    },
                    concurrency_key: Some(BUILTIN_BOOST_TASK_ID.into()),
                }],
                indexers: Vec::new(),
                maintenance: Vec::new(),
                endpoints: Vec::new(),
                lists: Vec::new(),
                channels: Vec::new(),
            },
            ComponentManifest {
                id: "builtin-gate".into(),
                entry: "builtin".into(),
                world: plugin_host::HOOK_WORLD.into(),
                permissions: vec!["hooks:download.before".into()],
                hooks: vec![HookManifest {
                    id: "download.before".into(),
                    priority: 100,
                }],
                tasks: Vec::new(),
                indexers: Vec::new(),
                maintenance: Vec::new(),
                endpoints: Vec::new(),
                lists: Vec::new(),
                channels: Vec::new(),
            },
        ],
        ui: Some(super::ui_builtins::boost_ui_manifest()),
    };
    serde_json::to_string(&manifest).unwrap_or_else(|_| "{}".into())
}

pub(crate) fn bootstrap_builtin_boost(state: &ApiState) {
    let boost_ver = PluginVersionMetadata {
        plugin_id: BUILTIN_BOOST_PLUGIN_ID.into(),
        package_hash: "builtin:boost:1.0.0".into(),
        version: "1.0.0".into(),
        manifest_json: builtin_boost_manifest_json(),
        config_schema_json: boost_config_schema_json(),
        state_schema: "1".into(),
        artifact_path: String::new(),
        signing_key_id: None,
        signature_status: "builtin".into(),
        ui_metadata_ref: Some("ui/asset-manifest.json".into()),
        credential_bound: false,
    };
    let mut grants = Vec::new();
    for cap in [
        "kv",
        "read:sites",
        "read:downloads",
        "read:boost",
        "hooks:download.before",
        "command:indexer.search",
        "command:indexer.rss",
        "command:downloader.add",
        "command:downloader.control",
        "command:downloader.delete-task",
        "command:downloader.prepare-cleanup",
        "command:downloader.delete-files",
        "command:boost.run",
    ] {
        grants.push(PluginGrant {
            capability: cap.into(),
            scope: PluginGrantScope::Global,
            sensitive: matches!(cap, "command:downloader.delete-files"),
        });
    }
    if let Err(err) = ensure_builtin_plugin(&state.store, boost_ver, grants) {
        tracing::error!(error = %err, "Failed to bootstrap builtin.boost plugin");
        return;
    }
    super::ui_builtins::ensure_builtin_ui_trust(
        &state.store,
        BUILTIN_BOOST_PLUGIN_ID,
        "builtin:boost:1.0.0",
    );

    if let Ok(Some(plugin)) = state.store.get_plugin(BUILTIN_BOOST_PLUGIN_ID)
        && state
            .store
            .get_plugin_task_binding(BUILTIN_BOOST_PLUGIN_ID, BUILTIN_BOOST_TASK_ID)
            .ok()
            .flatten()
            .is_none()
        && let Ok(binding) = state.store.save_plugin_task_binding(
            &plugin,
            BUILTIN_BOOST_TASK_ID,
            0,
            &PluginTaskDefinition {
                enabled: false,
                interval_secs: Some(1800),
                public_config: json!({}),
            },
        )
        && let Err(err) = sync_task_binding(state, &binding)
    {
        tracing::warn!(error = %err, "Failed to project builtin.boost task binding on startup");
    }
}
