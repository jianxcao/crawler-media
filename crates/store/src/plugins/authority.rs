use super::*;
use crate::{App, SiteObservation, Store, StoreError};
use rusqlite::params;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Scope {
    Global,
    Site { id: domain::SiteId, epoch: String },
}

/// Opaque host-issued credential domain. It cannot be decoded from guest input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginSecurityScope(Scope);

#[derive(Clone, Debug)]
pub(super) struct Binding {
    pub plugin_id: String,
    pub hash: String,
    pub generation: i64,
    pub revision: i64,
    pub scope: PluginSecurityScope,
}

/// Enabled invocation authority, distinct from disabled administrative config access.
#[derive(Clone, Debug)]
pub struct PluginStateAuthority(pub(super) Binding);
#[derive(Clone, Debug)]
pub struct PluginConfigAuthority(pub(super) Binding);

impl PluginStateAuthority {
    pub fn security_scope(&self) -> &PluginSecurityScope {
        &self.0.scope
    }
    pub fn plugin_id(&self) -> &str {
        &self.0.plugin_id
    }
    pub fn generation(&self) -> i64 {
        self.0.generation
    }
    pub fn revision(&self) -> i64 {
        self.0.revision
    }
    pub fn package_hash(&self) -> &str {
        &self.0.hash
    }
    pub fn site_id(&self) -> Option<domain::SiteId> {
        self.0.scope.site_id()
    }
}
impl PluginConfigAuthority {
    pub fn security_scope(&self) -> &PluginSecurityScope {
        &self.0.scope
    }
}
impl PluginSecurityScope {
    pub fn global() -> Self {
        Self(Scope::Global)
    }
    pub fn for_site(site: &SiteObservation) -> Self {
        Self(Scope::Site {
            id: site.site().id,
            epoch: site.credential_epoch().storage_key().into(),
        })
    }
    pub fn site_id(&self) -> Option<domain::SiteId> {
        match &self.0 {
            Scope::Global => None,
            Scope::Site { id, .. } => Some(*id),
        }
    }
    pub(super) fn key(&self) -> Result<String, StoreError> {
        Ok(serde_json::to_string(&self.0)?)
    }
    pub(super) fn site_scope_key(id: domain::SiteId, epoch: &str) -> Result<String, StoreError> {
        Ok(serde_json::to_string(&Scope::Site {
            id,
            epoch: epoch.to_string(),
        })?)
    }
    pub(super) fn from_key(key: &str) -> Result<Self, StoreError> {
        Ok(Self(serde_json::from_str(key)?))
    }
    pub(super) fn grant_scope(&self) -> PluginGrantScope {
        match &self.0 {
            Scope::Global => PluginGrantScope::Global,
            Scope::Site { id, .. } => PluginGrantScope::Site(*id),
        }
    }
    pub(super) fn validate(&self, app: &App<'_>, active: bool) -> Result<(), StoreError> {
        if let Scope::Site { id, epoch } = &self.0 {
            let valid: bool = app.app.query_row("SELECT EXISTS(SELECT 1 FROM sites WHERE id=?1 AND credential_epoch=?2 AND (?3=0 OR enabled=1))",params![id.to_string(),epoch,active],|r|r.get(0))?;
            if !valid {
                return Err(PluginError::StaleAuthority.into());
            }
        }
        Ok(())
    }
}

pub(super) fn validate_runtime(
    app: &App<'_>,
    binding: &Binding,
    capability: &str,
) -> Result<(), StoreError> {
    validate_runtime_binding(app, binding)?;
    require_grant(app, binding, capability, &binding.scope.grant_scope())
}

pub(super) fn validate_runtime_binding(app: &App<'_>, binding: &Binding) -> Result<(), StoreError> {
    let p = app
        .get_plugin(&binding.plugin_id)?
        .ok_or(PluginError::StaleAuthority)?;
    if p.package_hash != binding.hash || p.generation != binding.generation {
        return Err(PluginError::StaleAuthority.into());
    }
    if p.lifecycle != PluginLifecycle::Enabled || !p.desired_enabled {
        return Err(PluginError::StaleAuthority.into());
    }
    binding.scope.validate(app, true)
}

fn strip_optional_kind_prefix<'a>(raw: &'a str, kind: &str) -> &'a str {
    let trimmed = raw.trim();
    if trimmed.len() > kind.len()
        && trimmed
            .get(..kind.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(kind))
        && trimmed.as_bytes().get(kind.len()) == Some(&b':')
    {
        trimmed[kind.len() + 1..].trim()
    } else {
        trimmed
    }
}

fn parse_site_id_opt(raw: &str) -> Option<domain::SiteId> {
    strip_optional_kind_prefix(raw, "site").parse().ok()
}

fn scoped_ids_match(kind: &str, granted_id: &str, target_id: &str) -> bool {
    let g = strip_optional_kind_prefix(granted_id, kind);
    let t = strip_optional_kind_prefix(target_id, kind);
    match (uuid::Uuid::parse_str(g), uuid::Uuid::parse_str(t)) {
        (Ok(gu), Ok(tu)) => gu == tu,
        _ => !g.is_empty() && g.eq_ignore_ascii_case(t),
    }
}

pub(super) fn canonical_grant_scope_key(scope: &PluginGrantScope) -> Option<String> {
    if let PluginGrantScope::Object { kind, .. } = scope
        && (kind != kind.trim()
            || kind
                .chars()
                .any(|c| c.is_whitespace() || c.is_ascii_uppercase()))
    {
        return None;
    }
    semantic_grant_scope_key(scope)
}

pub(super) fn semantic_grant_scope_key(scope: &PluginGrantScope) -> Option<String> {
    match scope {
        PluginGrantScope::Global => Some("global".into()),
        PluginGrantScope::Site(sid) => Some(format!("site:{sid}")),
        PluginGrantScope::Object { kind, id } => {
            let k = kind.trim().to_ascii_lowercase();
            let i = id.trim();
            if k.is_empty() || i.is_empty() || k.contains('*') || i.contains('*') {
                return None;
            }
            if k == "site" {
                let stripped = strip_optional_kind_prefix(i, "site");
                if stripped.is_empty() {
                    return None;
                }
                return Some(match parse_site_id_opt(i) {
                    Some(sid) => format!("site:{sid}"),
                    None => format!("obj:site:{}", stripped.to_ascii_lowercase()),
                });
            }
            if matches!(k.as_str(), "downloader" | "library" | "subscribe") {
                let stripped = strip_optional_kind_prefix(i, &k);
                if stripped.is_empty() {
                    return None;
                }
                let norm = uuid::Uuid::parse_str(stripped)
                    .map_or_else(|_| stripped.to_ascii_lowercase(), |u| u.to_string());
                return Some(format!("obj:{k}:{norm}"));
            }
            Some(format!("obj:{k}:{i}"))
        }
    }
}

pub(super) fn grant_scopes_equivalent(a: &PluginGrantScope, b: &PluginGrantScope) -> bool {
    matches!(
        (semantic_grant_scope_key(a), semantic_grant_scope_key(b)),
        (Some(ka), Some(kb)) if ka == kb
    )
}

fn grant_scope_covers_target(granted: &PluginGrantScope, target: &PluginGrantScope) -> bool {
    if matches!(granted, PluginGrantScope::Global) {
        return true;
    }
    match target {
        PluginGrantScope::Site(t_sid) => {
            return match granted {
                PluginGrantScope::Site(g_sid) => g_sid == t_sid,
                PluginGrantScope::Object { kind, id }
                    if kind.trim().eq_ignore_ascii_case("site") =>
                {
                    parse_site_id_opt(id) == Some(*t_sid)
                }
                _ => false,
            };
        }
        PluginGrantScope::Object { kind, id } if kind.trim().eq_ignore_ascii_case("site") => {
            let Some(t_sid) = parse_site_id_opt(id) else {
                return false;
            };
            return match granted {
                PluginGrantScope::Site(g_sid) => *g_sid == t_sid,
                PluginGrantScope::Object {
                    kind: g_kind,
                    id: g_id,
                } if g_kind.trim().eq_ignore_ascii_case("site") => {
                    parse_site_id_opt(g_id) == Some(t_sid)
                }
                _ => false,
            };
        }
        _ => {}
    }
    match (granted, target) {
        (
            PluginGrantScope::Object {
                kind: g_kind,
                id: g_id,
            },
            PluginGrantScope::Object {
                kind: t_kind,
                id: t_id,
            },
        ) if !g_kind.trim().eq_ignore_ascii_case("site")
            && g_kind.trim().eq_ignore_ascii_case(t_kind.trim()) =>
        {
            let k_lower = t_kind.trim().to_ascii_lowercase();
            match k_lower.as_str() {
                "downloader" | "library" | "subscribe" => scoped_ids_match(&k_lower, g_id, t_id),
                _ => !t_id.trim().is_empty() && g_id.trim() == t_id.trim(),
            }
        }
        _ => false,
    }
}

pub(super) fn require_grant(
    app: &App<'_>,
    binding: &Binding,
    capability: &str,
    target: &PluginGrantScope,
) -> Result<(), StoreError> {
    let mut stmt = app.app.prepare(
        "SELECT object_scope FROM plugin_grants WHERE plugin_id=?1 AND package_hash=?2 AND capability=?3 AND status='granted'",
    )?;
    let rows = stmt.query_map(params![binding.plugin_id, binding.hash, capability], |r| {
        r.get::<_, String>(0)
    })?;
    for row in rows {
        let raw = row?;
        let granted: PluginGrantScope = serde_json::from_str(&raw)?;
        if grant_scope_covers_target(&granted, target) {
            return Ok(());
        }
    }
    Err(PluginError::Denied.into())
}

pub(super) fn validate_config(app: &App<'_>, binding: &Binding) -> Result<(), StoreError> {
    let p = app
        .get_plugin(&binding.plugin_id)?
        .ok_or(PluginError::StaleAuthority)?;
    if p.package_hash != binding.hash
        || p.generation != binding.generation
        || p.revision != binding.revision
    {
        return Err(PluginError::StaleAuthority.into());
    }
    super::config::drained(&p)?;
    binding.scope.validate(app, false)
}

fn issue(
    app: &App<'_>,
    expected: &PluginRecord,
    site: Option<&SiteObservation>,
    active: bool,
) -> Result<Binding, StoreError> {
    let p = app
        .get_plugin(&expected.plugin_id)?
        .ok_or(PluginError::NotFound)?;
    if p.package_hash != expected.package_hash
        || p.generation != expected.generation
        || p.revision != expected.revision
    {
        return Err(PluginError::StaleAuthority.into());
    }
    if active {
        if p.lifecycle != PluginLifecycle::Enabled || !p.desired_enabled {
            return Err(PluginError::StaleAuthority.into());
        }
    } else {
        super::config::drained(&p)?;
    }
    let bound: bool = app.app.query_row(
        "SELECT credential_bound FROM plugin_versions WHERE plugin_id=?1 AND package_hash=?2",
        params![p.plugin_id, p.package_hash],
        |r| r.get(0),
    )?;
    let scope = match (bound, site) {
        (false, None) => PluginSecurityScope(Scope::Global),
        (true, Some(site)) => PluginSecurityScope(Scope::Site {
            id: site.site().id,
            epoch: site.credential_epoch().storage_key().into(),
        }),
        _ => return Err(PluginError::Denied.into()),
    };
    scope.validate(app, active)?;
    Ok(Binding {
        plugin_id: p.plugin_id,
        hash: p.package_hash,
        generation: p.generation,
        revision: p.revision,
        scope,
    })
}

impl App<'_> {
    pub fn issue_plugin_state_authority(
        &self,
        plugin: &PluginRecord,
        site: Option<&SiteObservation>,
    ) -> Result<PluginStateAuthority, StoreError> {
        let tx = crate::read_tx(self.app)?;
        let binding = issue(self, plugin, site, true)?;
        tx.commit()?;
        Ok(PluginStateAuthority(binding))
    }
    pub fn issue_plugin_config_authority(
        &self,
        plugin: &PluginRecord,
        site: Option<&SiteObservation>,
    ) -> Result<PluginConfigAuthority, StoreError> {
        let tx = crate::read_tx(self.app)?;
        let binding = issue(self, plugin, site, false)?;
        tx.commit()?;
        Ok(PluginConfigAuthority(binding))
    }
    pub fn validate_plugin_state_authority(
        &self,
        authority: &PluginStateAuthority,
    ) -> Result<(), StoreError> {
        let tx = crate::read_tx(self.app)?;
        validate_runtime_binding(self, &authority.0)?;
        tx.commit()?;
        Ok(())
    }
}
impl Store {
    pub fn issue_plugin_state_authority(
        &self,
        plugin: &PluginRecord,
        site: Option<&SiteObservation>,
    ) -> Result<PluginStateAuthority, StoreError> {
        self.with_app_read(|app| app.issue_plugin_state_authority(plugin, site))
    }
    pub fn issue_plugin_config_authority(
        &self,
        plugin: &PluginRecord,
        site: Option<&SiteObservation>,
    ) -> Result<PluginConfigAuthority, StoreError> {
        self.with_app_read(|app| app.issue_plugin_config_authority(plugin, site))
    }
    pub fn validate_plugin_state_authority(
        &self,
        authority: &PluginStateAuthority,
    ) -> Result<(), StoreError> {
        self.with_app_read(|app| app.validate_plugin_state_authority(authority))
    }
}
