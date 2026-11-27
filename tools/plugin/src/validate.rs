use crate::error::CliError;
pub(crate) use crate::path_guard::{
    ensure_no_symlink_components, ensure_project_root_dir_no_symlink,
    ensure_safe_output_subpath_no_symlinks, read_project_file_no_symlinks,
    read_project_file_no_symlinks_optional,
};
use plugin_package::{
    ArchiveEntryMeta, PackageError, PackagePolicy, PluginManifest, SignatureStatus, TrustedKey,
    build_deterministic_cmplugin_zip, is_forbidden_ui_asset_path, parse_minisign_public_key,
    validate_cmplugin_bytes, validate_config_schema, validate_manifest_toml_with_policy,
    validate_safe_package_path, validate_site_profile_yaml, validate_ui_manifest_with_policy,
    verify_minisign,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_MANIFEST_BYTES: usize = 512 * 1024;
const MAX_SCHEMA_BYTES: usize = 512 * 1024;
const FORBIDDEN_EXEC_EXTS: &[&str] = &[
    "sh", "bat", "cmd", "exe", "dll", "so", "dylib", "ps1", "py", "rb", "pl",
];

pub fn execute_validate_with_trust_key(
    path: &Path,
    trust_key: Option<&str>,
) -> Result<String, CliError> {
    let clean_path = ensure_no_symlink_components(path, true)?;
    let meta = fs::symlink_metadata(&clean_path)?;
    if meta.is_file() {
        return validate_archive_file(&clean_path, trust_key);
    }
    if trust_key.is_some() {
        return Err(CliError::Usage(
            "--trust-key can only be used when validating a .cmplugin archive file".into(),
        ));
    }
    let manifest = validate_project_dir(&clean_path, false)?;
    Ok(format!(
        "Valid plugin project: {}@{}\n",
        manifest.id, manifest.version
    ))
}

fn load_trusted_public_key(raw: &str) -> Result<TrustedKey, CliError> {
    let candidate_path = Path::new(raw);
    let key_text = if !raw.contains('\n') && fs::symlink_metadata(candidate_path).is_ok() {
        let clean_key = ensure_no_symlink_components(candidate_path, true)?;
        if fs::symlink_metadata(&clean_key)?.is_file() {
            fs::read_to_string(&clean_key)?
        } else {
            raw.to_string()
        }
    } else if !raw.contains('\n') && candidate_path.is_file() {
        let clean_key = ensure_no_symlink_components(candidate_path, true)?;
        fs::read_to_string(&clean_key)?
    } else {
        raw.to_string()
    };
    let parsed = parse_minisign_public_key(&key_text)?;
    Ok(TrustedKey {
        key_id: parsed.key_id,
        public_key: key_text,
        revoked: false,
    })
}

fn parse_minisig_syntax_key_id(sig_text: &str) -> Result<String, CliError> {
    match verify_minisign(&[0u8; 32], sig_text, &[]) {
        Err(PackageError::UntrustedKey(key_id)) => Ok(key_id),
        Err(err) => Err(CliError::Package(err)),
        Ok(ver) => match ver.status {
            SignatureStatus::Trusted { key_id, .. } => Ok(key_id),
            SignatureStatus::UnsignedDevMode => Ok(String::new()),
        },
    }
}

fn validate_archive_file(path: &Path, trust_key: Option<&str>) -> Result<String, CliError> {
    let bytes = fs::read(path)?;
    let sig_path = PathBuf::from(format!("{}.minisig", path.display()));
    let clean_sig = ensure_no_symlink_components(&sig_path, false)?;
    let detached_signature = match fs::symlink_metadata(&clean_sig) {
        Ok(meta) if meta.is_file() => Some(fs::read_to_string(&clean_sig)?),
        Ok(_) => {
            return Err(CliError::Validation(format!(
                "signature path `{}` is not a regular file",
                sig_path.display()
            )));
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(CliError::Io(err)),
    };

    if let Some(raw_trust) = trust_key {
        let trusted = load_trusted_public_key(raw_trust)?;
        let policy = PackagePolicy {
            dev_mode: false,
            detached_signature,
            trust_keys: vec![trusted],
            ..PackagePolicy::default()
        };
        let validated = validate_cmplugin_bytes(&bytes, &policy)?;
        let key_id = match &validated.signature.status {
            SignatureStatus::Trusted { key_id, .. } => key_id.as_str(),
            SignatureStatus::UnsignedDevMode => "unsigned",
        };
        return Ok(format!(
            "Valid signed plugin package: {}@{} (sha256: {}, key_id: {key_id})\n",
            validated.manifest.id, validated.manifest.version, validated.package_sha256_hex
        ));
    }

    let sig_key_id = match &detached_signature {
        Some(sig_text) => Some(parse_minisig_syntax_key_id(sig_text)?),
        None => None,
    };
    let policy = PackagePolicy {
        dev_mode: true,
        detached_signature: None,
        ..PackagePolicy::default()
    };
    let validated = validate_cmplugin_bytes(&bytes, &policy)?;
    match sig_key_id {
        Some(key_id) => Ok(format!(
            "Valid plugin package: {}@{} (sha256: {}, signature key_id: {key_id})\n",
            validated.manifest.id, validated.manifest.version, validated.package_sha256_hex
        )),
        None => Ok(format!(
            "Valid plugin package: {}@{} (sha256: {})\n",
            validated.manifest.id, validated.manifest.version, validated.package_sha256_hex
        )),
    }
}

pub fn validate_project_dir(
    dir: &Path,
    require_built_artifacts: bool,
) -> Result<PluginManifest, CliError> {
    ensure_project_root_dir_no_symlink(dir)?;
    let manifest_bytes = read_project_file_no_symlinks(dir, "plugin.toml")?;
    if manifest_bytes.len() > MAX_MANIFEST_BYTES {
        return Err(CliError::Validation(
            "plugin.toml exceeds 512 KiB limit".into(),
        ));
    }
    let manifest_str = std::str::from_utf8(&manifest_bytes)
        .map_err(|_| CliError::Validation("plugin.toml must be valid UTF-8".into()))?;
    let policy = PackagePolicy::default();
    let manifest = validate_manifest_toml_with_policy(manifest_str, &policy)?;

    let mut path_registry = PackagePathRegistry::default();
    validate_manifest_declared_paths(&manifest, &mut path_registry)?;

    validate_schema_file(dir, &manifest.config_schema)?;
    validate_components_in_dir(dir, &manifest, require_built_artifacts)?;
    validate_sites_in_dir(dir, &manifest.sites)?;
    if let Some(ui) = &manifest.ui {
        validate_ui_in_dir(
            dir,
            ui,
            &policy,
            require_built_artifacts,
            &mut path_registry,
        )?;
    }

    Ok(manifest)
}

fn check_forbidden_exec_ext(path: &str) -> Result<(), CliError> {
    let trimmed = path.trim_end_matches('/');
    if let Some((_, ext)) = trimmed.rsplit_once('.') {
        let lower = ext.to_ascii_lowercase();
        if FORBIDDEN_EXEC_EXTS.contains(&lower.as_str()) {
            return Err(CliError::Package(PackageError::ForbiddenExecutable(
                path.to_string(),
            )));
        }
    }
    Ok(())
}

#[derive(Default)]
struct PackagePathRegistry {
    file_entries: BTreeMap<String, Vec<u8>>,
    implied_dirs: BTreeSet<String>,
}

impl PackagePathRegistry {
    fn register(&mut self, raw_path: &str) -> Result<(), CliError> {
        validate_safe_package_path(raw_path)?;
        check_forbidden_exec_ext(raw_path)?;
        let canonical = raw_path.trim_end_matches('/');
        if self.file_entries.contains_key(canonical) {
            return Ok(());
        }
        self.file_entries.insert(canonical.to_string(), Vec::new());
        let mut cur = canonical;
        while let Some((parent, _)) = cur.rsplit_once('/') {
            if !parent.is_empty() {
                self.implied_dirs.insert(parent.to_string());
            }
            cur = parent;
        }
        Ok(())
    }

    fn verify(&self) -> Result<(), CliError> {
        build_deterministic_cmplugin_zip(&self.file_entries)?;
        for dir in &self.implied_dirs {
            if self.file_entries.contains_key(dir) {
                return Err(CliError::Package(PackageError::InvalidArchive(format!(
                    "path `{dir}` conflicts with directory entry"
                ))));
            }
            let mut probe = self.file_entries.clone();
            probe.insert(dir.clone(), Vec::new());
            if let Err(PackageError::PathCollision { path, existing }) =
                build_deterministic_cmplugin_zip(&probe)
            {
                let conflicting_file = if self.file_entries.contains_key(&path) {
                    path
                } else {
                    existing
                };
                return Err(CliError::Package(PackageError::InvalidArchive(format!(
                    "path `{conflicting_file}` conflicts with directory entry"
                ))));
            }
        }
        Ok(())
    }
}

fn validate_ui_declared_paths(ui: &plugin_package::UiManifest) -> Result<(), CliError> {
    check_forbidden_exec_ext(&ui.entry)?;
    check_forbidden_exec_ext(&ui.asset_manifest)?;
    if ui.asset_manifest.to_ascii_lowercase().ends_with(".wasm") {
        return Err(CliError::Package(PackageError::ForbiddenExecutable(
            format!("undeclared wasm component `{}`", ui.asset_manifest),
        )));
    }
    if ui.entry == ui.asset_manifest {
        return Err(CliError::Package(PackageError::InvalidUiManifest(format!(
            "ui.entry `{}` must not equal ui.asset_manifest `{}`",
            ui.entry, ui.asset_manifest
        ))));
    }
    if is_forbidden_ui_asset_path(&ui.asset_manifest) {
        return Err(CliError::Package(PackageError::ForbiddenUiAsset(
            ui.asset_manifest.clone(),
        )));
    }
    Ok(())
}

fn validate_manifest_declared_paths(
    manifest: &PluginManifest,
    registry: &mut PackagePathRegistry,
) -> Result<(), CliError> {
    registry.register("plugin.toml")?;
    check_non_ui_manifest_path(&manifest.config_schema, false)?;
    registry.register(&manifest.config_schema)?;

    for comp in &manifest.components {
        check_non_ui_manifest_path(&comp.entry, true)?;
        registry.register(&comp.entry)?;
        for task in &comp.tasks {
            check_non_ui_manifest_path(&task.input_schema, false)?;
            registry.register(&task.input_schema)?;
        }
        for ep in &comp.endpoints {
            check_non_ui_manifest_path(&ep.input_schema, false)?;
            registry.register(&ep.input_schema)?;
        }
        for idx in &comp.indexers {
            check_non_ui_manifest_path(&idx.site_schema, false)?;
            registry.register(&idx.site_schema)?;
        }
    }

    for site in &manifest.sites {
        check_non_ui_manifest_path(site, false)?;
        registry.register(site)?;
    }

    if let Some(ui) = &manifest.ui {
        validate_ui_declared_paths(ui)?;
        registry.register(&ui.asset_manifest)?;
        registry.register(&ui.entry)?;
    }

    registry.verify()
}

fn check_non_ui_manifest_path(path: &str, is_component_entry: bool) -> Result<(), CliError> {
    check_forbidden_exec_ext(path)?;
    let lower = path.to_ascii_lowercase();
    if lower.starts_with("ui/") {
        return Err(CliError::Package(PackageError::ForbiddenUiAsset(format!(
            "undeclared file `{path}` in ui/"
        ))));
    }
    if lower.ends_with(".js") || lower.ends_with(".mjs") || lower.ends_with(".cjs") {
        return Err(CliError::Package(PackageError::ForbiddenExecutable(
            format!("undeclared script `{path}`"),
        )));
    }
    if !is_component_entry && lower.ends_with(".wasm") {
        return Err(CliError::Package(PackageError::ForbiddenExecutable(
            format!("undeclared wasm component `{path}`"),
        )));
    }
    Ok(())
}

pub(crate) fn validate_schema_file(dir: &Path, rel_path: &str) -> Result<(), CliError> {
    check_non_ui_manifest_path(rel_path, false)?;
    let bytes = read_project_file_no_symlinks(dir, rel_path)?;
    if bytes.len() > MAX_SCHEMA_BYTES {
        return Err(CliError::Validation(format!(
            "schema `{rel_path}` exceeds 512 KiB limit"
        )));
    }
    let json: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| CliError::Validation(format!("invalid JSON schema `{rel_path}`: {e}")))?;
    validate_config_schema(&json)?;
    Ok(())
}

fn validate_components_in_dir(
    dir: &Path,
    manifest: &PluginManifest,
    require_built_artifacts: bool,
) -> Result<(), CliError> {
    for comp in &manifest.components {
        check_non_ui_manifest_path(&comp.entry, true)?;
        for task in &comp.tasks {
            validate_schema_file(dir, &task.input_schema)?;
        }
        for ep in &comp.endpoints {
            validate_schema_file(dir, &ep.input_schema)?;
        }
        for idx in &comp.indexers {
            validate_schema_file(dir, &idx.site_schema)?;
        }

        match read_project_file_no_symlinks_optional(dir, &comp.entry)? {
            Some(bytes) => {
                validate_wasm_component_bytes(&bytes, &comp.id, &comp.entry)?;
            }
            None if require_built_artifacts => {
                return Err(CliError::Validation(format!(
                    "built component `{}` missing at `{}`; run `plugin build` first",
                    comp.id, comp.entry
                )));
            }
            None if read_project_file_no_symlinks_optional(dir, "Cargo.toml")?.is_none() => {
                return Err(CliError::Validation(format!(
                    "component `{}` entry `{}` missing and no Cargo.toml present",
                    comp.id, comp.entry
                )));
            }
            None => {}
        }
    }
    Ok(())
}

pub(crate) fn validate_wasm_component_bytes(
    bytes: &[u8],
    comp_id: &str,
    entry: &str,
) -> Result<(), CliError> {
    if !wasmparser::Parser::is_component(bytes) {
        return Err(CliError::Package(PackageError::InvalidManifest(format!(
            "component `{comp_id}` entry `{entry}` is not a valid WebAssembly Component Model binary"
        ))));
    }
    wasmparser::Validator::new_with_features(wasmparser::WasmFeatures::default())
        .validate_all(bytes)
        .map_err(|e| {
            CliError::Package(PackageError::InvalidManifest(format!(
                "component `{comp_id}` entry `{entry}` failed WebAssembly component validation: {e}"
            )))
        })?;
    Ok(())
}

fn validate_sites_in_dir(dir: &Path, sites: &[String]) -> Result<(), CliError> {
    let mut seen_ids = HashSet::new();
    for rel in sites {
        check_non_ui_manifest_path(rel, false)?;
        let bytes = read_project_file_no_symlinks(dir, rel)?;
        let id = validate_site_profile_yaml(&bytes, rel)?;
        if !seen_ids.insert(id.clone()) {
            return Err(CliError::Validation(format!(
                "duplicate site profile id `{id}`"
            )));
        }
    }
    Ok(())
}

fn validate_ui_in_dir(
    dir: &Path,
    ui: &plugin_package::UiManifest,
    policy: &PackagePolicy,
    require_built_artifacts: bool,
    path_registry: &mut PackagePathRegistry,
) -> Result<(), CliError> {
    validate_ui_declared_paths(ui)?;
    let ui_dir = dir.join("ui");
    let ui_meta = fs::symlink_metadata(&ui_dir).map_err(|_| {
        CliError::Validation("manifest declares [ui] but `ui/` directory is missing".into())
    })?;
    if ui_meta.file_type().is_symlink() {
        return Err(CliError::Package(PackageError::UnsafePath {
            path: ui_dir.display().to_string(),
            reason: "symlinks are forbidden inside UI assets".into(),
        }));
    }
    if !ui_meta.is_dir() {
        return Err(CliError::Validation(
            "manifest declares [ui] but `ui/` directory is missing".into(),
        ));
    }

    let assets_dir = ui_dir.join("assets");
    let mut built_entries = BTreeMap::new();
    if let Ok(assets_meta) = fs::symlink_metadata(&assets_dir) {
        if assets_meta.file_type().is_symlink() {
            return Err(CliError::Package(PackageError::UnsafePath {
                path: assets_dir.display().to_string(),
                reason: "symlinks are forbidden inside UI assets".into(),
            }));
        }
        if assets_meta.is_dir() {
            collect_built_ui_assets(dir, &assets_dir, &mut built_entries)?;
        }
    }

    for rel in built_entries.keys() {
        path_registry.register(rel)?;
    }

    match read_project_file_no_symlinks_optional(dir, &ui.asset_manifest)? {
        Some(manifest_bytes) => {
            let json: serde_json::Value = serde_json::from_slice(&manifest_bytes).map_err(|e| {
                CliError::Package(PackageError::InvalidUiManifest(format!(
                    "invalid `{}`: {e}",
                    ui.asset_manifest
                )))
            })?;
            collect_declared_extra_ui_assets(
                dir,
                &ui.asset_manifest,
                &json,
                &mut built_entries,
                path_registry,
            )?;
            path_registry.verify()?;

            let mut entry_metas: Vec<ArchiveEntryMeta> = built_entries
                .iter()
                .map(|(rel, bytes)| ArchiveEntryMeta {
                    path: rel.clone(),
                    size: bytes.len() as u64,
                    sha256_hex: sha256_hex(bytes),
                })
                .collect();
            if !built_entries.contains_key(&ui.asset_manifest) {
                entry_metas.push(ArchiveEntryMeta {
                    path: ui.asset_manifest.clone(),
                    size: manifest_bytes.len() as u64,
                    sha256_hex: sha256_hex(&manifest_bytes),
                });
            }

            validate_ui_manifest_with_policy(&json, Some(ui), &entry_metas, policy)?;
        }
        None if require_built_artifacts => {
            return Err(CliError::Validation(format!(
                "built UI manifest `{}` is missing; run `plugin build` first",
                ui.asset_manifest
            )));
        }
        None if read_project_file_no_symlinks_optional(dir, "ui/package.json")?.is_none() => {
            return Err(CliError::Validation(
                "UI directory lacks both built `ui/asset-manifest.json` and `ui/package.json`"
                    .into(),
            ));
        }
        None => {
            path_registry.verify()?;
        }
    }

    Ok(())
}

fn collect_declared_extra_ui_assets(
    project_root: &Path,
    asset_manifest_rel: &str,
    json: &serde_json::Value,
    built_entries: &mut BTreeMap<String, Vec<u8>>,
    path_registry: &mut PackagePathRegistry,
) -> Result<(), CliError> {
    let Some(files) = json.get("files").and_then(|v| v.as_array()) else {
        return Ok(());
    };
    for item in files {
        let Some(rel) = item.get("path").and_then(|v| v.as_str()) else {
            continue;
        };
        if rel == asset_manifest_rel {
            continue;
        }
        validate_safe_package_path(rel)?;
        check_forbidden_exec_ext(rel)?;
        if !rel.starts_with("ui/") {
            continue;
        }
        if is_forbidden_ui_asset_path(rel) {
            return Err(CliError::Package(PackageError::ForbiddenUiAsset(
                rel.to_string(),
            )));
        }
        path_registry.register(rel)?;
        if !built_entries.contains_key(rel)
            && let Some(bytes) = read_project_file_no_symlinks_optional(project_root, rel)?
        {
            built_entries.insert(rel.to_string(), bytes);
        }
    }
    Ok(())
}

pub(crate) fn collect_built_ui_assets(
    project_root: &Path,
    dir: &Path,
    out: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(), CliError> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            return Err(CliError::Package(PackageError::UnsafePath {
                path: path.display().to_string(),
                reason: "symlinks are forbidden inside UI assets".into(),
            }));
        }
        if meta.is_dir() {
            collect_built_ui_assets(project_root, &path, out)?;
            continue;
        }
        let rel = path
            .strip_prefix(project_root)
            .map_err(|_| CliError::Validation("UI asset path escapes project root".into()))?
            .to_string_lossy()
            .replace('\\', "/");
        if rel.ends_with(".DS_Store") {
            continue;
        }
        validate_safe_package_path(&rel)?;
        check_forbidden_exec_ext(&rel)?;
        if is_forbidden_ui_asset_path(&rel) {
            return Err(CliError::Package(PackageError::ForbiddenUiAsset(rel)));
        }
        let bytes = fs::read(&path)?;
        out.insert(rel, bytes);
    }
    Ok(())
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}
