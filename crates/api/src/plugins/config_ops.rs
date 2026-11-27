use std::collections::{BTreeMap, BTreeSet};

use plugin_package::{SecretWriteAction, validate_config_schema};
use store::plugins::{
    PluginError, PluginLifecycle, PluginSecretChange, PluginSecretMetadata, PluginSecretUpdate,
    PluginSecurityScope, PluginSubject,
};
use store::{App, StoreError};

use super::idempotency::{check_idempotent_replay, validate_idempotency_input};
use super::service::{PluginService, run_app_read, run_app_write};
use super::types::{
    ConfigValidationView, PluginConfigSchemaView, PluginConfigView, PluginServiceError,
    UpdateConfigInput, ValidateConfigInput, map_package_err,
};
use crate::management::ApiState;

fn validate_secret_actions_for_scopes<L: ConfigObjectLookup>(
    lookup: &L,
    schema: &plugin_package::ValidatedConfigSchema,
    values: &serde_json::Value,
    secrets: &BTreeMap<String, SecretWriteAction>,
    stored_secrets: &[PluginSecretMetadata],
    credential_bound: bool,
    specific_site: Option<domain::SiteId>,
) -> Result<(serde_json::Value, Vec<PluginSecretUpdate>), PluginServiceError> {
    let resolved = resolve_config_write_for_scopes(
        lookup,
        schema,
        values,
        secrets,
        stored_secrets,
        credential_bound,
        specific_site,
    )?
    .map_err(|fields| {
        PluginServiceError::config_invalid("configuration validation failed", fields)
    })?;
    let updates = resolved
        .secret_updates
        .into_iter()
        .map(|u| PluginSecretUpdate {
            key: u.key,
            change: match u.change {
                SecretWriteAction::Keep => PluginSecretChange::Keep,
                SecretWriteAction::Replace(v) => PluginSecretChange::Replace(v.into_bytes()),
                SecretWriteAction::Clear => PluginSecretChange::Clear,
            },
        })
        .collect();
    Ok((resolved.public_values, updates))
}

pub(super) trait ConfigObjectLookup {
    fn observe_site(
        &self,
        id: domain::SiteId,
    ) -> Result<Option<store::SiteObservation>, PluginServiceError>;
    fn site_exists(&self, site_id: domain::SiteId) -> Result<bool, StoreError>;
    fn downloader_exists(&self, dl_id: domain::DownloaderId) -> Result<bool, StoreError>;
}

pub(super) trait PluginConfigLookup: ConfigObjectLookup {
    fn get_plugin_config(
        &self,
        id: &str,
    ) -> Result<Option<store::plugins::PluginConfig>, StoreError>;
    fn get_plugin_config_summary(
        &self,
        id: &str,
    ) -> Result<store::plugins::PluginConfigSummary, StoreError>;
}

impl ConfigObjectLookup for App<'_> {
    fn observe_site(
        &self,
        id: domain::SiteId,
    ) -> Result<Option<store::SiteObservation>, PluginServiceError> {
        Ok(App::observe_site(self, id)?)
    }
    fn site_exists(&self, site_id: domain::SiteId) -> Result<bool, StoreError> {
        Ok(App::observe_site(self, site_id)?.is_some())
    }
    fn downloader_exists(&self, dl_id: domain::DownloaderId) -> Result<bool, StoreError> {
        Ok(self.get_downloader(dl_id)?.is_some()
            || (dl_id == domain::DownloaderId::from_uuid(uuid::Uuid::nil())
                && self.default_downloader()?.is_none()))
    }
}

impl PluginConfigLookup for App<'_> {
    fn get_plugin_config(
        &self,
        id: &str,
    ) -> Result<Option<store::plugins::PluginConfig>, StoreError> {
        App::get_plugin_config(self, id)
    }
    fn get_plugin_config_summary(
        &self,
        id: &str,
    ) -> Result<store::plugins::PluginConfigSummary, StoreError> {
        App::get_plugin_config_summary(self, id)
    }
}

impl ConfigObjectLookup for store::Store {
    fn observe_site(
        &self,
        id: domain::SiteId,
    ) -> Result<Option<store::SiteObservation>, PluginServiceError> {
        Ok(store::Store::observe_site(self, id)?)
    }
    fn site_exists(&self, site_id: domain::SiteId) -> Result<bool, StoreError> {
        Ok(store::Store::observe_site(self, site_id)?.is_some())
    }
    fn downloader_exists(&self, dl_id: domain::DownloaderId) -> Result<bool, StoreError> {
        Ok(self.get_downloader(dl_id)?.is_some()
            || (dl_id == domain::DownloaderId::from_uuid(uuid::Uuid::nil())
                && self.default_downloader()?.is_none()))
    }
}

impl PluginConfigLookup for store::Store {
    fn get_plugin_config(
        &self,
        id: &str,
    ) -> Result<Option<store::plugins::PluginConfig>, StoreError> {
        store::Store::get_plugin_config(self, id)
    }
    fn get_plugin_config_summary(
        &self,
        id: &str,
    ) -> Result<store::plugins::PluginConfigSummary, StoreError> {
        store::Store::get_plugin_config_summary(self, id)
    }
}

pub(super) fn is_live_secret_scope<L: ConfigObjectLookup>(
    lookup: &L,
    credential_bound: bool,
    specific_site: Option<domain::SiteId>,
    scope: &PluginSecurityScope,
) -> Result<bool, PluginServiceError> {
    if !credential_bound {
        return Ok(scope == &PluginSecurityScope::global());
    }
    let Some(sid) = scope.site_id() else {
        return Ok(false);
    };
    if specific_site.is_some_and(|s| s != sid) {
        return Ok(false);
    }
    let obs = lookup.observe_site(sid)?;
    Ok(obs.is_some_and(|o| &PluginSecurityScope::for_site(&o) == scope))
}

pub(super) fn live_secret_keys_by_scope<L: ConfigObjectLookup>(
    lookup: &L,
    secrets: &[PluginSecretMetadata],
    credential_bound: bool,
    specific_site: Option<domain::SiteId>,
) -> Result<Vec<(PluginSecurityScope, BTreeSet<String>)>, PluginServiceError> {
    let mut by_scope: Vec<(PluginSecurityScope, BTreeSet<String>)> = Vec::new();
    for s in secrets {
        if s.configured && is_live_secret_scope(lookup, credential_bound, specific_site, &s.scope)?
        {
            if let Some((_, keys)) = by_scope.iter_mut().find(|(sc, _)| sc == &s.scope) {
                keys.insert(s.key.clone());
            } else {
                let mut keys = BTreeSet::new();
                keys.insert(s.key.clone());
                by_scope.push((s.scope.clone(), keys));
            }
        }
    }
    Ok(by_scope)
}

pub(super) fn resolve_config_write_for_scopes<L: ConfigObjectLookup>(
    lookup: &L,
    schema: &plugin_package::ValidatedConfigSchema,
    values: &serde_json::Value,
    actions: &BTreeMap<String, SecretWriteAction>,
    secrets: &[PluginSecretMetadata],
    credential_bound: bool,
    specific_site: Option<domain::SiteId>,
) -> Result<
    Result<plugin_package::ValidatedConfigWrite, Vec<plugin_package::SchemaFieldError>>,
    PluginServiceError,
> {
    let by_scope = live_secret_keys_by_scope(lookup, secrets, credential_bound, specific_site)?;
    if !credential_bound || specific_site.is_some() {
        let keys = by_scope
            .into_iter()
            .next()
            .map(|(_, k)| k)
            .unwrap_or_default();
        return Ok(schema.resolve_config_write(values, actions, &keys));
    }
    let empty = BTreeSet::new();
    let mut fallback_errs = match schema.resolve_config_write(values, actions, &empty) {
        Ok(resolved) => return Ok(Ok(resolved)),
        Err(errs) => errs,
    };
    for (_, scope_keys) in &by_scope {
        match schema.resolve_config_write(values, actions, scope_keys) {
            Ok(resolved) => return Ok(Ok(resolved)),
            Err(errs) if errs.len() < fallback_errs.len() => fallback_errs = errs,
            Err(_) => {}
        }
    }
    Ok(Err(fallback_errs))
}

fn live_configured_secret_keys_for_redaction<L: ConfigObjectLookup>(
    lookup: &L,
    secrets: &[PluginSecretMetadata],
    credential_bound: bool,
    specific_site: Option<domain::SiteId>,
) -> Result<BTreeSet<String>, PluginServiceError> {
    let mut keys = BTreeSet::new();
    for s in secrets {
        if s.configured && is_live_secret_scope(lookup, credential_bound, specific_site, &s.scope)?
        {
            keys.insert(s.key.clone());
        }
    }
    Ok(keys)
}

pub(super) fn validate_config_object_grants<L: ConfigObjectLookup>(
    lookup: &L,
    values: &serde_json::Value,
) -> Result<(), PluginServiceError> {
    if let Some(site_str) = values
        .get("site_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(super::ports::strip_site_prefix)
    {
        let site_id = site_str.parse::<domain::SiteId>().map_err(|_| {
            PluginServiceError::invalid("plugin.scope_invalid", "invalid site_id UUID")
        })?;
        if !lookup.site_exists(site_id)? {
            return Err(PluginServiceError::invalid(
                "plugin.scope_invalid",
                "referenced site_id does not exist",
            ));
        }
    }
    if let Some(sites) = values.get("sites").and_then(|v| v.as_array()) {
        for item in sites {
            let Some(site_str) = item
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(super::ports::strip_site_prefix)
            else {
                continue;
            };
            let site_id = site_str.parse::<domain::SiteId>().map_err(|_| {
                PluginServiceError::invalid("plugin.scope_invalid", "invalid sites[] UUID")
            })?;
            if !lookup.site_exists(site_id)? {
                return Err(PluginServiceError::invalid(
                    "plugin.scope_invalid",
                    "referenced site in sites[] does not exist",
                ));
            }
        }
    }
    for dl_key in ["downloader", "downloader_id"] {
        if let Some(dl_str) = values
            .get(dl_key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let dl_id = dl_str.parse::<domain::DownloaderId>().map_err(|_| {
                PluginServiceError::invalid("plugin.scope_invalid", "invalid downloader UUID")
            })?;
            if !lookup.downloader_exists(dl_id)? {
                return Err(PluginServiceError::invalid(
                    "plugin.scope_invalid",
                    "referenced downloader does not exist",
                ));
            }
        }
    }
    Ok(())
}

fn resolve_config_scope(
    app: &App<'_>,
    plugin: &store::plugins::PluginRecord,
    credential_bound: bool,
    site_id: Option<domain::SiteId>,
) -> Result<store::plugins::PluginConfigAuthority, PluginServiceError> {
    if !credential_bound {
        return Ok(app.issue_plugin_config_authority(plugin, None)?);
    }
    let sid = site_id.ok_or_else(|| {
        PluginServiceError::invalid(
            "plugin.scope_invalid",
            "credential-bound plugin config update requires site_id",
        )
    })?;
    let obs = app.observe_site(sid)?.ok_or_else(|| {
        PluginServiceError::invalid("plugin.scope_invalid", "referenced site_id does not exist")
    })?;
    Ok(app.issue_plugin_config_authority(plugin, Some(&obs))?)
}

fn apply_config_update_tx(
    app: &App<'_>,
    pid: &str,
    input: &UpdateConfigInput,
) -> Result<PluginConfigView, PluginServiceError> {
    let p = app
        .get_plugin(pid)?
        .ok_or_else(|| PluginServiceError::not_found("plugin.missing", "plugin not found"))?;
    if !matches!(
        p.lifecycle,
        PluginLifecycle::InstalledDisabled | PluginLifecycle::Disabled
    ) || p.desired_enabled
        || p.active_operation_id.is_some()
    {
        return Err(PluginServiceError::Store(PluginError::NotDrained.into()));
    }
    if p.revision != input.expected_revision {
        return Err(PluginServiceError::conflict(
            "plugin.revision_conflict",
            "plugin revision mismatch",
        ));
    }
    let ver = app
        .get_plugin_version(pid, &p.package_hash)?
        .ok_or_else(|| PluginServiceError::not_found("plugin.missing", "plugin version missing"))?;
    let raw_schema: serde_json::Value = serde_json::from_str(&ver.config_schema_json)
        .map_err(|e| PluginServiceError::Io(e.to_string()))?;
    let schema = validate_config_schema(&raw_schema).map_err(map_package_err)?;
    if input.schema_revision != schema.schema_revision() {
        return Err(PluginServiceError::config_invalid(
            "schema_revision mismatch",
            Vec::new(),
        ));
    }
    let specific_site = if ver.credential_bound {
        if let Some(sid) = input.site_id
            && app.observe_site(sid)?.is_none()
        {
            return Err(PluginServiceError::invalid(
                "plugin.scope_invalid",
                "referenced site_id does not exist",
            ));
        }
        input.site_id
    } else {
        None
    };
    let summary = app.get_plugin_config_summary(pid)?;
    let (public_values, secret_updates) = validate_secret_actions_for_scopes(
        app,
        &schema,
        &input.values,
        &input.secrets,
        &summary.secrets,
        ver.credential_bound,
        specific_site,
    )?;
    validate_config_object_grants(app, &public_values)?;
    let updated = if secret_updates.is_empty() {
        app.save_plugin_config(
            pid,
            input.expected_revision,
            schema.schema_revision(),
            &public_values,
        )?
    } else {
        let auth = resolve_config_scope(app, &p, ver.credential_bound, input.site_id)?;
        app.save_plugin_config_with_secrets(
            &auth,
            input.expected_revision,
            schema.schema_revision(),
            &public_values,
            &secret_updates,
        )?
    };
    let after_summary = app.get_plugin_config_summary(pid)?;
    let after_keys = live_configured_secret_keys_for_redaction(
        app,
        &after_summary.secrets,
        ver.credential_bound,
        specific_site,
    )?;
    let redacted = schema.redact_config(&public_values, &after_keys);
    Ok(PluginConfigView {
        plugin_id: pid.to_string(),
        revision: updated.revision,
        config_revision: updated.config_revision,
        schema_revision: schema.schema_revision().to_string(),
        values: redacted.values,
        secrets: redacted.secrets,
    })
}

pub(crate) fn app_write_service<T, F>(
    store: &store::Store,
    work: F,
) -> Result<T, PluginServiceError>
where
    F: FnOnce(&App<'_>) -> Result<T, PluginServiceError>,
{
    let mut service_err: Option<PluginServiceError> = None;
    let out = store.app_write(|app| match work(app) {
        Ok(val) => Ok(val),
        Err(err) => {
            service_err = Some(err);
            Err(StoreError::Plugin(PluginError::Conflict))
        }
    });
    match (out, service_err) {
        (Ok(val), _) => Ok(val),
        (Err(_), Some(err)) => Err(err),
        (Err(e), None) => Err(PluginServiceError::Store(e)),
    }
}

pub(crate) async fn run_app_write_tx<T, F>(
    state: &ApiState,
    work: F,
) -> Result<T, PluginServiceError>
where
    F: FnOnce(&App<'_>) -> Result<T, PluginServiceError> + Send + 'static,
    T: Send + 'static,
{
    run_app_write(state, move |store| app_write_service(store, work)).await
}

impl PluginService {
    pub async fn get_config_schema(
        &self,
        state: &ApiState,
        plugin_id: &str,
    ) -> Result<PluginConfigSchemaView, PluginServiceError> {
        let pid = plugin_id.to_string();
        run_app_read(state, move |store| {
            let p = store.get_plugin(&pid)?.ok_or_else(|| {
                PluginServiceError::not_found("plugin.missing", "plugin not found")
            })?;
            let ver = store
                .get_plugin_version(&pid, &p.package_hash)?
                .ok_or_else(|| {
                    PluginServiceError::not_found("plugin.missing", "plugin version missing")
                })?;
            let raw: serde_json::Value = serde_json::from_str(&ver.config_schema_json)
                .map_err(|e| PluginServiceError::Io(e.to_string()))?;
            let validated = validate_config_schema(&raw).map_err(map_package_err)?;
            Ok(PluginConfigSchemaView {
                plugin_id: pid,
                schema_revision: validated.schema_revision().to_string(),
                schema: raw,
            })
        })
        .await
    }

    pub async fn get_config(
        &self,
        state: &ApiState,
        plugin_id: &str,
    ) -> Result<PluginConfigView, PluginServiceError> {
        let pid = plugin_id.to_string();
        run_app_read(state, move |store| {
            let p = store.get_plugin(&pid)?.ok_or_else(|| {
                PluginServiceError::not_found("plugin.missing", "plugin not found")
            })?;
            let ver = store
                .get_plugin_version(&pid, &p.package_hash)?
                .ok_or_else(|| {
                    PluginServiceError::not_found("plugin.missing", "plugin version missing")
                })?;
            let raw_schema: serde_json::Value = serde_json::from_str(&ver.config_schema_json)
                .map_err(|e| PluginServiceError::Io(e.to_string()))?;
            let schema = validate_config_schema(&raw_schema).map_err(map_package_err)?;
            let summary = store.get_plugin_config_summary(&pid)?;
            let configured_keys = live_configured_secret_keys_for_redaction(
                store,
                &summary.secrets,
                ver.credential_bound,
                None,
            )?;
            let values = summary
                .config
                .as_ref()
                .map(|c| c.values.clone())
                .unwrap_or_else(|| serde_json::json!({}));
            let redacted = schema.redact_config(&values, &configured_keys);
            Ok(PluginConfigView {
                plugin_id: pid,
                revision: summary.revision,
                config_revision: summary.config_revision,
                schema_revision: schema.schema_revision().to_string(),
                values: redacted.values,
                secrets: redacted.secrets,
            })
        })
        .await
    }

    pub async fn validate_config(
        &self,
        state: &ApiState,
        plugin_id: &str,
        input: ValidateConfigInput,
    ) -> Result<ConfigValidationView, PluginServiceError> {
        let pid = plugin_id.to_string();
        run_app_read(state, move |store| {
            let p = store.get_plugin(&pid)?.ok_or_else(|| {
                PluginServiceError::not_found("plugin.missing", "plugin not found")
            })?;
            let ver = store
                .get_plugin_version(&pid, &p.package_hash)?
                .ok_or_else(|| {
                    PluginServiceError::not_found("plugin.missing", "plugin version missing")
                })?;
            let raw_schema: serde_json::Value = serde_json::from_str(&ver.config_schema_json)
                .map_err(|e| PluginServiceError::Io(e.to_string()))?;
            let schema = validate_config_schema(&raw_schema).map_err(map_package_err)?;
            if input.schema_revision != schema.schema_revision() {
                return Err(PluginServiceError::config_invalid(
                    "schema_revision mismatch",
                    Vec::new(),
                ));
            }
            let summary = store.get_plugin_config_summary(&pid)?;
            let (public_values, _) = validate_secret_actions_for_scopes(
                store,
                &schema,
                &input.values,
                &input.secrets,
                &summary.secrets,
                ver.credential_bound,
                None,
            )?;
            validate_config_object_grants(store, &public_values)?;
            Ok(ConfigValidationView {
                valid: true,
                schema_revision: schema.schema_revision().to_string(),
                values: public_values,
            })
        })
        .await
    }

    pub async fn update_config(
        &self,
        state: &ApiState,
        plugin_id: &str,
        input: UpdateConfigInput,
    ) -> Result<PluginConfigView, PluginServiceError> {
        validate_idempotency_input(&input.idempotency_key, &input.request)?;
        if let Some(replayed) = check_idempotent_replay::<PluginConfigView>(
            state,
            input.actor_user_id,
            "config-update",
            &input.idempotency_key,
            &input.request,
        )
        .await?
        {
            return Ok(replayed);
        }

        let pid = plugin_id.to_string();
        run_app_write_tx(state, move |app| {
            let subject = PluginSubject::User(input.actor_user_id);
            if let Some(val) = app.get_plugin_idempotency_envelope(
                &subject,
                "config-update",
                &input.idempotency_key,
                &input.request,
            )? {
                return serde_json::from_value(val)
                    .map_err(|e| PluginServiceError::Io(e.to_string()));
            }
            let view = apply_config_update_tx(app, &pid, &input)?;
            let json =
                serde_json::to_value(&view).map_err(|e| PluginServiceError::Io(e.to_string()))?;
            app.put_plugin_idempotency_envelope(
                &subject,
                "config-update",
                &input.idempotency_key,
                &input.request,
                &json,
            )?;
            Ok(view)
        })
        .await
    }
}

pub(super) fn validate_config_for_enable<L: PluginConfigLookup>(
    lookup: &L,
    plugin_id: &str,
    version: &store::plugins::PluginVersionMetadata,
    saved_config: Option<&store::plugins::PluginConfig>,
) -> Result<store::plugins::PluginConfig, (&'static str, String)> {
    let raw_schema: serde_json::Value =
        serde_json::from_str(&version.config_schema_json).map_err(|e| {
            (
                "config-invalid",
                format!("invalid stored config schema: {e}"),
            )
        })?;
    let schema =
        validate_config_schema(&raw_schema).map_err(|e| ("config-invalid", e.to_string()))?;
    let values = saved_config
        .map(|c| c.values.clone())
        .unwrap_or_else(|| serde_json::json!({}));
    let summary = lookup
        .get_plugin_config_summary(plugin_id)
        .map_err(|e| ("config-invalid", e.to_string()))?;
    let materialized = schema
        .validate_values(&values, saved_config.is_none())
        .map_err(|fields| {
            (
                "config-invalid",
                fields
                    .into_iter()
                    .map(|f| format!("{}: {}", f.path, f.message))
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        })?;
    let resolved = resolve_config_write_for_scopes(
        lookup,
        &schema,
        &materialized,
        &BTreeMap::new(),
        &summary.secrets,
        version.credential_bound,
        None,
    )
    .map_err(|e| ("config-invalid", e.to_string()))?
    .map_err(|fields| {
        (
            "config-invalid",
            fields
                .into_iter()
                .map(|f| format!("{}: {}", f.path, f.message))
                .collect::<Vec<_>>()
                .join("; "),
        )
    })?;
    validate_config_object_grants(lookup, &resolved.public_values)
        .map_err(|e| ("scope-invalid", e.to_string()))?;
    Ok(store::plugins::PluginConfig {
        schema_revision: saved_config.map_or_else(
            || schema.schema_revision().to_string(),
            |c| c.schema_revision.clone(),
        ),
        revision: saved_config.map_or(0, |c| c.revision),
        values: resolved.public_values,
    })
}
