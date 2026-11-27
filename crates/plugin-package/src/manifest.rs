use crate::manifest_rules::validate_manifest_rules;
use crate::toml_parse::parse_toml_to_json;
use crate::{PackageError, PackagePolicy};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub manifest_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub api: String,
    pub license: String,
    pub authors: Vec<String>,
    pub description: String,
    pub config_schema: String,
    pub state_schema: u32,
    #[serde(default)]
    pub credential_binding: Option<String>,
    #[serde(default)]
    pub http: Vec<HttpDestinationManifest>,
    #[serde(default)]
    pub sites: Vec<String>,
    #[serde(default)]
    pub limits: Option<LimitsManifest>,
    #[serde(default)]
    pub components: Vec<ComponentManifest>,
    #[serde(default)]
    pub ui: Option<UiManifest>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpDestinationManifest {
    pub origin: String,
    pub path_prefix: String,
    pub methods: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsManifest {
    pub memory_mib: Option<u32>,
    pub fuel_limit: Option<u64>,
    pub io_ms: Option<u64>,
    pub wall_ms: Option<u64>,
    pub http_calls: Option<u32>,
    pub response_bytes: Option<u64>,
    pub kv_bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentManifest {
    pub id: String,
    pub entry: String,
    pub world: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub hooks: Vec<HookManifest>,
    #[serde(default)]
    pub tasks: Vec<TaskManifest>,
    #[serde(default)]
    pub indexers: Vec<IndexerManifest>,
    #[serde(default)]
    pub maintenance: Vec<MaintenanceManifest>,
    #[serde(default)]
    pub endpoints: Vec<EndpointManifest>,
    #[serde(default)]
    pub lists: Vec<ListManifest>,
    #[serde(default)]
    pub channels: Vec<ChannelManifest>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookManifest {
    pub id: String,
    pub priority: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskScheduleManifest {
    pub kind: String,
    pub seconds: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskManifest {
    pub id: String,
    pub input_schema: String,
    pub schedule: TaskScheduleManifest,
    #[serde(default)]
    pub concurrency_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexerManifest {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub capabilities: Vec<String>,
    pub site_schema: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceManifest {
    pub id: String,
    pub actions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointManifest {
    pub id: String,
    pub method: String,
    pub path: String,
    pub auth: String,
    pub input_schema: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListManifest {
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelManifest {
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiManifest {
    pub kind: String,
    pub contract: String,
    pub remote_name: String,
    pub entry: String,
    pub asset_manifest: String,
    pub react: String,
    pub react_dom: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub surfaces: Vec<UiSurfaceManifest>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiSurfaceKind {
    Config,
    Page,
    App,
    Dashboard,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiSurfaceManifest {
    pub id: String,
    pub kind: UiSurfaceKind,
    pub expose: String,
    pub title: String,
    #[serde(default)]
    pub icon: Option<String>,
}

pub fn validate_manifest_toml(toml_str: &str) -> Result<PluginManifest, PackageError> {
    validate_manifest_toml_with_policy(toml_str, &PackagePolicy::default())
}

pub fn validate_manifest_toml_with_policy(
    toml_str: &str,
    policy: &PackagePolicy,
) -> Result<PluginManifest, PackageError> {
    let json_val = parse_toml_to_json(toml_str)?;
    let manifest: PluginManifest = serde_json::from_value(json_val)
        .map_err(|e| PackageError::InvalidManifest(format!("manifest schema error: {e}")))?;
    validate_manifest_rules(&manifest, policy)?;
    Ok(manifest)
}
