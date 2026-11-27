use std::path::{Component, Path, PathBuf};

use plugin_package::{UiAssetManifest, UiManifest};
use sha2::{Digest, Sha256};
use store::plugins::PluginVersionMetadata;

use super::staging::hex_lower;
use super::types::PluginServiceError;
use super::ui_builtins::builtin_bundle_sha256;
use super::ui_builtins_assets::builtin_asset_manifest_bytes;

fn package_root_dir(data_dir: &Path, version: &PluginVersionMetadata) -> PathBuf {
    let artifact = Path::new(&version.artifact_path);
    if artifact.is_absolute() {
        artifact.to_path_buf()
    } else {
        data_dir.join(artifact)
    }
}

fn expected_ui_metadata_sha256(version: &PluginVersionMetadata) -> Option<&str> {
    version
        .ui_metadata_ref
        .as_deref()
        .and_then(|r| r.rsplit_once(':'))
        .map(|(_, h)| h.trim())
        .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

async fn verify_manifest_bytes_against_version(
    pkg_root: &Path,
    version: &PluginVersionMetadata,
    manifest_bytes: &[u8],
    expected_hex: &str,
) -> Result<(), PluginServiceError> {
    if hex_lower(&Sha256::digest(manifest_bytes)).eq_ignore_ascii_case(expected_hex) {
        return Ok(());
    }
    let Ok(parsed) = serde_json::from_slice::<UiAssetManifest>(manifest_bytes) else {
        return Err(PluginServiceError::forbidden(
            "plugin.ui_asset_corrupted",
            "UI asset manifest failed SHA-256 verification",
        ));
    };
    if serde_json::to_vec(&parsed).ok().as_deref() != Some(manifest_bytes) {
        return Err(PluginServiceError::forbidden(
            "plugin.ui_asset_corrupted",
            "UI asset manifest failed SHA-256 verification",
        ));
    }
    if let Ok(canon_val) = serde_json::to_value(&parsed)
        && let Ok(canon_bytes) = serde_json::to_vec(&canon_val)
        && hex_lower(&Sha256::digest(&canon_bytes)).eq_ignore_ascii_case(expected_hex)
    {
        return Ok(());
    }
    if let Ok(archive_bytes) = tokio::fs::read(pkg_root.join("package.cmplugin")).await
        && hex_lower(&Sha256::digest(&archive_bytes)).eq_ignore_ascii_case(&version.package_hash)
    {
        let policy = plugin_package::PackagePolicy {
            dev_mode: true,
            ..Default::default()
        };
        if let Ok(validated) = plugin_package::validate_cmplugin_bytes(&archive_bytes, &policy)
            && let Some(ui_bundle) = validated.ui
            && ui_bundle
                .ui_metadata_ref
                .rsplit_once(':')
                .is_some_and(|(_, h)| h.eq_ignore_ascii_case(expected_hex))
            && ui_bundle.asset_manifest == parsed
        {
            return Ok(());
        }
    }
    Err(PluginServiceError::forbidden(
        "plugin.ui_asset_corrupted",
        "UI asset manifest failed SHA-256 verification",
    ))
}

async fn read_verified_ui_manifest_bytes(
    pkg_root: &Path,
    version: &PluginVersionMetadata,
    ui: &UiManifest,
) -> Result<Option<Vec<u8>>, PluginServiceError> {
    let manifest_rel = validate_clean_rel_path(&ui.asset_manifest)?;
    if !manifest_rel.starts_with("ui/") || manifest_rel == Path::new("ui") {
        return Err(PluginServiceError::forbidden(
            "plugin.invalid_asset_path",
            "UI asset manifest must reside inside ui/",
        ));
    }
    ensure_no_symlink_components_async(pkg_root, &manifest_rel).await?;
    let ui_root = match tokio::fs::canonicalize(pkg_root.join("ui")).await {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(PluginServiceError::Io(e.to_string())),
    };
    let canonical_manifest = match tokio::fs::canonicalize(pkg_root.join(&manifest_rel)).await {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(PluginServiceError::Io(e.to_string())),
    };
    if !canonical_manifest.starts_with(&ui_root) {
        return Err(PluginServiceError::forbidden(
            "plugin.invalid_asset_path",
            "UI asset manifest escapes bundle root",
        ));
    }
    let manifest_bytes = tokio::fs::read(&canonical_manifest).await.map_err(|_| {
        PluginServiceError::not_found("plugin.asset_not_found", "UI asset manifest unreadable")
    })?;
    if let Some(expected_hex) = expected_ui_metadata_sha256(version) {
        verify_manifest_bytes_against_version(pkg_root, version, &manifest_bytes, expected_hex)
            .await?;
    }
    Ok(Some(manifest_bytes))
}

pub(crate) async fn resolve_bundle_sha256(
    data_dir: &Path,
    version: &PluginVersionMetadata,
    ui: &UiManifest,
) -> Result<String, PluginServiceError> {
    if version.signature_status == "builtin" {
        return Ok(builtin_bundle_sha256(&version.plugin_id)
            .unwrap_or_else(|| version.package_hash.clone()));
    }
    let pkg_root = package_root_dir(data_dir, version);
    if let Some(bytes) = read_verified_ui_manifest_bytes(&pkg_root, version, ui).await? {
        return Ok(hex_lower(&Sha256::digest(&bytes)));
    }
    if let Some(hash) = expected_ui_metadata_sha256(version) {
        return Ok(hash.to_ascii_lowercase());
    }
    Err(PluginServiceError::not_found(
        "plugin.asset_not_found",
        "UI asset manifest missing",
    ))
}

fn extract_manifest_styles(bytes: &[u8]) -> Vec<String> {
    let Ok(manifest) = serde_json::from_slice::<UiAssetManifest>(bytes) else {
        return Vec::new();
    };
    manifest
        .files
        .into_iter()
        .filter(|f| {
            f.path.to_ascii_lowercase().ends_with(".css")
                && validate_asset_request_path(&f.path, &f.path).is_ok()
        })
        .map(|f| f.path)
        .collect()
}

pub(super) async fn resolve_ui_styles(
    data_dir: &Path,
    version: &PluginVersionMetadata,
    ui: &UiManifest,
) -> Result<Vec<String>, PluginServiceError> {
    if version.signature_status == "builtin" {
        return Ok(builtin_asset_manifest_bytes(&version.plugin_id)
            .map(extract_manifest_styles)
            .unwrap_or_default());
    }
    let pkg_root = package_root_dir(data_dir, version);
    Ok(read_verified_ui_manifest_bytes(&pkg_root, version, ui)
        .await?
        .map(|bytes| extract_manifest_styles(&bytes))
        .unwrap_or_default())
}

fn validate_clean_rel_path(rel: &str) -> Result<PathBuf, PluginServiceError> {
    if rel.is_empty() || rel.contains('\0') || rel.contains('\\') || rel.starts_with('/') {
        return Err(PluginServiceError::invalid(
            "plugin.invalid_asset_path",
            "unsafe UI asset path",
        ));
    }
    let mut out = PathBuf::new();
    for comp in Path::new(rel).components() {
        match comp {
            Component::Normal(part) => out.push(part),
            _ => {
                return Err(PluginServiceError::invalid(
                    "plugin.invalid_asset_path",
                    "unsafe UI asset path segment",
                ));
            }
        }
    }
    Ok(out)
}

pub(crate) fn validate_asset_request_path(
    raw_uri_path: &str,
    asset_path: &str,
) -> Result<PathBuf, PluginServiceError> {
    let lower_raw = raw_uri_path.to_ascii_lowercase();
    if lower_raw.contains("%2e") || lower_raw.contains("%5c") || raw_uri_path.contains('\\') {
        return Err(PluginServiceError::forbidden(
            "plugin.invalid_asset_path",
            "path traversal forbidden",
        ));
    }
    let clean = validate_clean_rel_path(asset_path)?;
    if !asset_path.starts_with("ui/") || asset_path == "ui/asset-manifest.json" {
        return Err(PluginServiceError::forbidden(
            "plugin.invalid_asset_path",
            "requested path is outside allowed UI assets",
        ));
    }
    let lower = asset_path.to_ascii_lowercase();
    if [
        ".html", ".htm", ".map", ".ts", ".tsx", ".jsx", ".wasm", ".toml",
    ]
    .iter()
    .any(|ext| lower.ends_with(ext))
    {
        return Err(PluginServiceError::forbidden(
            "plugin.invalid_asset_path",
            "forbidden UI asset extension",
        ));
    }
    Ok(clean)
}

pub(super) async fn read_external_ui_asset_off_lock(
    data_dir: &Path,
    version: &PluginVersionMetadata,
    ui: &UiManifest,
    expected_bundle_sha256: Option<&str>,
    asset_path: &str,
    clean_rel: &Path,
) -> Result<(Vec<u8>, String), PluginServiceError> {
    let Some(exp_sha) = expected_bundle_sha256
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Err(PluginServiceError::forbidden(
            "plugin.ui_asset_corrupted",
            "UI asset manifest missing trusted SHA-256",
        ));
    };
    let pkg_root = package_root_dir(data_dir, version);
    let Some(manifest_bytes) = read_verified_ui_manifest_bytes(&pkg_root, version, ui).await?
    else {
        return Err(PluginServiceError::not_found(
            "plugin.asset_not_found",
            "UI asset manifest missing",
        ));
    };
    if !hex_lower(&Sha256::digest(&manifest_bytes)).eq_ignore_ascii_case(exp_sha) {
        return Err(PluginServiceError::forbidden(
            "plugin.ui_asset_corrupted",
            "UI asset manifest failed SHA-256 verification",
        ));
    }
    let asset_manifest: UiAssetManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|_| {
            PluginServiceError::invalid("plugin.invalid_asset_path", "corrupt UI asset manifest")
        })?;
    let declared = asset_manifest
        .files
        .iter()
        .find(|f| f.path == asset_path)
        .ok_or_else(|| {
            PluginServiceError::not_found("plugin.asset_not_found", "UI asset not declared")
        })?;

    let ui_root = tokio::fs::canonicalize(pkg_root.join("ui"))
        .await
        .map_err(|_| PluginServiceError::not_found("plugin.asset_not_found", "UI dir missing"))?;
    ensure_no_symlink_components_async(&pkg_root, clean_rel).await?;
    let canonical_target = tokio::fs::canonicalize(pkg_root.join(clean_rel))
        .await
        .map_err(|_| PluginServiceError::not_found("plugin.asset_not_found", "UI asset missing"))?;
    if !canonical_target.starts_with(&ui_root) {
        return Err(PluginServiceError::forbidden(
            "plugin.invalid_asset_path",
            "UI asset escapes bundle root",
        ));
    }
    let bytes = tokio::fs::read(&canonical_target).await.map_err(|_| {
        PluginServiceError::not_found("plugin.asset_not_found", "UI asset unreadable")
    })?;
    if bytes.len() as u64 != declared.size
        || !hex_lower(&Sha256::digest(&bytes)).eq_ignore_ascii_case(declared.sha256.trim())
    {
        return Err(PluginServiceError::forbidden(
            "plugin.ui_asset_corrupted",
            "UI asset failed SHA-256 or size verification",
        ));
    }
    Ok((bytes, canonical_mime_header(&declared.content_type)))
}

async fn ensure_no_symlink_components_async(
    root: &Path,
    rel: &Path,
) -> Result<(), PluginServiceError> {
    let mut cur = root.to_path_buf();
    for comp in rel.components() {
        cur.push(comp);
        if let Ok(meta) = tokio::fs::symlink_metadata(&cur).await
            && meta.file_type().is_symlink()
        {
            return Err(PluginServiceError::forbidden(
                "plugin.invalid_asset_path",
                "symlink forbidden inside UI bundle",
            ));
        }
    }
    Ok(())
}

fn canonical_mime_header(mime: &str) -> String {
    match mime {
        "application/javascript" | "text/javascript" => {
            "application/javascript; charset=utf-8".into()
        }
        "text/css" => "text/css; charset=utf-8".into(),
        "application/json" => "application/json; charset=utf-8".into(),
        other => other.to_string(),
    }
}
