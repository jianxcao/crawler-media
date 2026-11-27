use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use crate::RegistryError;
use crate::native_providers::provider_from_profile;
use crate::profile::{Profile, load_builtin_profiles, load_overlay_profiles, parse_yaml_profile};
use crate::provider::{
    IndexerProvider, MaintenanceDescriptor, MaintenanceProvider, ProviderDescriptor, ProviderSource,
};

const RESERVED_PLUGIN_IDS: &[&str] = &["installed", "repos", "developer", "new"];

fn validate_plugin_id_part(plugin_id: &str, full_id: &str) -> Result<(), RegistryError> {
    if !(3..=128).contains(&plugin_id.len()) {
        return Err(RegistryError::InvalidNamespace {
            id: full_id.into(),
            reason: "plugin id length must be 3..=128".into(),
        });
    }
    if RESERVED_PLUGIN_IDS.contains(&plugin_id) {
        return Err(RegistryError::InvalidNamespace {
            id: full_id.into(),
            reason: format!("plugin id `{plugin_id}` is reserved"),
        });
    }
    let segments: Vec<&str> = plugin_id.split('.').collect();
    if segments.len() < 2 {
        return Err(RegistryError::InvalidNamespace {
            id: full_id.into(),
            reason: "plugin id must contain at least 2 dot-separated segments".into(),
        });
    }
    for seg in segments {
        if seg.is_empty()
            || seg.starts_with('-')
            || seg.ends_with('-')
            || !seg
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(RegistryError::InvalidNamespace {
                id: full_id.into(),
                reason: format!("invalid plugin id segment `{seg}`"),
            });
        }
    }
    Ok(())
}

fn validate_provider_slug(slug: &str, full_id: &str) -> Result<(), RegistryError> {
    if slug.is_empty()
        || slug.len() > 64
        || !slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(RegistryError::InvalidNamespace {
            id: full_id.into(),
            reason: format!("invalid provider slug `{slug}`; expected 1..=64 [a-z0-9-]"),
        });
    }
    Ok(())
}

pub fn parse_plugin_provider_id(id: &str) -> Result<(&str, &str), RegistryError> {
    let Some(rest) = id.strip_prefix("plugin:") else {
        return Err(RegistryError::InvalidNamespace {
            id: id.into(),
            reason: "plugin provider id must start with `plugin:`".into(),
        });
    };
    let Some((plugin_id, provider_id)) = rest.split_once('/') else {
        return Err(RegistryError::InvalidNamespace {
            id: id.into(),
            reason: "plugin provider id must use `plugin:<plugin-id>/<provider-id>`".into(),
        });
    };
    if provider_id.contains('/') {
        return Err(RegistryError::InvalidNamespace {
            id: id.into(),
            reason: "plugin provider id must contain only one `/`".into(),
        });
    }
    validate_plugin_id_part(plugin_id, id)?;
    validate_provider_slug(provider_id, id)?;
    Ok((plugin_id, provider_id))
}

fn validate_non_plugin_id(id: &str) -> Result<(), RegistryError> {
    if id.is_empty() || id.starts_with("plugin:") || id.contains('/') {
        return Err(RegistryError::InvalidNamespace {
            id: id.into(),
            reason: "non-plugin id must not be empty, contain `/`, or start with `plugin:`".into(),
        });
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(RegistryError::InvalidNamespace {
            id: id.into(),
            reason: "id contains unsupported characters".into(),
        });
    }
    Ok(())
}

fn validate_descriptor_namespace(id: &str, source: &ProviderSource) -> Result<(), RegistryError> {
    if let Some(source_plugin_id) = source.plugin_id() {
        let (parsed_plugin_id, _slug) = parse_plugin_provider_id(id)?;
        if parsed_plugin_id != source_plugin_id {
            return Err(RegistryError::InvalidNamespace {
                id: id.into(),
                reason: format!(
                    "provider plugin_id `{parsed_plugin_id}` does not match source plugin_id `{source_plugin_id}`"
                ),
            });
        }
    } else {
        validate_non_plugin_id(id)?;
    }
    Ok(())
}

pub struct IndexerEntry {
    provider: Arc<dyn IndexerProvider>,
}

impl IndexerEntry {
    pub fn new(provider: Arc<dyn IndexerProvider>) -> Self {
        Self { provider }
    }

    pub fn descriptor(&self) -> &ProviderDescriptor {
        self.provider.descriptor()
    }

    pub fn provider(&self) -> &dyn IndexerProvider {
        self.provider.as_ref()
    }

    pub fn profile(&self) -> Option<&Arc<Profile>> {
        self.provider.profile()
    }
}

pub struct MaintenanceEntry {
    provider: Arc<dyn MaintenanceProvider>,
}

impl MaintenanceEntry {
    pub fn new(provider: Arc<dyn MaintenanceProvider>) -> Self {
        Self { provider }
    }

    pub fn descriptor(&self) -> &MaintenanceDescriptor {
        self.provider.descriptor()
    }

    pub fn provider(&self) -> &dyn MaintenanceProvider {
        self.provider.as_ref()
    }

    pub fn provider_arc(&self) -> Arc<dyn MaintenanceProvider> {
        Arc::clone(&self.provider)
    }
}

pub struct RegistrySnapshot {
    generation: u64,
    indexers: BTreeMap<String, Arc<IndexerEntry>>,
    maintenance: BTreeMap<String, Arc<MaintenanceEntry>>,
}

impl RegistrySnapshot {
    pub fn builtin() -> Result<Arc<Self>, RegistryError> {
        RegistryCandidate::from_builtin()?.build_with_generation(1)
    }

    pub fn load_with_overlay(overlay: Option<&Path>) -> Result<Arc<Self>, RegistryError> {
        RegistryCandidate::from_builtin()?
            .with_overlay_dir(overlay)?
            .build_with_generation(1)
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn indexer(&self, id: &str) -> Option<&Arc<IndexerEntry>> {
        self.indexers.get(id)
    }

    pub fn profile(&self, id: &str) -> Option<Arc<Profile>> {
        self.indexers.get(id).and_then(|e| e.profile().cloned())
    }

    pub fn profile_ids(&self) -> Vec<String> {
        self.indexers.keys().cloned().collect()
    }

    pub fn profiles(&self) -> impl Iterator<Item = Arc<Profile>> + '_ {
        self.indexers.values().filter_map(|e| e.profile().cloned())
    }

    pub fn indexer_descriptors(&self) -> impl Iterator<Item = &ProviderDescriptor> + '_ {
        self.indexers.values().map(|e| e.descriptor())
    }

    pub fn maintenance(&self, id: &str) -> Option<&Arc<MaintenanceEntry>> {
        self.maintenance.get(id)
    }

    pub fn maintenance_descriptors(&self) -> impl Iterator<Item = &MaintenanceDescriptor> + '_ {
        self.maintenance.values().map(|e| e.descriptor())
    }

    pub fn maintenance_entries(&self) -> impl Iterator<Item = &Arc<MaintenanceEntry>> + '_ {
        self.maintenance.values()
    }
}

#[derive(Default)]
pub struct RegistryCandidate {
    indexers: BTreeMap<String, Arc<IndexerEntry>>,
    maintenance: BTreeMap<String, Arc<MaintenanceEntry>>,
    overlay_seen: BTreeSet<String>,
}

impl RegistryCandidate {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_builtin() -> Result<Self, RegistryError> {
        let mut candidate = Self::empty();
        for profile in load_builtin_profiles()? {
            validate_non_plugin_id(&profile.id)?;
            if let Some(existing) = candidate.indexers.get(&profile.id) {
                return Err(RegistryError::Collision {
                    id: profile.id.clone(),
                    existing_source: existing.descriptor().source.as_label().into(),
                    new_source: "builtin".into(),
                });
            }
            let id = profile.id.clone();
            let provider = provider_from_profile(Arc::new(profile), ProviderSource::Builtin)?;
            candidate
                .indexers
                .insert(id, Arc::new(IndexerEntry::new(provider)));
        }
        Ok(candidate)
    }

    pub fn with_overlay_dir(mut self, dir: Option<&Path>) -> Result<Self, RegistryError> {
        let Some(dir) = dir else {
            return Ok(self);
        };
        for (path, profile) in load_overlay_profiles(dir)? {
            validate_non_plugin_id(&profile.id)?;
            if !self.overlay_seen.insert(profile.id.clone()) {
                return Err(RegistryError::Collision {
                    id: profile.id.clone(),
                    existing_source: "overlay".into(),
                    new_source: "overlay".into(),
                });
            }
            if let Some(existing) = self.indexers.get(&profile.id)
                && !existing.descriptor().source.is_builtin()
            {
                return Err(RegistryError::Collision {
                    id: profile.id.clone(),
                    existing_source: existing.descriptor().source.as_label().into(),
                    new_source: "overlay".into(),
                });
            }
            let id = profile.id.clone();
            let provider =
                provider_from_profile(Arc::new(profile), ProviderSource::Overlay { path })?;
            self.indexers
                .insert(id, Arc::new(IndexerEntry::new(provider)));
        }
        Ok(self)
    }

    pub fn add_plugin_yaml(
        mut self,
        plugin_id: &str,
        version: &str,
        plugin_generation: u64,
        yaml: &str,
    ) -> Result<Self, RegistryError> {
        let mut profile = parse_yaml_profile(yaml)?;
        validate_provider_slug(&profile.id, &profile.id)?;
        let canonical_id = format!("plugin:{plugin_id}/{}", profile.id);
        parse_plugin_provider_id(&canonical_id)?;
        if let Some(existing) = self.indexers.get(&canonical_id) {
            return Err(RegistryError::Collision {
                id: canonical_id,
                existing_source: existing.descriptor().source.as_label().into(),
                new_source: "plugin-yaml".into(),
            });
        }
        profile.id = canonical_id.clone();
        let source = ProviderSource::PluginYaml {
            plugin_id: plugin_id.to_string(),
            version: version.to_string(),
            plugin_generation,
        };
        let provider = provider_from_profile(Arc::new(profile), source)?;
        self.indexers
            .insert(canonical_id, Arc::new(IndexerEntry::new(provider)));
        Ok(self)
    }

    pub fn add_indexer_provider(
        mut self,
        provider: Arc<dyn IndexerProvider>,
    ) -> Result<Self, RegistryError> {
        let desc = provider.descriptor();
        validate_descriptor_namespace(&desc.id, &desc.source)?;
        if desc.capabilities.is_empty() || desc.auth_types.is_empty() {
            return Err(RegistryError::InvalidDescriptor {
                id: desc.id.clone(),
                reason: "indexer provider descriptor must declare capabilities and auth_types"
                    .into(),
            });
        }
        if let Some(existing) = self.indexers.get(&desc.id) {
            return Err(RegistryError::Collision {
                id: desc.id.clone(),
                existing_source: existing.descriptor().source.as_label().into(),
                new_source: desc.source.as_label().into(),
            });
        }
        self.indexers
            .insert(desc.id.clone(), Arc::new(IndexerEntry::new(provider)));
        Ok(self)
    }

    pub fn add_maintenance_provider(
        mut self,
        provider: Arc<dyn MaintenanceProvider>,
    ) -> Result<Self, RegistryError> {
        let desc = provider.descriptor();
        validate_descriptor_namespace(&desc.id, &desc.source)?;
        if desc.actions.is_empty() {
            return Err(RegistryError::InvalidDescriptor {
                id: desc.id.clone(),
                reason: "maintenance provider descriptor must declare at least one action".into(),
            });
        }
        if let Some(existing) = self.maintenance.get(&desc.id) {
            return Err(RegistryError::Collision {
                id: desc.id.clone(),
                existing_source: existing.descriptor().source.as_label().into(),
                new_source: desc.source.as_label().into(),
            });
        }
        self.maintenance
            .insert(desc.id.clone(), Arc::new(MaintenanceEntry::new(provider)));
        Ok(self)
    }

    pub fn build(self) -> Result<Arc<RegistrySnapshot>, RegistryError> {
        self.build_with_generation(1)
    }

    pub fn build_with_generation(
        self,
        generation: u64,
    ) -> Result<Arc<RegistrySnapshot>, RegistryError> {
        Ok(Arc::new(RegistrySnapshot {
            generation,
            indexers: self.indexers,
            maintenance: self.maintenance,
        }))
    }
}
