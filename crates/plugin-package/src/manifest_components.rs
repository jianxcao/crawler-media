use crate::archive::validate_safe_rel_path;
use crate::manifest_rules::{validate_plain_text, validate_slug_id};
use crate::{ComponentManifest, PackageError, PackagePolicy};
use std::collections::HashSet;

const ENDPOINT_METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE"];
const INDEXER_CAPS: &[&str] = &[
    "search",
    "rss",
    "detail",
    "resolve-download",
    "user-stats",
    "categories",
];
const MAINTENANCE_ACTIONS: &[&str] = &["verify-session", "check-in"];
const READ_CAPS: &[&str] = &[
    "read:sites",
    "read:subscribes",
    "read:library",
    "read:downloads",
    "read:history",
    "read:catalog",
    "read:boost",
];
const STATIC_CAPS: &[&str] = &[
    "kv",
    "blob",
    "notify:send",
    "http",
    "http:site-auth",
    "browser:search",
    "browser:maintenance",
    "indexer:provide",
    "indexer:yaml",
    "maintenance:provide",
    "endpoints",
];
const CREDENTIAL_CAPS: &[&str] = &[
    "http:site-auth",
    "browser:search",
    "browser:maintenance",
    "indexer:provide",
    "maintenance:provide",
];

pub(crate) fn validate_component(
    c: &ComponentManifest,
    api: &str,
    policy: &PackagePolicy,
    comp_ids: &mut HashSet<String>,
    task_ids: &mut HashSet<String>,
    needs_cred_binding: &mut bool,
) -> Result<(), PackageError> {
    validate_slug_id("component.id", &c.id, 64)?;
    if !comp_ids.insert(c.id.clone()) {
        return Err(PackageError::InvalidManifest(format!(
            "duplicate component id `{}`",
            c.id
        )));
    }
    validate_safe_rel_path(&c.entry, &c.entry)?;
    if !c.entry.ends_with(".wasm") {
        return Err(PackageError::InvalidManifest(format!(
            "component entry `{}` must end with .wasm",
            c.entry
        )));
    }
    if !policy.allowed_worlds.iter().any(|w| w == &c.world) {
        return Err(PackageError::UnsupportedWorld {
            api: api.to_string(),
            world: c.world.clone(),
        });
    }

    let mut perm_set = HashSet::new();
    for cap in &c.permissions {
        validate_component_capability(cap, policy)?;
        if CREDENTIAL_CAPS.contains(&cap.as_str()) {
            *needs_cred_binding = true;
        }
        perm_set.insert(cap.as_str());
    }

    validate_component_world_tables(c, policy, &perm_set, task_ids, needs_cred_binding)
}

fn validate_component_world_tables(
    c: &ComponentManifest,
    policy: &PackagePolicy,
    perm_set: &HashSet<&str>,
    task_ids: &mut HashSet<String>,
    needs_cred_binding: &mut bool,
) -> Result<(), PackageError> {
    if !c.lists.is_empty() || !c.channels.is_empty() {
        return Err(PackageError::InvalidManifest(
            "lists and channels tables are not supported in Phase 1".into(),
        ));
    }
    let world_name = c
        .world
        .split('/')
        .nth(1)
        .and_then(|s| s.split('@').next())
        .unwrap_or("");

    if matches!(world_name, "indexer-plugin" | "maintenance-plugin") {
        *needs_cred_binding = true;
    }

    let check_empty = |ok: bool, tbl: &str| {
        if !ok {
            Err(PackageError::InvalidManifest(format!(
                "table `{tbl}` is not allowed on world `{}`",
                c.world
            )))
        } else {
            Ok(())
        }
    };
    check_empty(world_name == "hook-plugin" || c.hooks.is_empty(), "hooks")?;
    check_empty(world_name == "task-plugin" || c.tasks.is_empty(), "tasks")?;
    check_empty(
        world_name == "endpoint-plugin" || c.endpoints.is_empty(),
        "endpoints",
    )?;
    check_empty(
        world_name == "indexer-plugin" || c.indexers.is_empty(),
        "indexers",
    )?;
    check_empty(
        world_name == "maintenance-plugin" || c.maintenance.is_empty(),
        "maintenance",
    )?;

    match world_name {
        "hook-plugin" => validate_hooks_table(c, policy),
        "task-plugin" => validate_tasks_table(c, task_ids),
        "endpoint-plugin" => validate_endpoints_table(c, perm_set, policy),
        "indexer-plugin" => validate_indexers_table(c, perm_set),
        "maintenance-plugin" => validate_maintenance_table(c, perm_set),
        other => Err(PackageError::UnsupportedWorld {
            api: policy.api_version.clone(),
            world: other.to_string(),
        }),
    }
}

fn validate_hooks_table(c: &ComponentManifest, policy: &PackagePolicy) -> Result<(), PackageError> {
    if c.hooks.is_empty() || c.hooks.len() > 32 {
        return Err(PackageError::InvalidManifest(
            "hook-plugin must declare 1..=32 hooks".into(),
        ));
    }
    let mut seen = HashSet::new();
    for h in &c.hooks {
        if !policy.allowed_hooks.iter().any(|allowed| allowed == &h.id) {
            return Err(PackageError::UnsupportedHook(h.id.clone()));
        }
        if !seen.insert(h.id.as_str()) {
            return Err(PackageError::InvalidManifest(format!(
                "duplicate hook id `{}`",
                h.id
            )));
        }
        if !(0..=1000).contains(&h.priority) {
            return Err(PackageError::InvalidManifest(format!(
                "hook `{}` priority {} out of 0..=1000",
                h.id, h.priority
            )));
        }
    }
    Ok(())
}

fn validate_tasks_table(
    c: &ComponentManifest,
    task_ids: &mut HashSet<String>,
) -> Result<(), PackageError> {
    if c.tasks.is_empty() || c.tasks.len() > 32 {
        return Err(PackageError::InvalidManifest(
            "task-plugin must declare 1..=32 tasks".into(),
        ));
    }
    let mut local = HashSet::new();
    for t in &c.tasks {
        validate_slug_id("task.id", &t.id, 64)?;
        if !local.insert(t.id.as_str()) || !task_ids.insert(t.id.clone()) {
            return Err(PackageError::InvalidManifest(format!(
                "duplicate task id `{}`",
                t.id
            )));
        }
        validate_safe_rel_path(&t.input_schema, &t.input_schema)?;
        if t.schedule.kind != "interval" {
            return Err(PackageError::InvalidManifest(format!(
                "unsupported task schedule kind `{}`; only `interval` is supported",
                t.schedule.kind
            )));
        }
        if !(60..=86400).contains(&t.schedule.seconds) {
            return Err(PackageError::InvalidManifest(format!(
                "task `{}` schedule.seconds {} must be in 60..=86400",
                t.id, t.schedule.seconds
            )));
        }
    }
    Ok(())
}

fn validate_endpoints_table(
    c: &ComponentManifest,
    perm_set: &HashSet<&str>,
    policy: &PackagePolicy,
) -> Result<(), PackageError> {
    if c.endpoints.is_empty() || c.endpoints.len() > 32 {
        return Err(PackageError::InvalidManifest(
            "endpoint-plugin must declare 1..=32 endpoints".into(),
        ));
    }
    let mut ids = HashSet::new();
    let mut routes = HashSet::new();
    for ep in &c.endpoints {
        validate_slug_id("endpoint.id", &ep.id, 64)?;
        if !ids.insert(ep.id.as_str()) {
            return Err(PackageError::InvalidManifest(format!(
                "duplicate endpoint id `{}`",
                ep.id
            )));
        }
        if !ENDPOINT_METHODS.contains(&ep.method.as_str()) {
            return Err(PackageError::InvalidManifest(format!(
                "invalid endpoint method `{}`",
                ep.method
            )));
        }
        if !ep.path.starts_with('/')
            || ep.path.contains("..")
            || ep.path.contains('?')
            || ep.path.contains('#')
            || ep.path.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(PackageError::InvalidManifest(format!(
                "invalid endpoint path `{}`",
                ep.path
            )));
        }
        if !routes.insert((ep.method.as_str(), ep.path.as_str())) {
            return Err(PackageError::InvalidManifest(format!(
                "duplicate endpoint route `{} {}`",
                ep.method, ep.path
            )));
        }
        if !policy.allowed_endpoint_auths.iter().any(|a| a == &ep.auth) {
            return Err(PackageError::InvalidManifest(format!(
                "invalid endpoint auth `{}`; allowed: {}",
                ep.auth,
                policy.allowed_endpoint_auths.join("/")
            )));
        }
        validate_safe_rel_path(&ep.input_schema, &ep.input_schema)?;
        for cap in &ep.capabilities {
            if !(cap.starts_with("read:") || cap.starts_with("command:"))
                || !perm_set.contains(cap.as_str())
            {
                return Err(PackageError::InvalidManifest(format!(
                    "endpoint `{}` capability `{cap}` is not declared in component permissions",
                    ep.id
                )));
            }
        }
    }
    Ok(())
}

fn validate_indexers_table(
    c: &ComponentManifest,
    perm_set: &HashSet<&str>,
) -> Result<(), PackageError> {
    if !perm_set.contains("indexer:provide") {
        return Err(PackageError::InvalidManifest(
            "indexer-plugin component must declare `indexer:provide` in permissions".into(),
        ));
    }
    if c.indexers.is_empty() || c.indexers.len() > 32 {
        return Err(PackageError::InvalidManifest(
            "indexer-plugin must declare 1..=32 indexers".into(),
        ));
    }
    let mut ids = HashSet::new();
    for idx in &c.indexers {
        validate_slug_id("indexer.id", &idx.id, 64)?;
        if !ids.insert(idx.id.as_str()) {
            return Err(PackageError::InvalidManifest(format!(
                "duplicate indexer id `{}`",
                idx.id
            )));
        }
        validate_plain_text("indexer.name", &idx.name, 80, false)?;
        validate_plain_text("indexer.kind", &idx.kind, 40, false)?;
        if idx.capabilities.is_empty() {
            return Err(PackageError::InvalidManifest(
                "indexer capabilities must be non-empty".into(),
            ));
        }
        for cap in &idx.capabilities {
            if !INDEXER_CAPS.contains(&cap.as_str()) {
                return Err(PackageError::InvalidManifest(format!(
                    "unknown indexer capability `{cap}`"
                )));
            }
        }
        validate_safe_rel_path(&idx.site_schema, &idx.site_schema)?;
    }
    Ok(())
}

fn validate_maintenance_table(
    c: &ComponentManifest,
    perm_set: &HashSet<&str>,
) -> Result<(), PackageError> {
    if !perm_set.contains("maintenance:provide") {
        return Err(PackageError::InvalidManifest(
            "maintenance-plugin component must declare `maintenance:provide` in permissions".into(),
        ));
    }
    if c.maintenance.is_empty() || c.maintenance.len() > 32 {
        return Err(PackageError::InvalidManifest(
            "maintenance-plugin must declare 1..=32 maintenance entries".into(),
        ));
    }
    let mut ids = HashSet::new();
    for item in &c.maintenance {
        validate_slug_id("maintenance.id", &item.id, 64)?;
        if !ids.insert(item.id.as_str()) {
            return Err(PackageError::InvalidManifest(format!(
                "duplicate maintenance id `{}`",
                item.id
            )));
        }
        if item.actions.is_empty() {
            return Err(PackageError::InvalidManifest(
                "maintenance actions must be non-empty".into(),
            ));
        }
        for act in &item.actions {
            if !MAINTENANCE_ACTIONS.contains(&act.as_str()) {
                return Err(PackageError::InvalidManifest(format!(
                    "unknown maintenance action `{act}`"
                )));
            }
        }
    }
    Ok(())
}

fn validate_component_capability(cap: &str, policy: &PackagePolicy) -> Result<(), PackageError> {
    if cap.is_empty() || cap.contains('*') || cap.starts_with("task:run:") {
        return Err(PackageError::UnknownCapability(cap.to_string()));
    }
    if STATIC_CAPS.contains(&cap) || READ_CAPS.contains(&cap) {
        return Ok(());
    }
    if let Some(hook_id) = cap.strip_prefix("hooks:") {
        if policy.allowed_hooks.iter().any(|h| h == hook_id) {
            return Ok(());
        }
        return Err(PackageError::UnknownCapability(cap.to_string()));
    }
    if let Some(domain) = cap.strip_prefix("events:") {
        if policy.allowed_events.iter().any(|e| e == domain) {
            return Ok(());
        }
        return Err(PackageError::UnknownCapability(cap.to_string()));
    }
    if let Some(cmd) = cap.strip_prefix("command:")
        && policy.allowed_commands.iter().any(|c| c == cmd)
    {
        return Ok(());
    }
    Err(PackageError::UnknownCapability(cap.to_string()))
}

pub(crate) fn validate_credential_binding(
    binding: Option<&str>,
    required: bool,
) -> Result<(), PackageError> {
    match binding {
        Some("invocation-site") => Ok(()),
        Some(other) => Err(PackageError::InvalidManifest(format!(
            "invalid credential_binding `{other}`; only `invocation-site` is allowed"
        ))),
        None if required => Err(PackageError::InvalidManifest(
            "credential_binding = \"invocation-site\" is required for site-auth/browser/indexer/maintenance capabilities".into(),
        )),
        None => Ok(()),
    }
}
