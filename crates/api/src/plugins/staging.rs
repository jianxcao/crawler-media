use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use domain::UserId;
use parking_lot::Mutex;
use plugin_package::{
    ArchiveLimits, PackageError, PackagePolicy, SignatureStatus, TrustedKey, ValidatedPackage,
    validate_cmplugin_bytes,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use store::Store;
use store::plugins::{PluginTrustKeyStatus, PluginVersionMetadata};
use uuid::Uuid;

use super::reauth::now_unix;
use super::types::{PluginServiceError, StagedUploadView, format_rfc3339, map_package_err};

const UPLOAD_TTL_SECS: i64 = 3600;
const MAX_SIGNATURE_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub(crate) struct ActiveStagingPins {
    counts: Mutex<HashMap<String, usize>>,
}

impl ActiveStagingPins {
    pub(crate) fn pin(self: &Arc<Self>, package_hash: &str) -> StagingPinGuard {
        let mut guard = self.counts.lock();
        *guard.entry(package_hash.to_string()).or_default() += 1;
        StagingPinGuard {
            pins: Arc::clone(self),
            package_hash: package_hash.to_string(),
        }
    }

    pub(crate) fn snapshot(&self) -> HashSet<String> {
        self.counts.lock().keys().cloned().collect()
    }

    pub(crate) fn is_pinned(&self, package_hash: &str) -> bool {
        self.counts.lock().contains_key(package_hash)
    }
}

pub(crate) struct StagingPinGuard {
    pins: Arc<ActiveStagingPins>,
    package_hash: String,
}

impl Drop for StagingPinGuard {
    fn drop(&mut self) {
        let mut guard = self.pins.counts.lock();
        if let Some(count) = guard.get_mut(&self.package_hash) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                guard.remove(&self.package_hash);
            }
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct StagedUploadMeta {
    pub upload_id: Uuid,
    pub actor_user_id: UserId,
    pub package_hash: String,
    pub signature_status: String,
    pub signing_key_id: Option<String>,
    pub fingerprint: Option<String>,
    pub has_sig: bool,
    pub created_at: i64,
    pub expires_at: i64,
}

pub(crate) fn plugins_root(data_dir: &Path) -> PathBuf {
    data_dir.join("plugins")
}

pub(crate) fn uploads_root(data_dir: &Path) -> PathBuf {
    plugins_root(data_dir).join("uploads")
}

pub(crate) fn packages_root(data_dir: &Path) -> PathBuf {
    plugins_root(data_dir).join("packages")
}

pub(crate) fn map_signature_status(status: &SignatureStatus) -> (&'static str, Option<String>) {
    match status {
        SignatureStatus::Trusted { key_id, .. } => ("signed", Some(key_id.clone())),
        SignatureStatus::UnsignedDevMode => ("development", None),
    }
}

pub(crate) fn signature_fingerprint(status: &SignatureStatus) -> Option<String> {
    match status {
        SignatureStatus::Trusted { fingerprint, .. } => Some(fingerprint.clone()),
        SignatureStatus::UnsignedDevMode => None,
    }
}

pub(crate) fn build_package_policy_from_store(
    store: &Store,
    detached_signature: Option<String>,
) -> Result<PackagePolicy, PluginServiceError> {
    let dev_mode = store.plugin_developer_mode()?.enabled;
    let mut trust_keys = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let page = store.list_plugin_trust_keys(after.as_deref(), Some(200))?;
        for item in &page.items {
            trust_keys.push(TrustedKey {
                key_id: item.key.key_id.clone(),
                public_key: item.key.public_key.clone(),
                revoked: item.key.status == PluginTrustKeyStatus::Revoked,
            });
        }
        match page.next_cursor {
            Some(next) if Some(&next) != after.as_ref() => after = Some(next),
            _ => break,
        }
    }
    Ok(PackagePolicy {
        dev_mode,
        detached_signature,
        trust_keys,
        ..PackagePolicy::default()
    })
}

pub(crate) fn safe_rel_path(root: &Path, rel: &str) -> Result<PathBuf, PluginServiceError> {
    let rel_path = Path::new(rel);
    if rel.is_empty() || rel_path.is_absolute() {
        return Err(PluginServiceError::invalid(
            "plugin.invalid_package",
            format!("unsafe package path: {rel}"),
        ));
    }
    let mut out = root.to_path_buf();
    for comp in rel_path.components() {
        match comp {
            Component::Normal(part) => out.push(part),
            _ => {
                return Err(PluginServiceError::invalid(
                    "plugin.invalid_package",
                    format!("unsafe package path component in `{rel}`"),
                ));
            }
        }
    }
    Ok(out)
}

fn ensure_no_symlink_components(root: &Path, target: &Path) -> Result<(), PluginServiceError> {
    let Ok(rel) = target.strip_prefix(root) else {
        return Err(PluginServiceError::invalid(
            "plugin.invalid_package",
            "path escapes package root",
        ));
    };
    let mut cur = root.to_path_buf();
    if let Ok(meta) = fs::symlink_metadata(&cur)
        && meta.file_type().is_symlink()
    {
        return Err(PluginServiceError::invalid(
            "plugin.invalid_package",
            "package root must not be a symlink",
        ));
    }
    for comp in rel.components() {
        cur.push(comp);
        if let Ok(meta) = fs::symlink_metadata(&cur)
            && meta.file_type().is_symlink()
        {
            return Err(PluginServiceError::invalid(
                "plugin.invalid_package",
                "symlink forbidden inside package directory",
            ));
        }
    }
    Ok(())
}

pub(crate) fn stage_upload_sync(
    data_dir: &Path,
    actor_user_id: UserId,
    upload_id: Uuid,
    cmplugin_bytes: &[u8],
    signature_text: Option<&str>,
    expected_sha256: Option<&str>,
    policy: &PackagePolicy,
) -> Result<StagedUploadView, PluginServiceError> {
    cleanup_expired_uploads_sync(data_dir, now_unix());

    if let Some(sig) = signature_text
        && sig.len() > MAX_SIGNATURE_BYTES
    {
        return Err(PluginServiceError::PayloadTooLarge {
            code: "plugin.signature_too_large",
            message: "signature exceeds 64 KiB limit".into(),
        });
    }
    let limits = ArchiveLimits::default();
    if (cmplugin_bytes.len() as u64) > limits.max_package_bytes {
        return Err(map_package_err(PackageError::LimitExceeded(
            "package exceeds 64 MiB limit".into(),
        )));
    }
    if let Some(exp) = expected_sha256 {
        let actual_hex = hex_lower(&Sha256::digest(cmplugin_bytes));
        if actual_hex != exp.to_ascii_lowercase() {
            return Err(PluginServiceError::invalid(
                "plugin.invalid_package",
                "uploaded package sha256 mismatch",
            ));
        }
    }
    let validated = validate_cmplugin_bytes(cmplugin_bytes, policy).map_err(map_package_err)?;

    let upload_dir = uploads_root(data_dir).join(upload_id.to_string());
    if upload_dir.exists() {
        return Err(PluginServiceError::conflict(
            "plugin.conflict",
            "upload_id already exists",
        ));
    }
    fs::create_dir_all(uploads_root(data_dir))
        .map_err(|e| PluginServiceError::Io(e.to_string()))?;
    let tmp_dir = uploads_root(data_dir).join(format!(".tmp-{upload_id}-{}", Uuid::new_v4()));
    fs::create_dir_all(&tmp_dir).map_err(|e| PluginServiceError::Io(e.to_string()))?;

    let now = now_unix();
    let expires_at = now.saturating_add(UPLOAD_TTL_SECS);
    let (sig_status, signing_key_id) = map_signature_status(&validated.signature.status);
    let fingerprint = signature_fingerprint(&validated.signature.status);
    let meta = StagedUploadMeta {
        upload_id,
        actor_user_id,
        package_hash: validated.package_sha256_hex.clone(),
        signature_status: sig_status.to_string(),
        signing_key_id: signing_key_id.clone(),
        fingerprint: fingerprint.clone(),
        has_sig: signature_text.is_some(),
        created_at: now,
        expires_at,
    };

    let write_res = (|| -> Result<(), PluginServiceError> {
        fs::write(tmp_dir.join("package.cmplugin"), cmplugin_bytes)
            .map_err(|e| PluginServiceError::Io(e.to_string()))?;
        if let Some(sig) = signature_text {
            fs::write(tmp_dir.join("package.minisig"), sig.as_bytes())
                .map_err(|e| PluginServiceError::Io(e.to_string()))?;
        }
        let meta_bytes =
            serde_json::to_vec(&meta).map_err(|e| PluginServiceError::Io(e.to_string()))?;
        fs::write(tmp_dir.join("meta.json"), meta_bytes)
            .map_err(|e| PluginServiceError::Io(e.to_string()))?;
        Ok(())
    })();
    if let Err(err) = write_res {
        let _ = fs::remove_dir_all(&tmp_dir);
        return Err(err);
    }
    if upload_dir.exists() {
        let _ = fs::remove_dir_all(&tmp_dir);
        return Err(PluginServiceError::conflict(
            "plugin.conflict",
            "upload_id already exists",
        ));
    }
    if let Err(err) = fs::rename(&tmp_dir, &upload_dir) {
        let _ = fs::remove_dir_all(&tmp_dir);
        if upload_dir.exists() {
            return Err(PluginServiceError::conflict(
                "plugin.conflict",
                "upload_id already exists",
            ));
        }
        return Err(PluginServiceError::Io(err.to_string()));
    }

    Ok(StagedUploadView {
        upload_id,
        hash: validated.package_sha256_hex,
        signature_status: sig_status.to_string(),
        signing_key_id,
        fingerprint,
        expires_at: format_rfc3339(expires_at),
    })
}

pub(crate) fn read_staged_upload_sync(
    data_dir: &Path,
    upload_id: Uuid,
) -> Result<(Vec<u8>, Option<String>, StagedUploadMeta), PluginServiceError> {
    let dir = uploads_root(data_dir).join(upload_id.to_string());
    if !dir.exists() {
        return Err(PluginServiceError::not_found(
            "plugin.upload_missing",
            "staged upload not found",
        ));
    }
    ensure_no_symlink_components(&uploads_root(data_dir), &dir)?;
    let meta_bytes = fs::read(dir.join("meta.json")).map_err(|_| {
        PluginServiceError::not_found("plugin.upload_missing", "staged upload metadata missing")
    })?;
    let meta: StagedUploadMeta = serde_json::from_slice(&meta_bytes)
        .map_err(|e| PluginServiceError::Io(format!("corrupt staged upload metadata: {e}")))?;
    if meta.expires_at < now_unix() {
        let _ = fs::remove_dir_all(&dir);
        return Err(PluginServiceError::not_found(
            "plugin.upload_expired",
            "staged upload has expired",
        ));
    }
    let pkg_bytes = fs::read(dir.join("package.cmplugin")).map_err(|_| {
        PluginServiceError::not_found("plugin.upload_missing", "staged package archive missing")
    })?;
    let sig_path = dir.join("package.minisig");
    let sig_text = if sig_path.exists() {
        Some(fs::read_to_string(&sig_path).map_err(|e| PluginServiceError::Io(e.to_string()))?)
    } else {
        None
    };
    Ok((pkg_bytes, sig_text, meta))
}

pub(crate) fn read_staged_upload_for_actor_sync(
    data_dir: &Path,
    upload_id: Uuid,
    expected_actor: Option<UserId>,
) -> Result<(Vec<u8>, Option<String>, StagedUploadMeta), PluginServiceError> {
    let (pkg_bytes, sig_text, meta) = read_staged_upload_sync(data_dir, upload_id)?;
    if let Some(actor) = expected_actor
        && meta.actor_user_id != actor
    {
        return Err(PluginServiceError::forbidden(
            "plugin.forbidden",
            "staged upload belongs to a different user",
        ));
    }
    Ok((pkg_bytes, sig_text, meta))
}

pub(crate) fn materialize_immutable_package_sync(
    data_dir: &Path,
    raw_cmplugin: &[u8],
    detached_sig: Option<&str>,
    validated: &ValidatedPackage,
) -> Result<String, PluginServiceError> {
    let hash = &validated.package_sha256_hex;
    let rel = format!("plugins/packages/{hash}");
    let root = packages_root(data_dir);
    let target_dir = root.join(hash);
    if target_dir.exists() {
        verify_existing_package_dir(&target_dir, hash)?;
        return Ok(rel);
    }

    fs::create_dir_all(&root).map_err(|e| PluginServiceError::Io(e.to_string()))?;
    let tmp_dir = root.join(format!(".tmp-{hash}-{}", Uuid::new_v4()));
    fs::create_dir_all(&tmp_dir).map_err(|e| PluginServiceError::Io(e.to_string()))?;

    let write_res = (|| -> Result<(), PluginServiceError> {
        let task_schemas = collect_task_schema_entries(validated);
        write_entries(&tmp_dir, &validated.component_bytes)?;
        write_entries(&tmp_dir, &validated.site_yaml_bytes)?;
        write_entries(&tmp_dir, &validated.ui_asset_bytes)?;
        if let Some(ui_bundle) = &validated.ui {
            let manifest_dest = safe_rel_path(&tmp_dir, &ui_bundle.declaration.asset_manifest)?;
            ensure_no_symlink_components(&tmp_dir, &manifest_dest)?;
            if let Some(parent) = manifest_dest.parent() {
                fs::create_dir_all(parent).map_err(|e| PluginServiceError::Io(e.to_string()))?;
            }
            let manifest_bytes = serde_json::to_vec(&ui_bundle.asset_manifest)
                .map_err(|e| PluginServiceError::Io(e.to_string()))?;
            fs::write(&manifest_dest, manifest_bytes)
                .map_err(|e| PluginServiceError::Io(e.to_string()))?;
        }
        write_entries(&tmp_dir, &task_schemas)?;
        fs::write(tmp_dir.join("package.cmplugin"), raw_cmplugin)
            .map_err(|e| PluginServiceError::Io(e.to_string()))?;
        if let Some(sig) = detached_sig {
            fs::write(tmp_dir.join("package.minisig"), sig.as_bytes())
                .map_err(|e| PluginServiceError::Io(e.to_string()))?;
        }
        fs::write(
            tmp_dir.join("manifest.json"),
            validated.manifest_json.as_bytes(),
        )
        .map_err(|e| PluginServiceError::Io(e.to_string()))?;
        fs::write(
            tmp_dir.join("config.schema.json"),
            validated.config_schema_json.as_bytes(),
        )
        .map_err(|e| PluginServiceError::Io(e.to_string()))?;
        Ok(())
    })();

    if let Err(err) = write_res {
        let _ = fs::remove_dir_all(&tmp_dir);
        return Err(err);
    }

    if target_dir.exists() {
        let _ = fs::remove_dir_all(&tmp_dir);
        verify_existing_package_dir(&target_dir, hash)?;
        return Ok(rel);
    }
    if let Err(err) = fs::rename(&tmp_dir, &target_dir) {
        let _ = fs::remove_dir_all(&tmp_dir);
        if target_dir.exists() {
            verify_existing_package_dir(&target_dir, hash)?;
            return Ok(rel);
        }
        return Err(PluginServiceError::Io(err.to_string()));
    }
    Ok(rel)
}

fn collect_task_schema_entries(
    validated: &ValidatedPackage,
) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut out = std::collections::BTreeMap::new();
    for (comp_decl, comp_val) in validated
        .manifest
        .components
        .iter()
        .zip(validated.components.iter())
    {
        for task in &comp_decl.tasks {
            if let Some(schema) = comp_val.task_input_schemas.get(&task.id) {
                out.insert(
                    task.input_schema.clone(),
                    schema.canonical_json().as_bytes().to_vec(),
                );
            }
        }
    }
    out
}

const RESERVED_ROOT_FILES: &[&str] = &[
    "package.cmplugin",
    "package.minisig",
    "manifest.json",
    "config.schema.json",
];

fn write_entries(
    root: &Path,
    entries: &std::collections::BTreeMap<String, Vec<u8>>,
) -> Result<(), PluginServiceError> {
    for (rel, bytes) in entries {
        let dest = safe_rel_path(root, rel)?;
        if dest.parent() == Some(root)
            && dest
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| RESERVED_ROOT_FILES.contains(&n))
        {
            return Err(PluginServiceError::invalid(
                "plugin.invalid_package",
                format!("archive entry `{rel}` conflicts with reserved package metadata file"),
            ));
        }
        ensure_no_symlink_components(root, &dest)?;
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| PluginServiceError::Io(e.to_string()))?;
        }
        fs::write(&dest, bytes).map_err(|e| PluginServiceError::Io(e.to_string()))?;
    }
    Ok(())
}

fn verify_existing_package_dir(dir: &Path, expected_hash: &str) -> Result<(), PluginServiceError> {
    let pkg = dir.join("package.cmplugin");
    let bytes = fs::read(&pkg).map_err(|e| {
        PluginServiceError::Io(format!("existing package missing package.cmplugin: {e}"))
    })?;
    let actual_hex = hex_lower(&Sha256::digest(&bytes));
    if actual_hex != expected_hash {
        return Err(PluginServiceError::conflict(
            "plugin.package_conflict",
            "stored package archive digest mismatch",
        ));
    }
    Ok(())
}

pub(crate) fn read_packaged_archive_for_version(
    data_dir: &Path,
    version: &PluginVersionMetadata,
) -> Result<(Vec<u8>, Option<String>), PluginServiceError> {
    let artifact = Path::new(&version.artifact_path);
    let full = if artifact.is_absolute() {
        artifact.to_path_buf()
    } else {
        data_dir.join(artifact)
    };
    let archive_path = if full.is_dir() {
        full.join("package.cmplugin")
    } else {
        full.clone()
    };
    let bytes = fs::read(&archive_path).map_err(|e| {
        tracing::warn!(
            plugin_id = %version.plugin_id,
            package_hash = %version.package_hash,
            archive_path = %archive_path.display(),
            error = %e,
            "plugin package archive unavailable"
        );
        PluginServiceError::not_found("plugin.artifact_missing", "package archive unavailable")
    })?;
    let sig_path = if full.is_dir() {
        full.join("package.minisig")
    } else {
        full.with_extension("minisig")
    };
    let sig = if sig_path.exists() {
        Some(fs::read_to_string(&sig_path).map_err(|e| PluginServiceError::Io(e.to_string()))?)
    } else {
        None
    };
    Ok((bytes, sig))
}

pub(crate) fn version_metadata_from_validated(
    validated: &ValidatedPackage,
    artifact_rel: String,
) -> PluginVersionMetadata {
    let (sig_status, signing_key_id) = map_signature_status(&validated.signature.status);
    PluginVersionMetadata {
        plugin_id: validated.manifest.id.clone(),
        package_hash: validated.package_sha256_hex.clone(),
        version: validated.manifest.version.clone(),
        manifest_json: validated.manifest_json.clone(),
        config_schema_json: validated.config_schema_json.clone(),
        state_schema: validated.state_schema.to_string(),
        artifact_path: artifact_rel,
        signing_key_id,
        signature_status: sig_status.to_string(),
        ui_metadata_ref: validated.ui.as_ref().map(|u| u.ui_metadata_ref.clone()),
        credential_bound: validated.credential_bound,
    }
}

fn is_tmp_entry_expired(path: &Path, now: i64) -> bool {
    let Ok(md) = fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = md.modified() else {
        return false;
    };
    let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) else {
        return false;
    };
    let Ok(mtime) = i64::try_from(dur.as_secs()) else {
        return false;
    };
    now.saturating_sub(mtime) > UPLOAD_TTL_SECS
}

fn extract_tmp_package_hash(name: &str) -> Option<&str> {
    let rest = name.strip_prefix(".tmp-")?;
    if rest.len() <= 37 {
        return None;
    }
    let split_at = rest.len() - 37;
    if rest.as_bytes().get(split_at) == Some(&b'-') {
        Some(&rest[..split_at])
    } else {
        rest.split('-').next()
    }
}

pub(crate) fn cleanup_expired_uploads_sync(data_dir: &Path, now: i64) {
    let root = uploads_root(data_dir);
    let Ok(entries) = fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with(".tmp-") {
            if is_tmp_entry_expired(&path, now) {
                let _ = fs::remove_dir_all(&path);
            }
            continue;
        }
        let meta_path = path.join("meta.json");
        let expired = match fs::read(&meta_path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<StagedUploadMeta>(&raw).ok())
        {
            Some(meta) => meta.expires_at < now,
            None => is_tmp_entry_expired(&path, now),
        };
        if expired {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

pub(crate) fn gc_unreferenced_artifacts_with_store(
    store: &Store,
    pins: &ActiveStagingPins,
) -> Result<usize, PluginServiceError> {
    let data_dir = store.data_dir();
    let now = now_unix();
    cleanup_expired_uploads_sync(data_dir, now);
    let root = packages_root(data_dir);
    let Ok(entries) = fs::read_dir(&root) else {
        return Ok(0);
    };
    let candidates: Vec<(PathBuf, String)> = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if !p.is_dir() {
                return None;
            }
            let name = p.file_name()?.to_str()?.to_string();
            Some((p, name))
        })
        .collect();
    // Snapshot pins BEFORE querying DB references: any concurrent installer pins BEFORE
    // materializing `packages/H` and unpins ONLY AFTER committing `H` to SQLite.
    let pinned_before = pins.snapshot();
    let referenced_after = store.referenced_plugin_package_hashes()?;
    let mut removed = 0usize;
    for (path, name) in candidates {
        if name.starts_with(".tmp-") {
            let hash_pinned = extract_tmp_package_hash(&name)
                .is_some_and(|h| pinned_before.contains(h) || pins.is_pinned(h));
            if !hash_pinned && is_tmp_entry_expired(&path, now) {
                let _ = fs::remove_dir_all(&path);
            }
            continue;
        }
        if pinned_before.contains(&name)
            || referenced_after.contains(&name)
            || pins.is_pinned(&name)
        {
            continue;
        }
        if store.referenced_plugin_package_hashes()?.contains(&name) || pins.is_pinned(&name) {
            continue;
        }
        if let Err(error) = fs::remove_dir_all(&path) {
            tracing::warn!(hash = %name, %error, "failed to remove unreferenced plugin artifact");
        } else {
            removed += 1;
        }
    }
    Ok(removed)
}

pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}
