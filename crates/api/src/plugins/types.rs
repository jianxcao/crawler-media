use std::collections::BTreeMap;
use std::fmt;

use domain::UserId;
use plugin_package::{SchemaFieldError, SecretConfiguredStatus, SecretWriteAction};
use serde::{Deserialize, Serialize};
use store::plugins::{
    CanonicalPluginRequest, PluginGrant, PluginGrantScope, PluginGrantStatus, PluginLifecycle,
    PluginOperationState, PluginSecurityScope, PluginTrustKeyStatus,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

pub use super::errors::PluginServiceError;
pub(crate) use super::errors::map_package_err;

pub(crate) fn format_rfc3339(ts: i64) -> String {
    OffsetDateTime::from_unix_timestamp(ts)
        .ok()
        .and_then(|dt| dt.format(&Rfc3339).ok())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReauthPurpose {
    #[serde(rename = "trust-key:create")]
    TrustKeyCreate,
    #[serde(rename = "trust-key:revoke")]
    TrustKeyRevoke,
    #[serde(rename = "developer-mode:enable")]
    DeveloperModeEnable,
    #[serde(rename = "plugin:upgrade")]
    PluginUpgrade,
    #[serde(rename = "plugin:rollback")]
    PluginRollback,
    #[serde(rename = "plugin:state-reset")]
    PluginStateReset,
    #[serde(rename = "plugin:upload")]
    PluginUpload,
    #[serde(rename = "plugin-ui-trust")]
    PluginUiTrust,
}

impl ReauthPurpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TrustKeyCreate => "trust-key:create",
            Self::TrustKeyRevoke => "trust-key:revoke",
            Self::DeveloperModeEnable => "developer-mode:enable",
            Self::PluginUpgrade => "plugin:upgrade",
            Self::PluginRollback => "plugin:rollback",
            Self::PluginStateReset => "plugin:state-reset",
            Self::PluginUpload => "plugin:upload",
            Self::PluginUiTrust => "plugin-ui-trust",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "trust-key:create" => Some(Self::TrustKeyCreate),
            "trust-key:revoke" => Some(Self::TrustKeyRevoke),
            "developer-mode:enable" => Some(Self::DeveloperModeEnable),
            "plugin:upgrade" => Some(Self::PluginUpgrade),
            "plugin:rollback" => Some(Self::PluginRollback),
            "plugin:state-reset" => Some(Self::PluginStateReset),
            "plugin:upload" => Some(Self::PluginUpload),
            "plugin-ui-trust" => Some(Self::PluginUiTrust),
            _ => None,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ReauthTokenView {
    pub reauth_token: String,
    pub expires_at: String,
    pub purpose: ReauthPurpose,
}

impl fmt::Debug for ReauthTokenView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReauthTokenView")
            .field("reauth_token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .field("purpose", &self.purpose)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PagedView<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

#[derive(Clone)]
pub struct StageUploadInput {
    pub actor_user_id: UserId,
    pub upload_id: Uuid,
    pub cmplugin_bytes: Vec<u8>,
    pub signature_text: Option<String>,
    pub expected_sha256: Option<String>,
    pub reauth_token: String,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
}

impl fmt::Debug for StageUploadInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StageUploadInput")
            .field("actor_user_id", &self.actor_user_id)
            .field("upload_id", &self.upload_id)
            .field("cmplugin_bytes_len", &self.cmplugin_bytes.len())
            .field("has_signature", &self.signature_text.is_some())
            .field("expected_sha256", &self.expected_sha256)
            .field("reauth_token", &"[REDACTED]")
            .field("idempotency_key", &self.idempotency_key)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedUploadView {
    pub upload_id: Uuid,
    pub hash: String,
    pub signature_status: String,
    pub signing_key_id: Option<String>,
    pub fingerprint: Option<String>,
    pub expires_at: String,
}

#[derive(Clone, Debug)]
pub struct PrepareTrustKeyInput {
    pub public_key: String,
    pub label: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct TrustKeyPreparationView {
    pub preparation_id: Uuid,
    pub key_id: String,
    pub label: String,
    pub public_key: String,
    pub fingerprint: String,
    pub consent_token: String,
    pub expires_at: String,
}

impl fmt::Debug for TrustKeyPreparationView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TrustKeyPreparationView")
            .field("preparation_id", &self.preparation_id)
            .field("key_id", &self.key_id)
            .field("label", &self.label)
            .field("fingerprint", &self.fingerprint)
            .field("consent_token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Clone)]
pub struct CreateTrustKeyInput {
    pub label: Option<String>,
    pub public_key: String,
    pub fingerprint: String,
    pub consent_token: String,
    pub reauth_token: String,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
}

#[derive(Clone)]
pub struct RevokeTrustKeyInput {
    pub key_id: String,
    pub reason: String,
    pub reauth_token: String,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustKeyView {
    pub key_id: String,
    pub label: String,
    pub public_key: String,
    pub fingerprint: String,
    pub status: PluginTrustKeyStatus,
    pub revision: i64,
    pub associated_package_count: i64,
    pub created_at: String,
    pub created_by: String,
    pub revoked_at: Option<String>,
    pub revoked_by: Option<String>,
    pub revoked_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSettingsView {
    pub developer_mode: bool,
    pub revision: i64,
    pub updated_at: Option<String>,
    pub updated_by: Option<String>,
}

#[derive(Clone)]
pub struct EnableDeveloperModeInput {
    pub reauth_token: String,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
}

#[derive(Clone, Debug)]
pub struct DisableDeveloperModeInput {
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantView {
    pub capability: String,
    pub scope: PluginGrantScope,
    pub sensitive: bool,
    pub status: PluginGrantStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSummaryView {
    pub id: String,
    pub name: String,
    pub version: String,
    pub package_hash: String,
    pub source: String,
    pub revision: i64,
    pub config_revision: i64,
    pub generation: i64,
    pub enabled: bool,
    pub lifecycle: PluginLifecycle,
    pub health: String,
    pub required_grants: Vec<String>,
    pub effective_grants: Vec<PluginGrant>,
    pub active_operation_id: Option<Uuid>,
    pub failure_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDetailView {
    pub id: String,
    pub name: String,
    pub version: String,
    pub package_hash: String,
    pub source: String,
    pub signing_key_id: Option<String>,
    pub revision: i64,
    pub config_revision: i64,
    pub generation: i64,
    pub enabled: bool,
    pub lifecycle: PluginLifecycle,
    pub health: String,
    pub required_grants: Vec<String>,
    pub effective_grants: Vec<PluginGrant>,
    pub active_operation_id: Option<Uuid>,
    pub failure_reason: Option<String>,
    pub state_schema: u32,
    pub credential_bound: bool,
    pub task_count: usize,
    pub version_count: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginConfigSchemaView {
    pub plugin_id: String,
    pub schema_revision: String,
    pub schema: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginConfigView {
    pub plugin_id: String,
    pub revision: i64,
    pub config_revision: i64,
    pub schema_revision: String,
    pub values: serde_json::Value,
    pub secrets: BTreeMap<String, SecretConfiguredStatus>,
}

#[derive(Clone)]
pub struct ValidateConfigInput {
    pub schema_revision: String,
    pub values: serde_json::Value,
    pub secrets: BTreeMap<String, SecretWriteAction>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigValidationView {
    pub valid: bool,
    pub schema_revision: String,
    pub values: serde_json::Value,
}

#[derive(Clone)]
pub struct UpdateConfigInput {
    pub actor_user_id: UserId,
    pub schema_revision: String,
    pub values: serde_json::Value,
    pub secrets: BTreeMap<String, SecretWriteAction>,
    pub expected_revision: i64,
    pub site_id: Option<domain::SiteId>,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionsView {
    pub plugin_id: String,
    pub package_hash: String,
    pub revision: i64,
    pub required_capabilities: Vec<String>,
    pub effective_grants: Vec<GrantView>,
}

#[derive(Clone, Debug)]
pub struct PreparePermissionsInput {
    pub requested_grants: Vec<PluginGrant>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantDiffView {
    pub added: Vec<PluginGrant>,
    pub kept: Vec<PluginGrant>,
    pub removed: Vec<PluginGrant>,
    pub required: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparationImpactView {
    pub summary: String,
    pub requires_drain: bool,
    pub requires_reauth: bool,
    pub resets_state: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PermissionsPreparationView {
    pub preparation_id: Uuid,
    pub plugin_id: String,
    pub package_hash: String,
    pub base_revision: i64,
    pub grant_diff: GrantDiffView,
    pub impact: PreparationImpactView,
    pub consent_token: String,
    pub expires_at: String,
}

impl fmt::Debug for PermissionsPreparationView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PermissionsPreparationView")
            .field("preparation_id", &self.preparation_id)
            .field("plugin_id", &self.plugin_id)
            .field("package_hash", &self.package_hash)
            .field("base_revision", &self.base_revision)
            .field("consent_token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Clone)]
pub struct GrantPermissionsInput {
    pub actor_user_id: UserId,
    pub consent_token: String,
    pub grants: Vec<PluginGrant>,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
}

#[derive(Clone, Debug)]
pub struct RevokePermissionsInput {
    pub actor_user_id: UserId,
    pub grants: Vec<PluginGrant>,
    pub reason: String,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
}

#[derive(Clone, Debug)]
pub struct EnablePluginInput {
    pub actor_user_id: UserId,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
}

#[derive(Clone, Debug)]
pub struct DisablePluginInput {
    pub actor_user_id: UserId,
    pub reason: Option<String>,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
}

#[derive(Clone, Debug)]
pub struct UninstallPluginInput {
    pub actor_user_id: UserId,
    pub remove_private_data: bool,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
}

#[derive(Clone, Debug)]
pub struct PrepareInstallInput {
    pub actor_user_id: UserId,
    pub upload_id: Option<Uuid>,
    pub repo_id: Option<String>,
    pub plugin_id: Option<String>,
    pub version: Option<String>,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct InstallationPreparationView {
    pub preparation_id: Uuid,
    pub plugin_id: String,
    pub version: String,
    pub package_hash: String,
    pub base_revision: i64,
    pub signature_status: String,
    pub signing_key_id: Option<String>,
    pub fingerprint: Option<String>,
    pub required_capabilities: Vec<String>,
    pub grant_diff: GrantDiffView,
    pub config_schema: serde_json::Value,
    pub config_schema_revision: String,
    pub state_schema: u32,
    pub impact: PreparationImpactView,
    pub consent_token: String,
    pub expires_at: String,
}

impl fmt::Debug for InstallationPreparationView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InstallationPreparationView")
            .field("preparation_id", &self.preparation_id)
            .field("plugin_id", &self.plugin_id)
            .field("version", &self.version)
            .field("package_hash", &self.package_hash)
            .field("consent_token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Clone)]
pub struct ExecuteInstallInput {
    pub actor_user_id: UserId,
    pub preparation_id: Uuid,
    pub consent_token: String,
    pub grants: Vec<PluginGrant>,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
}

#[derive(Clone)]
pub struct UpgradeTargetConfigInput {
    pub schema_revision: String,
    pub values: serde_json::Value,
    pub secrets: BTreeMap<String, SecretWriteAction>,
}

#[derive(Clone)]
pub struct PrepareUpgradeInput {
    pub actor_user_id: UserId,
    pub version: String,
    pub upload_id: Option<Uuid>,
    pub package_hash: Option<String>,
    pub repo_id: Option<String>,
    pub target_config: UpgradeTargetConfigInput,
    pub site_id: Option<domain::SiteId>,
    pub state_policy: String,
    pub reset_scopes: Option<Vec<PluginSecurityScope>>,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateResetPreviewView {
    pub kv_entries: i64,
    pub blob_entries: i64,
    pub kv_bytes: i64,
    pub blob_bytes: i64,
    pub state_revision: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateDiffView {
    pub old_state_schema: u32,
    pub new_state_schema: u32,
    pub schema_changed: bool,
    pub preserve_allowed: bool,
    pub reset_preview: Option<StateResetPreviewView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigDiffView {
    pub old_schema_revision: Option<String>,
    pub new_schema_revision: String,
    pub target_config_digest: String,
    pub existing_valid: bool,
    pub validation_errors: Vec<SchemaFieldError>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct UpgradePreparationView {
    pub preparation_id: Uuid,
    pub plugin_id: String,
    pub version: String,
    pub package_hash: String,
    pub base_revision: i64,
    pub signature_status: String,
    pub signing_key_id: Option<String>,
    pub state_policy: String,
    #[serde(default)]
    pub target_config_digest: String,
    pub grant_diff: GrantDiffView,
    pub config_diff: ConfigDiffView,
    pub state_diff: StateDiffView,
    pub impact: PreparationImpactView,
    pub consent_token: String,
    pub expires_at: String,
}

impl fmt::Debug for UpgradePreparationView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UpgradePreparationView")
            .field("preparation_id", &self.preparation_id)
            .field("plugin_id", &self.plugin_id)
            .field("version", &self.version)
            .field("package_hash", &self.package_hash)
            .field("state_policy", &self.state_policy)
            .field("consent_token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Clone)]
pub struct ExecuteUpgradeInput {
    pub actor_user_id: UserId,
    pub preparation_id: Uuid,
    pub consent_token: String,
    pub grants: Vec<PluginGrant>,
    pub state_policy: String,
    pub target_config_digest: String,
    pub reauth_token: Option<String>,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
}

#[derive(Clone)]
pub struct RollbackInput {
    pub actor_user_id: UserId,
    pub package_hash: String,
    pub preparation_id: Uuid,
    pub consent_token: String,
    pub grants: Vec<PluginGrant>,
    pub state_policy: String,
    pub target_config_digest: String,
    pub reauth_token: Option<String>,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
}

#[derive(Clone, Debug)]
pub struct PrepareStateResetInput {
    pub security_scopes: Vec<PluginSecurityScope>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct StateResetPreparationView {
    pub preparation_id: Uuid,
    pub plugin_id: String,
    pub base_revision: i64,
    pub kv_entries: i64,
    pub blob_entries: i64,
    pub kv_bytes: i64,
    pub blob_bytes: i64,
    pub state_revision: i64,
    pub consent_token: String,
    pub expires_at: String,
}

impl fmt::Debug for StateResetPreparationView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StateResetPreparationView")
            .field("preparation_id", &self.preparation_id)
            .field("plugin_id", &self.plugin_id)
            .field("base_revision", &self.base_revision)
            .field("state_revision", &self.state_revision)
            .field("consent_token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Clone)]
pub struct ExecuteStateResetInput {
    pub actor_user_id: UserId,
    pub security_scopes: Vec<PluginSecurityScope>,
    pub consent_token: String,
    pub reauth_token: String,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OperationAdminView {
    pub operation_id: Uuid,
    pub plugin_id: Option<String>,
    pub kind: String,
    pub state: PluginOperationState,
    pub phase: String,
    pub revision: i64,
    pub generation: Option<i64>,
    pub job_id: Option<String>,
    pub progress: u8,
    pub blocked_reason: Option<String>,
    pub error: Option<String>,
    pub result: Option<serde_json::Value>,
    pub next_run_at: Option<String>,
    pub retry_after_ms: Option<u64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug)]
pub struct CancelOperationInput {
    pub actor_user_id: UserId,
    pub reason: Option<String>,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
}

#[derive(Clone, Debug)]
pub struct ResumeOperationInput {
    pub actor_user_id: UserId,
    pub expected_revision: i64,
    pub evidence_ref: Option<String>,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskScheduleView {
    pub interval_secs: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskAdminView {
    pub id: String,
    pub binding_id: Uuid,
    pub plugin_id: String,
    pub definition_revision: i64,
    pub enabled: bool,
    pub schedule: TaskScheduleView,
    pub config: serde_json::Value,
    pub last_job: Option<String>,
    pub next_run_after: Option<String>,
    pub ownership_summary: String,
}

#[derive(Clone, Debug)]
pub struct UpdateTaskInput {
    pub actor_user_id: UserId,
    pub enabled: bool,
    pub interval_secs: i64,
    pub config: serde_json::Value,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
}

#[derive(Clone, Debug)]
pub struct RunTaskInput {
    pub actor_user_id: UserId,
    pub input: serde_json::Value,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskReceiptView {
    pub command_id: Uuid,
    pub operation_id: Uuid,
    pub job_id: Option<String>,
    pub scope: Vec<PluginGrantScope>,
    pub generation: Option<i64>,
    pub state: PluginOperationState,
    pub phase: String,
    pub effect: String,
    pub evidence: Option<String>,
    pub observed_at: String,
}

#[derive(Clone, Debug)]
pub struct SubmitAdminCommandInput {
    pub actor_user_id: UserId,
    pub kind: String,
    pub payload: serde_json::Value,
    pub targets: Vec<store::plugins::PluginCommandTarget>,
    pub evidence_ref: Option<String>,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEventView {
    pub operation_id: Uuid,
    pub plugin_id: Option<String>,
    pub action: String,
    pub subject_kind: String,
    pub canonical_method: String,
    pub canonical_path: String,
    pub state: PluginOperationState,
    pub phase: String,
    pub effect: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginMetricsView {
    pub plugin_id: String,
    pub window_secs: u64,
    pub active_workers: usize,
    pub in_flight_invocations: usize,
    pub draining: bool,
    pub unknown_operations: usize,
    pub total_invocations: u64,
    pub total_errors: u64,
    pub avg_latency_ms: f64,
    pub max_latency_ms: u64,
}
