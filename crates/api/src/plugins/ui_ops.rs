use axum::http::HeaderMap;
use domain::UserId;
use plugin_package::{PluginManifest, UiManifest, UiSurfaceKind};
use serde::Serialize;
use store::plugins::{PluginLifecycle, PluginRecord, PluginUiTrustRecord, PluginVersionMetadata};

use super::service::{PluginService, run_app_read};
use super::types::PluginServiceError;
use super::ui_asset_io::{read_external_ui_asset_off_lock, resolve_ui_styles};
pub(crate) use super::ui_asset_io::{resolve_bundle_sha256, validate_asset_request_path};
use crate::management::ApiState;

#[derive(Clone, Debug, Serialize)]
pub struct PluginUiContractView {
    pub contract: &'static str,
    pub react: &'static str,
    pub react_dom: &'static str,
    pub plugin_ui_sdk: &'static str,
    pub plugin_ui_kit: &'static str,
    pub federation_runtime_version: &'static str,
    pub theme_schema_version: &'static str,
    pub sdk: &'static str,
    pub sdk_methods: Vec<&'static str>,
    pub shared: serde_json::Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct UiTrustStateView {
    pub status: String,
    pub revision: i64,
    pub granted_at: Option<String>,
    pub granted_by_user_id: Option<String>,
    pub revoked_at: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct UiSurfaceDescriptorView {
    pub id: String,
    pub kind: UiSurfaceKind,
    pub expose: String,
    pub title: String,
    pub icon: Option<String>,
    pub location: Option<String>,
    pub mountable: bool,
    pub editable: bool,
    pub required_role: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct UiDescriptorView {
    pub plugin_id: String,
    pub plugin_name: String,
    pub version: String,
    pub generation: i64,
    pub lifecycle: PluginLifecycle,
    pub source: &'static str,
    pub package_hash: String,
    pub package_sha256: String,
    pub bundle_sha256: String,
    pub subject_user_id: String,
    pub subject_role: &'static str,
    pub ui_mode: String,
    pub remote_name: String,
    pub entry: String,
    pub entry_url: String,
    pub asset_base_url: String,
    pub contract: String,
    pub react: String,
    pub react_dom: String,
    pub ui_trusted: bool,
    pub trust: UiTrustStateView,
    pub capabilities: Vec<String>,
    pub styles: Vec<String>,
    pub surfaces: Vec<UiSurfaceDescriptorView>,
}

#[derive(Clone, Debug, Serialize)]
pub struct UiNavigationItemView {
    pub plugin_id: String,
    pub plugin_name: String,
    pub generation: i64,
    pub remote_name: String,
    pub entry_url: String,
    pub id: String,
    pub surface_id: String,
    pub kind: UiSurfaceKind,
    pub title: String,
    pub icon: Option<String>,
    pub expose: String,
    pub location: String,
    pub path: String,
    pub required_role: &'static str,
    pub subject_user_id: String,
}

pub(crate) fn parse_manifest_with_ui(
    version: &PluginVersionMetadata,
) -> Result<(PluginManifest, UiManifest), PluginServiceError> {
    let manifest: PluginManifest = serde_json::from_str(&version.manifest_json).map_err(|_| {
        PluginServiceError::not_found("plugin.ui_not_declared", "plugin has invalid manifest")
    })?;
    let ui = manifest.ui.clone().ok_or_else(|| {
        PluginServiceError::not_found("plugin.ui_not_declared", "plugin does not declare a UI")
    })?;
    Ok((manifest, ui))
}

pub(crate) fn evaluate_trust(
    version: &PluginVersionMetadata,
    row: Option<&PluginUiTrustRecord>,
) -> (bool, UiTrustStateView) {
    match row {
        Some(rec) => {
            let trusted = rec.revoked_at.is_none();
            (
                trusted,
                UiTrustStateView {
                    status: if trusted { "trusted" } else { "revoked" }.into(),
                    revision: rec.revision,
                    granted_at: Some(rec.granted_at.clone()),
                    granted_by_user_id: Some(rec.granted_by_user_id.clone()),
                    revoked_at: rec.revoked_at.clone(),
                },
            )
        }
        None => {
            let is_builtin = version.signature_status == "builtin";
            (
                is_builtin,
                UiTrustStateView {
                    status: if is_builtin { "trusted" } else { "untrusted" }.into(),
                    revision: 0,
                    granted_at: None,
                    granted_by_user_id: is_builtin.then(|| "system".into()),
                    revoked_at: None,
                },
            )
        }
    }
}

fn surface_location(plugin_id: &str, id: &str, kind: UiSurfaceKind) -> Option<String> {
    match kind {
        UiSurfaceKind::Config => None,
        UiSurfaceKind::Page => Some(format!("/settings/plugins/{plugin_id}?tab=page&view={id}")),
        UiSurfaceKind::App => Some(format!("/plugin-apps/{plugin_id}/{id}")),
        UiSurfaceKind::Dashboard => Some("/dashboard".into()),
    }
}

pub(crate) fn build_ui_contract_view() -> PluginUiContractView {
    PluginUiContractView {
        contract: "1.0.0",
        react: "19.3.0",
        react_dom: "19.3.0",
        plugin_ui_sdk: "1.0.0",
        plugin_ui_kit: "1.0.0",
        federation_runtime_version: "0.10",
        theme_schema_version: "1.0.0",
        sdk: "@crawler-media/plugin-ui@1.0.0",
        sdk_methods: vec![
            "query",
            "command",
            "runTask",
            "openSurface",
            "navigate",
            "toast",
            "confirm",
            "reauth",
        ],
        shared: serde_json::json!({
            "react": { "singleton": true, "required_version": "19.3.0" },
            "react-dom": { "singleton": true, "required_version": "19.3.0" }
        }),
    }
}

pub(super) fn can_serve_disabled_config_ui(plugin: &PluginRecord, ui: &UiManifest) -> bool {
    ui.surfaces.iter().any(|s| s.kind == UiSurfaceKind::Config)
        && matches!(
            plugin.lifecycle,
            PluginLifecycle::Disabled | PluginLifecycle::InstalledDisabled
        )
        && !plugin.desired_enabled
        && plugin.active_operation_id.is_none()
}

fn header_str<'a>(
    headers: &'a HeaderMap,
    name: &str,
) -> Result<Option<&'a str>, PluginServiceError> {
    match headers.get(name) {
        None => Ok(None),
        Some(v) => v.to_str().map(Some).map_err(|_| {
            PluginServiceError::invalid("plugin.invalid_input", format!("invalid {name} header"))
        }),
    }
}

impl PluginService {
    pub(crate) async fn get_plugin_ui_descriptor(
        &self,
        state: &ApiState,
        plugin_id: &str,
        caller_user_id: UserId,
    ) -> Result<UiDescriptorView, PluginServiceError> {
        let pid = plugin_id.to_string();
        let (plugin, version, trust_row) = run_app_read(state, move |store| {
            let p = store.get_plugin(&pid)?.ok_or_else(|| {
                PluginServiceError::not_found("plugin.missing", "plugin not found")
            })?;
            let ver = store
                .get_plugin_version(&pid, &p.package_hash)?
                .ok_or_else(|| {
                    PluginServiceError::not_found("plugin.missing", "plugin version not found")
                })?;
            let trust = store.get_ui_trust(&pid, &p.package_hash)?;
            Ok((p, ver, trust))
        })
        .await?;

        let (manifest, ui) = parse_manifest_with_ui(&version)?;
        let bundle_sha256 = resolve_bundle_sha256(state.store.data_dir(), &version, &ui).await?;
        let styles = resolve_ui_styles(state.store.data_dir(), &version, &ui).await?;
        let (ui_trusted, trust) = evaluate_trust(&version, trust_row.as_ref());
        let is_enabled = plugin.lifecycle == PluginLifecycle::Enabled && plugin.desired_enabled;
        let is_enabled_idle = is_enabled && plugin.active_operation_id.is_none();
        let can_serve_disabled = can_serve_disabled_config_ui(&plugin, &ui);
        let surfaces = ui
            .surfaces
            .into_iter()
            .map(|s| UiSurfaceDescriptorView {
                location: surface_location(&plugin.plugin_id, &s.id, s.kind),
                mountable: match s.kind {
                    UiSurfaceKind::Config => is_enabled_idle || can_serve_disabled,
                    UiSurfaceKind::Page | UiSurfaceKind::App | UiSurfaceKind::Dashboard => {
                        is_enabled
                    }
                },
                editable: s.kind == UiSurfaceKind::Config && can_serve_disabled,
                id: s.id,
                kind: s.kind,
                expose: s.expose,
                title: s.title,
                icon: s.icon,
                required_role: "admin",
            })
            .collect();

        Ok(UiDescriptorView {
            plugin_name: manifest.name,
            version: version.version,
            generation: plugin.generation,
            lifecycle: plugin.lifecycle,
            source: if version.signature_status == "builtin" {
                "builtin"
            } else {
                "external"
            },
            entry_url: format!(
                "/api/v1/plugin-ui-assets/{}/{}/{}",
                plugin.plugin_id, plugin.package_hash, ui.entry
            ),
            asset_base_url: format!(
                "/api/v1/plugin-ui-assets/{}/{}",
                plugin.plugin_id, plugin.package_hash
            ),
            package_hash: plugin.package_hash.clone(),
            package_sha256: plugin.package_hash,
            bundle_sha256,
            plugin_id: plugin.plugin_id,
            subject_user_id: caller_user_id.to_string(),
            subject_role: "admin",
            ui_mode: ui.kind,
            remote_name: ui.remote_name,
            entry: ui.entry,
            contract: ui.contract,
            react: ui.react,
            react_dom: ui.react_dom,
            ui_trusted,
            trust,
            capabilities: ui.capabilities,
            styles,
            surfaces,
        })
    }

    pub(crate) async fn list_plugin_ui_navigation(
        &self,
        state: &ApiState,
        caller_user_id: UserId,
    ) -> Result<Vec<UiNavigationItemView>, PluginServiceError> {
        let records = run_app_read(state, move |store| {
            let mut out = Vec::new();
            let mut cursor: Option<String> = None;
            loop {
                let page = store.list_plugins_page(None, cursor.as_deref(), Some(200))?;
                for p in page.items {
                    if p.lifecycle != PluginLifecycle::Enabled || !p.desired_enabled {
                        continue;
                    }
                    let Some(ver) = store.get_plugin_version(&p.plugin_id, &p.package_hash)? else {
                        continue;
                    };
                    let trust = store.get_ui_trust(&p.plugin_id, &p.package_hash)?;
                    out.push((p, ver, trust));
                }
                match page.next_cursor {
                    Some(next) if Some(&next) != cursor.as_ref() => cursor = Some(next),
                    _ => break,
                }
            }
            Ok(out)
        })
        .await?;

        let mut nav = Vec::new();
        let uid = caller_user_id.to_string();
        for (p, ver, trust) in records {
            let Ok((manifest, ui)) = parse_manifest_with_ui(&ver) else {
                continue;
            };
            let (trusted, _) = evaluate_trust(&ver, trust.as_ref());
            if !trusted {
                continue;
            }
            let entry_url = format!(
                "/api/v1/plugin-ui-assets/{}/{}/{}",
                p.plugin_id, p.package_hash, ui.entry
            );
            for s in ui.surfaces {
                if let Some(location) = surface_location(&p.plugin_id, &s.id, s.kind) {
                    nav.push(UiNavigationItemView {
                        plugin_id: p.plugin_id.clone(),
                        plugin_name: manifest.name.clone(),
                        generation: p.generation,
                        remote_name: ui.remote_name.clone(),
                        entry_url: entry_url.clone(),
                        id: s.id.clone(),
                        surface_id: s.id,
                        kind: s.kind,
                        title: s.title,
                        icon: s.icon,
                        expose: s.expose,
                        path: location.clone(),
                        location,
                        required_role: "admin",
                        subject_user_id: uid.clone(),
                    });
                }
            }
        }
        Ok(nav)
    }

    pub(crate) async fn resolve_ui_asset(
        &self,
        state: &ApiState,
        plugin_id: &str,
        bundle_hash: &str,
        raw_uri_path: &str,
        asset_path: &str,
    ) -> Result<(Vec<u8>, String), PluginServiceError> {
        let clean_rel = validate_asset_request_path(raw_uri_path, asset_path)?;
        let pid = plugin_id.to_string();
        let (plugin, version, trust_row) = run_app_read(state, move |store| {
            let p = store.get_plugin(&pid)?.ok_or_else(|| {
                PluginServiceError::not_found("plugin.missing", "plugin not found")
            })?;
            let ver = store
                .get_plugin_version(&pid, &p.package_hash)?
                .ok_or_else(|| {
                    PluginServiceError::not_found("plugin.missing", "plugin version not found")
                })?;
            let trust = store.get_ui_trust(&pid, &p.package_hash)?;
            Ok((p, ver, trust))
        })
        .await?;

        let (_, ui) = parse_manifest_with_ui(&version)?;
        if bundle_hash != plugin.package_hash {
            return Err(PluginServiceError::not_found(
                "plugin.asset_not_found",
                "requested UI bundle hash does not match active plugin package",
            ));
        }
        let (trusted, _) = evaluate_trust(&version, trust_row.as_ref());
        if !trusted {
            return Err(PluginServiceError::forbidden(
                "plugin.ui_untrusted",
                "plugin UI bundle is not trusted",
            ));
        }
        let is_enabled = plugin.lifecycle == PluginLifecycle::Enabled && plugin.desired_enabled;
        let has_non_config_surface = ui.surfaces.iter().any(|s| s.kind != UiSurfaceKind::Config);
        let can_serve_enabled =
            is_enabled && (plugin.active_operation_id.is_none() || has_non_config_surface);
        if !can_serve_enabled && !can_serve_disabled_config_ui(&plugin, &ui) {
            return Err(PluginServiceError::forbidden(
                "plugin.not_enabled",
                "plugin must be enabled to serve UI assets",
            ));
        }
        if version.signature_status == "builtin" {
            return Err(PluginServiceError::not_found(
                "plugin.asset_not_found",
                "UI asset not found",
            ));
        }
        let expected_bundle_sha = trust_row
            .as_ref()
            .map(|r| r.bundle_sha256.trim())
            .filter(|s| !s.is_empty());
        read_external_ui_asset_off_lock(
            state.store.data_dir(),
            &version,
            &ui,
            expected_bundle_sha,
            asset_path,
            &clean_rel,
        )
        .await
    }

    pub(crate) async fn verify_plugin_attribution(
        &self,
        state: &ApiState,
        plugin_id: &str,
        headers: &HeaderMap,
    ) -> Result<(PluginRecord, Option<i64>), PluginServiceError> {
        self.verify_attribution_with_mode(state, plugin_id, headers, true, &[])
            .await
    }

    pub(crate) async fn verify_plugin_attribution_with_cap(
        &self,
        state: &ApiState,
        plugin_id: &str,
        headers: &HeaderMap,
        required_ui_cap: Option<&str>,
    ) -> Result<(PluginRecord, Option<i64>), PluginServiceError> {
        let caps = required_ui_cap.as_slice();
        self.verify_attribution_with_mode(state, plugin_id, headers, true, caps)
            .await
    }

    pub(crate) async fn verify_plugin_attribution_with_any_cap(
        &self,
        state: &ApiState,
        plugin_id: &str,
        headers: &HeaderMap,
        required_ui_caps: &[&str],
    ) -> Result<(PluginRecord, Option<i64>), PluginServiceError> {
        self.verify_attribution_with_mode(state, plugin_id, headers, true, required_ui_caps)
            .await
    }

    pub(crate) async fn verify_config_attribution(
        &self,
        state: &ApiState,
        plugin_id: &str,
        headers: &HeaderMap,
    ) -> Result<(PluginRecord, Option<i64>), PluginServiceError> {
        self.verify_attribution_with_mode(state, plugin_id, headers, false, &[])
            .await
    }

    async fn verify_attribution_with_mode(
        &self,
        state: &ApiState,
        plugin_id: &str,
        headers: &HeaderMap,
        require_enabled: bool,
        required_ui_caps: &[&str],
    ) -> Result<(PluginRecord, Option<i64>), PluginServiceError> {
        let pkg_hdr = header_str(headers, "x-plugin-package")?;
        let gen_hdr = header_str(headers, "x-plugin-generation")?;
        if pkg_hdr.is_some() != gen_hdr.is_some() {
            return Err(PluginServiceError::invalid(
                "plugin.invalid_input",
                "X-Plugin-Package and X-Plugin-Generation must be provided together",
            ));
        }
        let parsed_gen = match gen_hdr {
            Some(raw) => Some(raw.trim().parse::<i64>().map_err(|_| {
                PluginServiceError::invalid(
                    "plugin.invalid_input",
                    "X-Plugin-Generation must be a valid integer",
                )
            })?),
            None => None,
        };

        let pid = plugin_id.to_string();
        let (plugin, version, trust_row) = run_app_read(state, move |store| {
            let p = store.get_plugin(&pid)?.ok_or_else(|| {
                PluginServiceError::not_found("plugin.missing", "plugin not found")
            })?;
            let ver = store.get_plugin_version(&pid, &p.package_hash)?;
            let trust = store.get_ui_trust(&pid, &p.package_hash)?;
            Ok((p, ver, trust))
        })
        .await?;

        if let (Some(expected_pkg), Some(expected_gen)) = (pkg_hdr, parsed_gen) {
            if require_enabled {
                if plugin.lifecycle != PluginLifecycle::Enabled || !plugin.desired_enabled {
                    return Err(PluginServiceError::forbidden(
                        "plugin.not_enabled",
                        "plugin is not enabled",
                    ));
                }
            } else if matches!(
                plugin.lifecycle,
                PluginLifecycle::Draining | PluginLifecycle::Enabling
            ) || plugin.active_operation_id.is_some()
            {
                return Err(PluginServiceError::conflict(
                    "plugin.not_drained",
                    "plugin operation in progress",
                ));
            }
            if expected_pkg.trim() != plugin.package_hash {
                return Err(PluginServiceError::conflict(
                    "plugin.package_conflict",
                    "X-Plugin-Package does not match active package",
                ));
            }
            if expected_gen != plugin.generation {
                return Err(PluginServiceError::conflict(
                    "plugin.stale_generation",
                    "X-Plugin-Generation does not match active generation",
                ));
            }
            let ver = version.as_ref().ok_or_else(|| {
                PluginServiceError::not_found("plugin.missing", "plugin version not found")
            })?;
            let (_, ui) = parse_manifest_with_ui(ver).map_err(|_| {
                PluginServiceError::forbidden(
                    "plugin.ui_untrusted",
                    "plugin does not declare a trusted UI",
                )
            })?;
            if !require_enabled {
                let is_enabled_idle = plugin.lifecycle == PluginLifecycle::Enabled
                    && plugin.desired_enabled
                    && plugin.active_operation_id.is_none();
                if !is_enabled_idle && !can_serve_disabled_config_ui(&plugin, &ui) {
                    return Err(PluginServiceError::forbidden(
                        "plugin.not_enabled",
                        "plugin Config UI is not enabled or available",
                    ));
                }
            }
            let (trusted, _) = evaluate_trust(ver, trust_row.as_ref());
            if !trusted {
                return Err(PluginServiceError::forbidden(
                    "plugin.ui_untrusted",
                    "plugin UI is not trusted",
                ));
            }
            if !required_ui_caps.is_empty()
                && !required_ui_caps
                    .iter()
                    .any(|cap| ui_capabilities_allow(&ui.capabilities, cap))
            {
                return Err(PluginServiceError::forbidden(
                    "plugin.capability_denied",
                    format!(
                        "plugin UI lacks required capability ({})",
                        required_ui_caps.join(" | ")
                    ),
                ));
            }
        }
        Ok((plugin, parsed_gen))
    }
}

pub(crate) fn has_attribution_headers(headers: &HeaderMap) -> bool {
    headers.contains_key("x-plugin-package") || headers.contains_key("x-plugin-generation")
}

fn ui_capabilities_allow(caps: &[String], req_cap: &str) -> bool {
    if caps.iter().any(|c| c == req_cap) {
        return true;
    }
    matches!(
        req_cap,
        "command:site.check-in" | "command:site.verify-session"
    ) && caps.iter().any(|c| c == "command:site.maintenance")
}
