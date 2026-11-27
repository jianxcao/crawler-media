use domain::SiteId;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PluginError {
    #[error("plugin record not found")]
    NotFound,
    #[error("plugin resource revision conflict")]
    Conflict,
    #[error("package identity conflict")]
    PackageConflict,
    #[error("plugin is not disabled and drained")]
    NotDrained,
    #[error("stale plugin authority")]
    StaleAuthority,
    #[error("plugin capability denied")]
    Denied,
    #[error("invalid plugin input: {0}")]
    Invalid(&'static str),
    #[error("plugin quota exceeded")]
    QuotaExceeded,
    #[error("unknown command effect requires reconciliation")]
    UnknownEffect,
    #[error("confirmed command effect requires fenced host disposition")]
    EffectDispositionRequired,
    #[error("plugin command cause loop or depth limit exceeded")]
    CauseLoop,
    #[error("idempotency key reused for a different plugin request")]
    IdempotencyConflict,
    #[error("plugin already has an active operation: {0}")]
    OperationActive(uuid::Uuid),
    #[error("plugin state schema mismatch across package switch")]
    StateSchemaMismatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginPage<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

/// Already validated by the host package service; registration does no verification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginVersionMetadata {
    pub plugin_id: String,
    pub package_hash: String,
    pub version: String,
    pub manifest_json: String,
    pub config_schema_json: String,
    pub state_schema: String,
    pub artifact_path: String,
    pub signing_key_id: Option<String>,
    pub signature_status: String,
    pub ui_metadata_ref: Option<String>,
    pub credential_bound: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginLifecycle {
    InstalledDisabled,
    Enabling,
    Enabled,
    Draining,
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginRecord {
    pub plugin_id: String,
    pub package_hash: String,
    pub lifecycle: PluginLifecycle,
    pub desired_enabled: bool,
    pub revision: i64,
    pub config_revision: i64,
    pub generation: i64,
    pub active_operation_id: Option<uuid::Uuid>,
    pub failure_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PluginConfig {
    pub schema_revision: String,
    pub revision: i64,
    pub values: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PluginAuthoritySnapshot {
    pub plugin: PluginRecord,
    pub version: PluginVersionMetadata,
    pub config: Option<PluginConfig>,
    pub grants: Vec<PluginGrant>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PluginActivationSnapshot {
    pub plugin: PluginRecord,
    pub version: PluginVersionMetadata,
    pub config: Option<PluginConfig>,
    pub grants: Vec<PluginGrant>,
    pub developer_mode_enabled: bool,
    pub signing_key_active: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginGrantScope {
    Global,
    Site(SiteId),
    Object { kind: String, id: String },
}
impl PluginGrantScope {
    pub fn target_key(&self) -> String {
        match self {
            Self::Global => "global".into(),
            Self::Site(id) => format!("site:{id}"),
            Self::Object { kind, id } => format!("{kind}:{id}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginGrant {
    pub capability: String,
    pub scope: PluginGrantScope,
    pub sensitive: bool,
}

/// Explicit host evidence, captured after actual worker/effect reconciliation.
/// A process restart or an empty in-memory worker counter is not this evidence.
#[derive(Clone, Debug)]
pub struct PluginLifecycleObservation {
    pub plugin_id: String,
    pub operation_id: uuid::Uuid,
    pub revision: i64,
    pub generation: i64,
    pub workers_exited: bool,
    pub unknown_effects_reconciled: bool,
    pub validation_succeeded: bool,
}
