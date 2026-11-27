use std::collections::BTreeMap;

use plugin_package::SecretWriteAction;
use serde::Deserialize;
use store::plugins::PluginGrant;
use uuid::Uuid;

use crate::plugins::ReauthPurpose;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReauthRequest {
    pub password: String,
    pub purpose: ReauthPurpose,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PrepareTrustKeyRequest {
    pub public_key: String,
    pub label: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateTrustKeyRequest {
    #[serde(default)]
    pub label: Option<String>,
    pub public_key: String,
    pub fingerprint: String,
    pub consent_token: String,
    pub reauth_token: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RevokeTrustKeyRequest {
    pub reason: String,
    pub reauth_token: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EnableDeveloperModeRequest {
    pub reauth_token: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EmptyObjectRequest {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PrepareInstallRequest {
    #[serde(default)]
    pub upload_id: Option<Uuid>,
    #[serde(default)]
    pub repo_id: Option<String>,
    #[serde(default)]
    pub plugin_id: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExecuteInstallRequest {
    pub preparation_id: Uuid,
    pub consent_token: String,
    pub grants: Vec<PluginGrant>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ValidateConfigRequest {
    pub schema_revision: String,
    pub values: serde_json::Value,
    #[serde(default)]
    pub secrets: BTreeMap<String, SecretWriteAction>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UpdateConfigRequest {
    pub schema_revision: String,
    pub values: serde_json::Value,
    #[serde(default)]
    pub secrets: BTreeMap<String, SecretWriteAction>,
    #[serde(default)]
    pub site_id: Option<domain::SiteId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PreparePermissionsRequest {
    pub requested_grants: Vec<PluginGrant>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GrantPermissionsRequest {
    pub consent_token: String,
    pub grants: Vec<PluginGrant>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RevokePermissionsRequest {
    pub grants: Vec<PluginGrant>,
    pub reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DisablePluginRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UninstallPluginRequest {
    pub remove_private_data: bool,
}

fn default_empty_json_obj() -> serde_json::Value {
    serde_json::json!({})
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UpgradeTargetConfigRequest {
    #[serde(default)]
    pub schema_revision: String,
    #[serde(default = "default_empty_json_obj")]
    pub values: serde_json::Value,
    #[serde(default)]
    pub secrets: BTreeMap<String, SecretWriteAction>,
}

impl Default for UpgradeTargetConfigRequest {
    fn default() -> Self {
        Self {
            schema_revision: String::new(),
            values: default_empty_json_obj(),
            secrets: BTreeMap::new(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum GlobalScopeTag {
    #[serde(alias = "Global")]
    Global,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SiteObjScope {
    pub site_id: domain::SiteId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TaggedSiteScope {
    #[serde(rename = "Site")]
    pub site: domain::SiteId,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum HttpSecurityScope {
    Global(GlobalScopeTag),
    SiteObj(SiteObjScope),
    TaggedSite(TaggedSiteScope),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PrepareUpgradeRequest {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub upload_id: Option<Uuid>,
    #[serde(default)]
    pub package_hash: Option<String>,
    #[serde(default)]
    pub repo_id: Option<String>,
    #[serde(default)]
    pub target_config: UpgradeTargetConfigRequest,
    #[serde(default)]
    pub site_id: Option<domain::SiteId>,
    pub state_policy: String,
    #[serde(default)]
    pub reset_scopes: Option<Vec<HttpSecurityScope>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExecuteUpgradeRequest {
    pub preparation_id: Uuid,
    pub consent_token: String,
    pub grants: Vec<PluginGrant>,
    pub state_policy: String,
    pub target_config_digest: String,
    #[serde(default)]
    pub reauth_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RollbackRequest {
    pub package_hash: String,
    pub preparation_id: Uuid,
    pub consent_token: String,
    pub grants: Vec<PluginGrant>,
    pub state_policy: String,
    pub target_config_digest: String,
    #[serde(default)]
    pub reauth_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PrepareStateResetRequest {
    #[serde(default)]
    pub security_scopes: Vec<HttpSecurityScope>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExecuteStateResetRequest {
    #[serde(default)]
    pub security_scopes: Vec<HttpSecurityScope>,
    pub consent_token: String,
    pub reauth_token: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CancelOperationRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ResumeOperationRequest {
    #[serde(default)]
    pub evidence_ref: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskScheduleRequest {
    pub interval_secs: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UpdateTaskRequest {
    pub enabled: bool,
    pub schedule: TaskScheduleRequest,
    pub config: serde_json::Value,
}

fn empty_json_object() -> serde_json::Value {
    serde_json::json!({})
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RunTaskRequest {
    #[serde(default = "empty_json_object")]
    pub input: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub(super) enum HttpCommandTarget {
    Global,
    Site { id: domain::SiteId },
    Library { id: domain::LibraryId },
    Downloader { id: domain::DownloaderId },
    Subscribe { id: domain::SubscribeId },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SubmitCommandRequest {
    pub kind: String,
    pub payload: serde_json::Value,
    #[serde(default)]
    pub targets: Vec<HttpCommandTarget>,
    #[serde(default)]
    pub evidence_ref: Option<String>,
}

fn default_repo_enabled() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PluginRepositoryRequestItem {
    pub id: String,
    pub name: String,
    pub index_url: String,
    #[serde(default = "default_repo_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub builtin: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SavePluginRepositoriesRequest {
    pub repositories: Vec<PluginRepositoryRequestItem>,
}
