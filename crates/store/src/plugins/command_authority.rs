use super::authority::Binding;
use super::*;
use crate::{App, Store, StoreError};

/// Resolved by the authenticated host service from typed handles, never guest scope JSON.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginCommandTarget {
    Global,
    Site(domain::SiteId),
    Library(domain::LibraryId),
    Downloader(domain::DownloaderId),
    Subscribe(domain::SubscribeId),
}
impl PluginCommandTarget {
    pub fn target_key(&self) -> String {
        self.grant_scope().target_key()
    }
    fn grant_scope(&self) -> PluginGrantScope {
        match self {
            Self::Global => PluginGrantScope::Global,
            Self::Site(id) => PluginGrantScope::Site(*id),
            Self::Library(id) => PluginGrantScope::Object {
                kind: "library".into(),
                id: id.to_string(),
            },
            Self::Downloader(id) => PluginGrantScope::Object {
                kind: "downloader".into(),
                id: id.to_string(),
            },
            Self::Subscribe(id) => PluginGrantScope::Object {
                kind: "subscribe".into(),
                id: id.to_string(),
            },
        }
    }
}

/// Host-issued exact target authorization plus the independent opaque data domain.
/// No Deserialize implementation; do not map this constructor to a WIT import.
#[derive(Clone, Debug)]
pub struct PluginCommandAuthority {
    pub(super) binding: Binding,
    pub(super) kind: String,
    pub(super) targets: Vec<PluginGrantScope>,
}
impl PluginCommandAuthority {
    pub fn security_scope(&self) -> &PluginSecurityScope {
        &self.binding.scope
    }
}

pub(super) fn validate_targets(
    app: &App<'_>,
    binding: &Binding,
    kind: &str,
    targets: &[PluginGrantScope],
) -> Result<(), StoreError> {
    super::authority::validate_runtime_binding(app, binding)?;
    if targets.is_empty() || targets.len() > 200 {
        return Err(PluginError::Invalid("command requires 1..200 exact targets").into());
    }
    let capability = format!("command:{kind}");
    for target in targets {
        super::authority::require_grant(app, binding, &capability, target)?;
    }
    Ok(())
}

impl App<'_> {
    pub fn authorize_plugin_command(
        &self,
        state: &PluginStateAuthority,
        kind: &str,
        targets: &[PluginCommandTarget],
    ) -> Result<PluginCommandAuthority, StoreError> {
        if kind.is_empty() || kind.len() > 128 || targets.is_empty() || targets.len() > 200 {
            return Err(PluginError::Invalid("invalid host command targets").into());
        }
        let mut sorted = targets
            .iter()
            .map(|target| {
                let scope = target.grant_scope();
                Ok((serde_json::to_string(&scope)?, scope))
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        sorted.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        if sorted.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(PluginError::Invalid("duplicate command target").into());
        }
        let targets: Vec<_> = sorted.into_iter().map(|(_, scope)| scope).collect();
        let tx = crate::read_tx(self.app)?;
        validate_targets(self, &state.0, kind, &targets)?;
        tx.commit()?;
        Ok(PluginCommandAuthority {
            binding: state.0.clone(),
            kind: kind.into(),
            targets,
        })
    }
}
impl Store {
    pub fn authorize_plugin_command(
        &self,
        state: &PluginStateAuthority,
        kind: &str,
        targets: &[PluginCommandTarget],
    ) -> Result<PluginCommandAuthority, StoreError> {
        self.with_app_read(|app| app.authorize_plugin_command(state, kind, targets))
    }
}
