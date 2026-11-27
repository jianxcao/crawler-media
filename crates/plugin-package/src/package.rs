use crate::archive::{ExtractedEntry, extract_zip_entries, read_bounded_bytes};
use crate::minisign::verify_minisign;
use crate::ui_manifest::validate_ui_manifest_with_policy;
use crate::yaml_guard::reject_yaml_anchors_and_aliases;
use crate::{
    PackageError, PackagePolicy, PluginManifest, SignatureStatus, SignatureVerification,
    UiAssetManifest, UiManifest, ValidatedConfigSchema, validate_config_schema,
    validate_manifest_toml_with_policy,
};
use serde::Deserialize;
use std::borrow::Borrow;
use std::collections::{BTreeMap, HashSet};
use std::io::Read;

const MAX_SITE_YAML_BYTES: usize = 256 * 1024;
const MAX_SCHEMA_JSON_BYTES: usize = 512 * 1024;

pub const BUILTIN_SITE_PROFILE_IDS: &[&str] = &[
    "demo",
    "mteam",
    "pterclub",
    "chdbits",
    "hdsky",
    "hddolby",
    "hdfans",
    "ourbits",
    "keepfrds",
    "agsvpt",
    "audiences",
    "hdarea",
    "hdhome",
    "hdtime",
    "hdvideo",
    "hhanclub",
    "nicept",
    "piggo",
    "pthome",
    "pttime",
    "soulvoice",
    "ssd",
    "tjupt",
    "ttg",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveEntryMeta {
    pub path: String,
    pub size: u64,
    pub sha256_hex: String,
}

#[derive(Clone, Debug)]
pub struct ValidatedComponent {
    pub id: String,
    pub entry: String,
    pub world: String,
    pub permissions: Vec<String>,
    pub wasm_bytes: Vec<u8>,
    pub task_input_schemas: BTreeMap<String, ValidatedConfigSchema>,
    pub endpoint_input_schemas: BTreeMap<String, ValidatedConfigSchema>,
    pub indexer_site_schemas: BTreeMap<String, ValidatedConfigSchema>,
}

#[derive(Clone, Debug)]
pub struct ValidatedSiteProfile {
    pub path: String,
    pub id: String,
    pub yaml_bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct ValidatedUiBundle {
    pub declaration: UiManifest,
    pub asset_manifest: UiAssetManifest,
    pub ui_metadata_ref: String,
    pub assets: BTreeMap<String, Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct ValidatedPackage {
    pub package_sha256: [u8; 32],
    pub package_sha256_hex: String,
    pub signature: SignatureVerification,
    pub manifest: PluginManifest,
    pub manifest_json: String,
    pub config_schema: ValidatedConfigSchema,
    pub config_schema_json: String,
    pub state_schema: u32,
    pub credential_bound: bool,
    pub entries: Vec<ArchiveEntryMeta>,
    pub components: Vec<ValidatedComponent>,
    pub sites: Vec<ValidatedSiteProfile>,
    pub ui: Option<ValidatedUiBundle>,
    pub component_bytes: BTreeMap<String, Vec<u8>>,
    pub site_yaml_bytes: BTreeMap<String, Vec<u8>>,
    pub ui_asset_bytes: BTreeMap<String, Vec<u8>>,
}

#[derive(Deserialize)]
struct SiteProfileCheck {
    id: String,
    framework: String,
    search: SiteSearchCheck,
}

#[derive(Deserialize)]
struct SiteSearchCheck {
    path: String,
    query_param: String,
}

pub fn validate_cmplugin_bytes(
    bytes: &[u8],
    policy: impl Borrow<PackagePolicy>,
) -> Result<ValidatedPackage, PackageError> {
    let policy = policy.borrow();
    if (bytes.len() as u64) > policy.limits.max_package_bytes {
        return Err(PackageError::LimitExceeded(format!(
            "compressed package exceeds {} bytes",
            policy.limits.max_package_bytes
        )));
    }
    validate_cmplugin_reader(bytes, policy)
}

pub fn validate_cmplugin_reader<R: Read>(
    reader: R,
    policy: impl Borrow<PackagePolicy>,
) -> Result<ValidatedPackage, PackageError> {
    let policy = policy.borrow();
    let bounded = read_bounded_bytes(reader, &policy.limits)?;
    let signature =
        verify_package_signature(&bounded.package_sha256, &bounded.package_sha256_hex, policy)?;
    let entries = extract_zip_entries(&bounded.raw, &policy.limits)?;

    let manifest_entry = entries
        .get("plugin.toml")
        .ok_or(PackageError::MissingManifest)?;
    let manifest_str = std::str::from_utf8(&manifest_entry.bytes)
        .map_err(|_| PackageError::InvalidManifest("plugin.toml must be valid UTF-8".into()))?;
    let manifest = validate_manifest_toml_with_policy(manifest_str, policy)?;
    let manifest_json = serde_json::to_string(&manifest).map_err(|e| {
        PackageError::InvalidManifest(format!("manifest serialization failed: {e}"))
    })?;

    let config_schema = read_and_validate_schema(&entries, &manifest.config_schema)?;
    let config_schema_json = config_schema.canonical_json().to_string();

    let (components, component_bytes, declared_wasm) = validate_components(&manifest, &entries)?;
    let ValidatedSitesOutput {
        profiles: sites,
        yaml_map: site_yaml_bytes,
    } = validate_sites(&manifest.sites, &entries)?;

    let entry_metas: Vec<ArchiveEntryMeta> = entries
        .values()
        .map(|e| ArchiveEntryMeta {
            path: e.path.clone(),
            size: e.size,
            sha256_hex: e.sha256_hex.clone(),
        })
        .collect();

    let (ui, ui_asset_bytes, declared_ui_paths) =
        validate_ui_bundle(&manifest, &entries, &entry_metas, policy)?;

    reject_undeclared_executables(&entries, &declared_wasm, &declared_ui_paths)?;

    Ok(ValidatedPackage {
        package_sha256: bounded.package_sha256,
        package_sha256_hex: bounded.package_sha256_hex,
        signature,
        credential_bound: manifest.credential_binding.is_some(),
        state_schema: manifest.state_schema,
        manifest,
        manifest_json,
        config_schema,
        config_schema_json,
        entries: entry_metas,
        components,
        sites,
        ui,
        component_bytes,
        site_yaml_bytes,
        ui_asset_bytes,
    })
}

fn verify_package_signature(
    package_sha256: &[u8; 32],
    package_sha256_hex: &str,
    policy: &PackagePolicy,
) -> Result<SignatureVerification, PackageError> {
    match &policy.detached_signature {
        Some(sig_text) => verify_minisign(package_sha256, sig_text, &policy.trust_keys),
        None if policy.dev_mode => Ok(SignatureVerification {
            status: SignatureStatus::UnsignedDevMode,
            package_sha256_hex: package_sha256_hex.to_string(),
        }),
        None => Err(PackageError::SignatureRequired),
    }
}

fn read_and_validate_schema(
    entries: &BTreeMap<String, ExtractedEntry>,
    path: &str,
) -> Result<ValidatedConfigSchema, PackageError> {
    let entry = entries.get(path).ok_or_else(|| {
        PackageError::InvalidManifest(format!("declared schema `{path}` missing from archive"))
    })?;
    if entry.bytes.len() > MAX_SCHEMA_JSON_BYTES {
        return Err(PackageError::InvalidSchema(format!(
            "schema `{path}` exceeds 512 KiB limit"
        )));
    }
    let json: serde_json::Value = serde_json::from_slice(&entry.bytes).map_err(|e| {
        PackageError::InvalidSchema(format!("invalid JSON in schema `{path}`: {e}"))
    })?;
    validate_config_schema(&json)
}

#[allow(clippy::type_complexity)]
fn validate_components(
    manifest: &PluginManifest,
    entries: &BTreeMap<String, ExtractedEntry>,
) -> Result<
    (
        Vec<ValidatedComponent>,
        BTreeMap<String, Vec<u8>>,
        HashSet<String>,
    ),
    PackageError,
> {
    let mut components = Vec::with_capacity(manifest.components.len());
    let mut component_bytes = BTreeMap::new();
    let mut declared_wasm = HashSet::new();

    for comp in &manifest.components {
        let wasm_entry = entries.get(&comp.entry).ok_or_else(|| {
            PackageError::InvalidManifest(format!(
                "component `{}` entry `{}` missing from archive",
                comp.id, comp.entry
            ))
        })?;
        if !wasmparser::Parser::is_component(&wasm_entry.bytes) {
            return Err(PackageError::InvalidManifest(format!(
                "component `{}` entry `{}` is not a valid WebAssembly Component Model binary",
                comp.id, comp.entry
            )));
        }
        wasmparser::Validator::new_with_features(wasmparser::WasmFeatures::default())
            .validate_all(&wasm_entry.bytes)
            .map_err(|e| {
                PackageError::InvalidManifest(format!(
                    "component `{}` entry `{}` failed WebAssembly component validation: {e}",
                    comp.id, comp.entry
                ))
            })?;
        declared_wasm.insert(comp.entry.clone());
        component_bytes.insert(comp.entry.clone(), wasm_entry.bytes.clone());

        let mut task_input_schemas = BTreeMap::new();
        for t in &comp.tasks {
            let schema = read_and_validate_schema(entries, &t.input_schema)?;
            task_input_schemas.insert(t.id.clone(), schema);
        }
        let mut endpoint_input_schemas = BTreeMap::new();
        for ep in &comp.endpoints {
            let schema = read_and_validate_schema(entries, &ep.input_schema)?;
            endpoint_input_schemas.insert(ep.id.clone(), schema);
        }
        let mut indexer_site_schemas = BTreeMap::new();
        for idx in &comp.indexers {
            let schema = read_and_validate_schema(entries, &idx.site_schema)?;
            indexer_site_schemas.insert(idx.id.clone(), schema);
        }

        components.push(ValidatedComponent {
            id: comp.id.clone(),
            entry: comp.entry.clone(),
            world: comp.world.clone(),
            permissions: comp.permissions.clone(),
            wasm_bytes: wasm_entry.bytes.clone(),
            task_input_schemas,
            endpoint_input_schemas,
            indexer_site_schemas,
        });
    }

    Ok((components, component_bytes, declared_wasm))
}

struct ValidatedSitesOutput {
    profiles: Vec<ValidatedSiteProfile>,
    yaml_map: BTreeMap<String, Vec<u8>>,
}

pub fn validate_site_profile_id_and_framework(
    raw_id: &str,
    framework: &str,
    path: &str,
) -> Result<String, PackageError> {
    let id = raw_id.trim().to_string();
    if id.is_empty() {
        return Err(PackageError::InvalidManifest(format!(
            "site profile `{path}` has empty id"
        )));
    }
    if !matches!(framework, "nexusphp" | "api") {
        return Err(PackageError::InvalidManifest(format!(
            "site profile `{path}` has unsupported framework `{framework}`"
        )));
    }
    if BUILTIN_SITE_PROFILE_IDS.contains(&id.as_str()) {
        return Err(PackageError::InvalidManifest(format!(
            "site profile id `{id}` collides with built-in indexer profile"
        )));
    }
    Ok(id)
}

pub fn validate_site_profile_yaml(yaml_bytes: &[u8], path: &str) -> Result<String, PackageError> {
    if yaml_bytes.len() > MAX_SITE_YAML_BYTES {
        return Err(PackageError::InvalidManifest(format!(
            "site profile `{path}` exceeds 256 KiB"
        )));
    }
    let text = std::str::from_utf8(yaml_bytes).map_err(|_| {
        PackageError::InvalidManifest(format!("site profile `{path}` must be valid UTF-8"))
    })?;
    reject_yaml_anchors_and_aliases(text, path)?;
    let _: serde_yaml::Value = serde_yaml::from_str(text)
        .map_err(|e| PackageError::InvalidManifest(format!("invalid site YAML `{path}`: {e}")))?;
    let parsed: SiteProfileCheck = serde_yaml::from_str(text)
        .map_err(|e| PackageError::InvalidManifest(format!("invalid site YAML `{path}`: {e}")))?;
    let id = validate_site_profile_id_and_framework(&parsed.id, &parsed.framework, path)?;
    if parsed.search.path.trim().is_empty() || parsed.search.query_param.trim().is_empty() {
        return Err(PackageError::InvalidManifest(format!(
            "site profile `{path}` search.path and search.query_param must be non-empty"
        )));
    }
    Ok(id)
}

fn validate_sites(
    site_paths: &[String],
    entries: &BTreeMap<String, ExtractedEntry>,
) -> Result<ValidatedSitesOutput, PackageError> {
    let mut profiles = Vec::with_capacity(site_paths.len());
    let mut yaml_map = BTreeMap::new();
    let mut seen_ids = HashSet::new();

    for path in site_paths {
        let entry = entries.get(path).ok_or_else(|| {
            PackageError::InvalidManifest(format!(
                "declared site profile `{path}` missing from archive"
            ))
        })?;
        let id = validate_site_profile_yaml(&entry.bytes, path)?;
        if !seen_ids.insert(id.clone()) {
            return Err(PackageError::InvalidManifest(format!(
                "duplicate site profile id `{id}` in package"
            )));
        }
        yaml_map.insert(path.clone(), entry.bytes.clone());
        profiles.push(ValidatedSiteProfile {
            path: path.clone(),
            id,
            yaml_bytes: entry.bytes.clone(),
        });
    }

    Ok(ValidatedSitesOutput { profiles, yaml_map })
}

#[allow(clippy::type_complexity)]
fn validate_ui_bundle(
    manifest: &PluginManifest,
    entries: &BTreeMap<String, ExtractedEntry>,
    entry_metas: &[ArchiveEntryMeta],
    policy: &PackagePolicy,
) -> Result<
    (
        Option<ValidatedUiBundle>,
        BTreeMap<String, Vec<u8>>,
        HashSet<String>,
    ),
    PackageError,
> {
    let Some(ui_decl) = &manifest.ui else {
        return Ok((None, BTreeMap::new(), HashSet::new()));
    };
    let manifest_entry = entries.get(&ui_decl.asset_manifest).ok_or_else(|| {
        PackageError::InvalidUiManifest(format!(
            "declared ui.asset_manifest `{}` missing from archive",
            ui_decl.asset_manifest
        ))
    })?;
    let json: serde_json::Value = serde_json::from_slice(&manifest_entry.bytes).map_err(|e| {
        PackageError::InvalidUiManifest(format!(
            "invalid JSON in `{}`: {e}",
            ui_decl.asset_manifest
        ))
    })?;
    let asset_manifest =
        validate_ui_manifest_with_policy(&json, Some(ui_decl), entry_metas, policy)?;

    let mut assets = BTreeMap::new();
    let mut declared_ui_paths = HashSet::new();
    declared_ui_paths.insert(ui_decl.asset_manifest.clone());

    for file in &asset_manifest.files {
        let entry = entries.get(&file.path).ok_or_else(|| {
            PackageError::InvalidUiManifest(format!(
                "declared UI asset `{}` missing from archive",
                file.path
            ))
        })?;
        declared_ui_paths.insert(file.path.clone());
        assets.insert(file.path.clone(), entry.bytes.clone());
    }

    let ui_metadata_ref = format!("{}:{}", ui_decl.asset_manifest, manifest_entry.sha256_hex);
    let bundle = ValidatedUiBundle {
        declaration: ui_decl.clone(),
        asset_manifest,
        ui_metadata_ref,
        assets: assets.clone(),
    };
    Ok((Some(bundle), assets, declared_ui_paths))
}

fn reject_undeclared_executables(
    entries: &BTreeMap<String, ExtractedEntry>,
    declared_wasm: &HashSet<String>,
    declared_ui_paths: &HashSet<String>,
) -> Result<(), PackageError> {
    for path in entries.keys() {
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(".wasm") && !declared_wasm.contains(path) {
            return Err(PackageError::ForbiddenExecutable(format!(
                "undeclared wasm component `{path}`"
            )));
        }
        if (lower.ends_with(".js") || lower.ends_with(".mjs") || lower.ends_with(".cjs"))
            && !declared_ui_paths.contains(path)
        {
            return Err(PackageError::ForbiddenExecutable(format!(
                "undeclared script `{path}`"
            )));
        }
        if lower.starts_with("ui/") && !declared_ui_paths.contains(path) {
            return Err(PackageError::ForbiddenUiAsset(format!(
                "undeclared file `{path}` in ui/"
            )));
        }
    }
    Ok(())
}
