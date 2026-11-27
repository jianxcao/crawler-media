mod archive;
mod config_secrets;
mod config_values;
mod error;
mod manifest;
mod manifest_components;
mod manifest_rules;
mod minisign;
mod minisign_sign;
mod package;
mod policy;
mod schema;
mod schema_annotations;
mod toml_parse;
mod toml_scalars;
mod ui_manifest;
mod yaml_guard;
mod zip_writer;

pub use config_secrets::{
    ResolvedSecretUpdate, SecretFieldInput, SecretWriteAction, SplitConfigResult,
    ValidatedConfigWrite, split_config_secrets,
};
pub use config_values::{
    RedactedConfigView, SecretConfiguredStatus, project_config_with_secrets, validate_config_values,
};
pub use error::{PackageError, SchemaFieldError};
pub use manifest::{
    ChannelManifest, ComponentManifest, EndpointManifest, HookManifest, HttpDestinationManifest,
    IndexerManifest, LimitsManifest, ListManifest, MaintenanceManifest, PluginManifest,
    TaskManifest, TaskScheduleManifest, UiManifest, UiSurfaceKind, UiSurfaceManifest,
    validate_manifest_toml, validate_manifest_toml_with_policy,
};
pub use minisign::{
    ParsedPublicKey, parse_minisign_public_key, verify_minisign, verify_minisign_payload,
};
pub use minisign_sign::{MinisignSigningKey, parse_signing_key, sign_cmplugin_bytes};
pub use package::{
    ArchiveEntryMeta, BUILTIN_SITE_PROFILE_IDS, ValidatedComponent, ValidatedPackage,
    ValidatedSiteProfile, ValidatedUiBundle, validate_cmplugin_bytes, validate_cmplugin_reader,
    validate_site_profile_id_and_framework, validate_site_profile_yaml,
};
pub use policy::{
    ArchiveLimits, PackagePolicy, SignatureStatus, SignatureVerification, TrustedKey,
};
pub use schema::{SecretFieldSpec, ValidatedConfigSchema, validate_config_schema};
pub use ui_manifest::{
    UiAssetEntry, UiAssetManifest, is_forbidden_ui_path as is_forbidden_ui_asset_path,
    validate_ui_manifest, validate_ui_manifest_with_policy,
};
pub use yaml_guard::reject_yaml_anchors_and_aliases as reject_yaml_anchors;
pub use zip_writer::{build_deterministic_cmplugin_zip, validate_safe_package_path};
