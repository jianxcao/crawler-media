pub(crate) mod config;
pub(crate) mod cycle;
pub(crate) mod cycle_evict;
pub(crate) mod cycle_filter;
pub(crate) mod cycle_submit;
pub(crate) mod preview;
pub(crate) mod snapshot;

pub(crate) use cycle::execute_boost_run_command;
pub(crate) use preview::{CleanupPreviewQuery, compute_plan_digest, preview_brush_cleanup};
pub(crate) use snapshot::{
    SnapshotQuery, collect_boost_snapshot_sync, expand_path_set_off_lock,
    expand_protection_sets_off_lock, extract_plan_paths, get_brush_state_http, path_is_in_set,
};

#[cfg(test)]
fn boost_rule_properties(include_site_id: bool) -> serde_json::Map<String, serde_json::Value> {
    use serde_json::{Map, json};
    let mut props = Map::new();
    if include_site_id {
        props.insert(
            "site_id".into(),
            json!({ "type": "string", "x-ui": { "widget": "site" } }),
        );
        props.insert(
            "enabled".into(),
            json!({ "type": "boolean", "default": true }),
        );
    }
    let extra = json!({
        "categories": {
            "type": "array",
            "items": { "type": "string" },
            "uniqueItems": true,
            "default": []
        },
        "free_mode": {
            "type": "string",
            "enum": ["2x_free", "free", "any"],
            "default": "free"
        },
        "free_only": { "type": "boolean", "default": true },
        "exclude_hr": { "type": "boolean", "default": true },
        "min_seeders": { "type": "integer", "minimum": 0, "default": 1 },
        "max_seeders": { "type": "integer", "minimum": 0 },
        "min_leechers": { "type": "integer", "minimum": 0 },
        "min_size_bytes": { "type": "integer", "minimum": 0, "x-ui": { "widget": "bytes" } },
        "max_size_bytes": { "type": "integer", "minimum": 0, "x-ui": { "widget": "bytes" } },
        "max_pub_age_minutes": { "type": "integer", "minimum": 0 },
        "include_regex": { "type": "string", "maxLength": 512 },
        "exclude_regex": { "type": "string", "maxLength": 512 },
        "active_hours": { "type": "string" },
        "target_site_ratio": { "type": "number", "minimum": 0 },
        "hold_days": { "type": "integer", "minimum": 0, "maximum": 365, "default": 0 },
        "max_adds_per_round": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
        "delete_on_free_expired": { "type": "boolean", "default": false },
        "max_seed_hours": { "type": "number", "minimum": 0 },
        "target_task_ratio": { "type": "number", "minimum": 0 },
        "target_uploaded_bytes": { "type": "integer", "minimum": 0, "x-ui": { "widget": "bytes" } },
        "max_download_hours": { "type": "number", "minimum": 0 },
        "min_avg_upspeed_bytes": { "type": "integer", "minimum": 0, "x-ui": { "widget": "bytes" } },
        "max_inactive_minutes": { "type": "integer", "minimum": 0 },
        "relative_directory": { "type": "string" },
        "category": { "type": "string" },
        "tags": {
            "type": "array",
            "items": { "type": "string" },
            "uniqueItems": true,
            "default": []
        },
        "upload_limit_bytes": { "type": "integer", "minimum": 0, "x-ui": { "widget": "bytes" } },
        "download_limit_bytes": { "type": "integer", "minimum": 0, "x-ui": { "widget": "bytes" } },
        "sequential": { "type": "boolean", "default": false },
        "first_last_piece": { "type": "boolean", "default": false }
    });
    if let Some(map) = extra.as_object() {
        for (k, v) in map {
            if include_site_id {
                let mut item = v.clone();
                if let Some(obj) = item.as_object_mut() {
                    obj.remove("default");
                }
                props.insert(k.clone(), item);
            } else {
                props.insert(k.clone(), v.clone());
            }
        }
    }
    props
}

#[cfg(test)]
fn boost_config_schema_json() -> String {
    use serde_json::{Map, json};
    let mut props = Map::new();
    let global = json!({
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
        "watermark_low_bytes": {
            "type": "integer",
            "minimum": 0,
            "x-ui": { "widget": "bytes" }
        },
        "dynamic_delete": {
            "type": "boolean",
            "default": false
        },
        "max_active": {
            "type": "integer",
            "minimum": 1,
            "maximum": 200,
            "default": 20
        },
        "max_downloading": {
            "type": "integer",
            "minimum": 1,
            "maximum": 200
        },
        "max_adds_per_hour": {
            "type": "integer",
            "minimum": 1,
            "maximum": 100,
            "default": 10
        },
        "dry_run": {
            "type": "boolean",
            "default": true
        },
        "auto_cleanup_files": {
            "type": "boolean",
            "default": false
        },
        "enforce_budget_on_all": {
            "type": "boolean",
            "default": false
        },
        "global_max_upspeed_bytes": {
            "type": "integer",
            "minimum": 0,
            "x-ui": { "widget": "bytes" }
        },
        "global_max_dlspeed_bytes": {
            "type": "integer",
            "minimum": 0,
            "x-ui": { "widget": "bytes" }
        }
    });
    if let Some(map) = global.as_object() {
        for (k, v) in map {
            props.insert(k.clone(), v.clone());
        }
    }
    for (k, v) in boost_rule_properties(false) {
        props.insert(k, v);
    }
    props.insert(
        "rules".into(),
        json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": boost_rule_properties(true),
                "required": ["site_id"],
                "additionalProperties": false
            },
            "default": []
        }),
    );
    json!({
        "type": "object",
        "properties": props,
        "additionalProperties": false
    })
    .to_string()
}

#[cfg(test)]
fn boost_task_input_schema_json() -> String {
    use serde_json::json;
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn boost_config_and_input_schemas_are_valid() {
        let cfg_val: Value = serde_json::from_str(&boost_config_schema_json()).unwrap();
        let cfg_schema = plugin_package::validate_config_schema(&cfg_val)
            .expect("boost_config_schema_json must validate cleanly");
        assert!(
            cfg_schema
                .root_json()
                .get("properties")
                .and_then(|p| p.get("rules"))
                .is_some()
        );
        let input_val: Value = serde_json::from_str(&boost_task_input_schema_json()).unwrap();
        plugin_package::validate_config_schema(&input_val)
            .expect("boost_task_input_schema_json must validate cleanly");
    }
}
