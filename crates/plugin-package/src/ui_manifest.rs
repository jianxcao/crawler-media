use crate::archive::validate_safe_rel_path;
use crate::manifest_rules::{validate_plain_text, validate_slug_id};
use crate::{
    ArchiveEntryMeta, PackageError, PackagePolicy, UiManifest, UiSurfaceKind, UiSurfaceManifest,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use unicode_normalization::UnicodeNormalization;

const UI_READ_CAPS: &[&str] = &[
    "read:sites",
    "read:downloads",
    "read:downloaders",
    "read:history",
    "read:boost",
    "read:library",
    "read:libraries",
    "read:subscribes",
    "read:catalog",
];

pub const MAX_UI_FILES: usize = 1024;
pub const MAX_UI_FILE_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_UI_TOTAL_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiAssetEntry {
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub content_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiAssetManifest {
    pub version: u32,
    pub files: Vec<UiAssetEntry>,
}

pub fn validate_ui_manifest(
    json: &serde_json::Value,
    entries: &[ArchiveEntryMeta],
) -> Result<UiAssetManifest, PackageError> {
    validate_ui_manifest_with_policy(json, None, entries, &PackagePolicy::default())
}

pub fn validate_ui_manifest_with_policy(
    json: &serde_json::Value,
    ui_decl: Option<&UiManifest>,
    entries: &[ArchiveEntryMeta],
    _policy: &PackagePolicy,
) -> Result<UiAssetManifest, PackageError> {
    let manifest: UiAssetManifest = serde_json::from_value(json.clone()).map_err(|e| {
        PackageError::InvalidUiManifest(format!("invalid asset-manifest.json: {e}"))
    })?;

    if manifest.version != 1 {
        return Err(PackageError::InvalidUiManifest(format!(
            "asset-manifest version must be 1, got {}",
            manifest.version
        )));
    }
    if manifest.files.len() > MAX_UI_FILES {
        return Err(PackageError::UiAssetLimitExceeded(format!(
            "UI asset count {} exceeds max {MAX_UI_FILES}",
            manifest.files.len()
        )));
    }

    let asset_manifest_path = ui_decl
        .map(|u| u.asset_manifest.as_str())
        .unwrap_or("ui/asset-manifest.json");

    let entry_map: HashMap<&str, &ArchiveEntryMeta> =
        entries.iter().map(|e| (e.path.as_str(), e)).collect();

    let mut seen_exact: HashMap<&str, String> = HashMap::new();
    let mut seen_fold: HashMap<String, String> = HashMap::new();
    let mut total_bytes: u64 = 0;

    for file in &manifest.files {
        validate_safe_rel_path(&file.path, &file.path)?;
        if !file.path.starts_with("ui/") {
            return Err(PackageError::ForbiddenUiAsset(format!(
                "UI asset `{}` must live under `ui/`",
                file.path
            )));
        }
        if file.path == asset_manifest_path || file.path.ends_with("/asset-manifest.json") {
            return Err(PackageError::InvalidUiManifest(
                "asset-manifest.json must not list itself in files[]".into(),
            ));
        }
        let fold: String = file.path.nfc().collect::<String>().to_lowercase();
        if seen_exact.contains_key(file.path.as_str()) {
            return Err(PackageError::InvalidUiManifest(format!(
                "duplicate UI asset path `{}`",
                file.path
            )));
        }
        if let Some(existing) = seen_fold.get(&fold) {
            return Err(PackageError::PathCollision {
                path: file.path.clone(),
                existing: existing.clone(),
            });
        }
        seen_fold.insert(fold, file.path.clone());

        let canonical_mime = validate_ui_asset_ext_and_mime(&file.path, &file.content_type)?;
        seen_exact.insert(file.path.as_str(), canonical_mime);
        validate_sha256_lower(&file.sha256, &file.path)?;

        if file.size > MAX_UI_FILE_BYTES {
            return Err(PackageError::UiAssetLimitExceeded(format!(
                "UI asset `{}` size {} exceeds 8 MiB",
                file.path, file.size
            )));
        }
        total_bytes = total_bytes
            .checked_add(file.size)
            .ok_or_else(|| PackageError::UiAssetLimitExceeded("UI total size overflow".into()))?;
        if total_bytes > MAX_UI_TOTAL_BYTES {
            return Err(PackageError::UiAssetLimitExceeded(
                "UI total asset size exceeds 32 MiB".into(),
            ));
        }

        let actual = entry_map.get(file.path.as_str()).ok_or_else(|| {
            PackageError::InvalidUiManifest(format!(
                "declared UI asset `{}` missing from archive",
                file.path
            ))
        })?;
        if actual.size != file.size || actual.sha256_hex != file.sha256 {
            return Err(PackageError::InvalidUiManifest(format!(
                "UI asset `{}` size/sha256 mismatch with archive entry",
                file.path
            )));
        }
    }

    validate_ui_entry_and_undeclared(ui_decl, asset_manifest_path, &seen_exact, entries)?;

    Ok(manifest)
}

fn validate_ui_entry_and_undeclared(
    ui_decl: Option<&UiManifest>,
    asset_manifest_path: &str,
    seen_exact: &HashMap<&str, String>,
    entries: &[ArchiveEntryMeta],
) -> Result<(), PackageError> {
    if let Some(ui) = ui_decl {
        if !(ui.entry.ends_with(".js") || ui.entry.ends_with(".mjs")) {
            return Err(PackageError::InvalidUiManifest(format!(
                "ui.entry `{}` must end with .js or .mjs",
                ui.entry
            )));
        }
        let Some(entry_mime) = seen_exact.get(ui.entry.as_str()) else {
            return Err(PackageError::InvalidUiManifest(format!(
                "ui.entry `{}` is not listed in asset-manifest.json files[]",
                ui.entry
            )));
        };
        if !matches!(
            entry_mime.as_str(),
            "application/javascript" | "text/javascript"
        ) {
            return Err(PackageError::InvalidUiManifest(format!(
                "ui.entry `{}` must have JavaScript MIME type",
                ui.entry
            )));
        }
    }

    for e in entries {
        let lower = e.path.to_ascii_lowercase();
        if lower.starts_with("ui/")
            && e.path != asset_manifest_path
            && !seen_exact.contains_key(e.path.as_str())
        {
            if is_forbidden_ui_path(&e.path) {
                return Err(PackageError::ForbiddenUiAsset(e.path.clone()));
            }
            return Err(PackageError::InvalidUiManifest(format!(
                "undeclared file `{}` under ui/",
                e.path
            )));
        }
    }
    Ok(())
}

fn validate_ui_asset_ext_and_mime(path: &str, content_type: &str) -> Result<String, PackageError> {
    if content_type.chars().any(char::is_control) {
        return Err(PackageError::ForbiddenUiAsset(format!(
            "{path} (invalid control character in MIME `{content_type}`)"
        )));
    }
    let mut parts = content_type.split(';');
    let raw_media = parts.next().unwrap_or("").trim();
    let Some((ty, sub)) = raw_media.split_once('/') else {
        return Err(PackageError::ForbiddenUiAsset(format!(
            "{path} (malformed MIME `{content_type}`)"
        )));
    };
    if !is_mime_token(ty) || !is_mime_token(sub) {
        return Err(PackageError::ForbiddenUiAsset(format!(
            "{path} (malformed MIME `{content_type}`)"
        )));
    }
    for param in parts {
        let p = param.trim();
        let Some((k, v)) = p.split_once('=') else {
            return Err(PackageError::ForbiddenUiAsset(format!(
                "{path} (malformed MIME parameter in `{content_type}`)"
            )));
        };
        let v_trimmed = v.trim();
        if !is_mime_token(k.trim()) || v_trimmed.is_empty() {
            return Err(PackageError::ForbiddenUiAsset(format!(
                "{path} (malformed MIME parameter in `{content_type}`)"
            )));
        }
    }

    let ext = path
        .rsplit_once('/')
        .map_or(path, |(_, file)| file)
        .rsplit_once('.')
        .map(|(_, ext)| ext)
        .unwrap_or("");
    let mime = raw_media.to_ascii_lowercase();

    let valid = match ext {
        "js" | "mjs" => matches!(mime.as_str(), "application/javascript" | "text/javascript"),
        "css" => mime == "text/css",
        "woff2" => mime == "font/woff2",
        "png" => mime == "image/png",
        "jpg" => mime == "image/jpeg",
        "webp" => mime == "image/webp",
        _ => {
            return Err(PackageError::ForbiddenUiAsset(format!(
                "{path} (disallowed UI extension)"
            )));
        }
    };
    if !valid {
        return Err(PackageError::ForbiddenUiAsset(format!(
            "{path} (MIME `{content_type}` does not match extension `.{ext}`)"
        )));
    }
    Ok(mime)
}

fn is_mime_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'+' | b'_'))
}

pub fn is_forbidden_ui_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".html")
        || lower.ends_with(".htm")
        || lower.ends_with(".map")
        || lower.ends_with(".ts")
        || lower.ends_with(".tsx")
        || lower.ends_with(".jsx")
        || lower.ends_with(".cjs")
        || lower.ends_with(".scss")
        || lower.ends_with("package.json")
}

fn validate_sha256_lower(hex: &str, path: &str) -> Result<(), PackageError> {
    if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(PackageError::InvalidUiManifest(format!(
            "UI asset `{path}` sha256 must be 64 lowercase hex characters"
        )));
    }
    Ok(())
}

pub(crate) fn validate_ui_declaration(
    ui: &UiManifest,
    task_ids: &HashSet<String>,
    policy: &PackagePolicy,
) -> Result<(), PackageError> {
    if ui.kind != "federation-react" {
        return Err(PackageError::InvalidUiManifest(format!(
            "unsupported ui.kind `{}`",
            ui.kind
        )));
    }
    if ui.contract != policy.ui_contract {
        return Err(PackageError::InvalidUiManifest(format!(
            "ui.contract `{}` does not match `{}`",
            ui.contract, policy.ui_contract
        )));
    }
    if ui.react != policy.react_version || ui.react_dom != policy.react_dom_version {
        return Err(PackageError::InvalidUiManifest(
            "ui.react and ui.react_dom must match exact host React version".into(),
        ));
    }
    validate_remote_name(&ui.remote_name, policy)?;
    validate_safe_rel_path(&ui.entry, &ui.entry)?;
    validate_safe_rel_path(&ui.asset_manifest, &ui.asset_manifest)?;
    if !ui.entry.starts_with("ui/") || !ui.asset_manifest.starts_with("ui/") {
        return Err(PackageError::InvalidUiManifest(
            "ui.entry and ui.asset_manifest must live under ui/".into(),
        ));
    }
    if !(ui.entry.ends_with(".js") || ui.entry.ends_with(".mjs")) {
        return Err(PackageError::InvalidUiManifest(format!(
            "ui.entry `{}` must end with .js or .mjs",
            ui.entry
        )));
    }
    for cap in &ui.capabilities {
        validate_ui_capability(cap, task_ids, policy)?;
    }
    if ui.surfaces.is_empty() || ui.surfaces.len() > 16 {
        return Err(PackageError::InvalidUiManifest(
            "ui.surfaces must contain 1..=16 entries".into(),
        ));
    }
    let mut config_count = 0usize;
    let mut ids = HashSet::new();
    let mut exposes = HashSet::new();
    for s in &ui.surfaces {
        validate_ui_surface(s, &mut ids, &mut exposes, &mut config_count, policy)?;
    }
    Ok(())
}

fn validate_ui_surface<'a>(
    s: &'a UiSurfaceManifest,
    ids: &mut HashSet<&'a str>,
    exposes: &mut HashSet<&'a str>,
    config_count: &mut usize,
    policy: &PackagePolicy,
) -> Result<(), PackageError> {
    validate_slug_id("ui.surfaces[].id", &s.id, 64)
        .map_err(|e| PackageError::InvalidUiManifest(e.to_string()))?;
    if !ids.insert(s.id.as_str()) {
        return Err(PackageError::InvalidUiManifest(format!(
            "duplicate surface id `{}`",
            s.id
        )));
    }
    if s.kind == UiSurfaceKind::Config {
        *config_count += 1;
        if *config_count > 1 {
            return Err(PackageError::InvalidUiManifest(
                "at most 1 `config` surface is allowed".into(),
            ));
        }
    }
    let Some(rest) = s.expose.strip_prefix("./") else {
        return Err(PackageError::InvalidUiManifest(format!(
            "invalid surface expose `{}`: must start with `./`",
            s.expose
        )));
    };
    if s.expose.len() > 128
        || s.expose
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
        || s.expose.contains('?')
        || s.expose.contains('#')
        || validate_safe_rel_path(rest, &s.expose).is_err()
        || !exposes.insert(s.expose.as_str())
    {
        return Err(PackageError::InvalidUiManifest(format!(
            "invalid or duplicate surface expose `{}`",
            s.expose
        )));
    }
    validate_plain_text("ui.surfaces[].title", &s.title, 80, false)
        .map_err(|e| PackageError::InvalidUiManifest(e.to_string()))?;
    if let Some(icon) = &s.icon
        && !policy
            .allowed_ui_icons
            .iter()
            .any(|allowed| allowed == icon)
    {
        return Err(PackageError::InvalidUiManifest(format!(
            "unknown ui surface icon `{icon}`"
        )));
    }
    Ok(())
}

fn validate_remote_name(name: &str, policy: &PackagePolicy) -> Result<(), PackageError> {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err(PackageError::InvalidUiManifest(
            "empty ui.remote_name".into(),
        ));
    };
    if name.len() > 64 || !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
        return Err(PackageError::InvalidUiManifest(format!(
            "invalid JS identifier ui.remote_name `{name}`"
        )));
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$') {
        return Err(PackageError::InvalidUiManifest(format!(
            "invalid JS identifier ui.remote_name `{name}`"
        )));
    }
    if policy.reserved_remote_names.iter().any(|r| r == name) {
        return Err(PackageError::InvalidUiManifest(format!(
            "reserved ui.remote_name `{name}`"
        )));
    }
    Ok(())
}

fn validate_ui_capability(
    cap: &str,
    task_ids: &HashSet<String>,
    policy: &PackagePolicy,
) -> Result<(), PackageError> {
    if cap.is_empty() || cap.contains('*') {
        return Err(PackageError::InvalidUiManifest(format!(
            "invalid ui capability `{cap}`"
        )));
    }
    if UI_READ_CAPS.contains(&cap) {
        return Ok(());
    }
    if let Some(cmd) = cap.strip_prefix("command:")
        && policy.allowed_commands.iter().any(|c| c == cmd)
    {
        return Ok(());
    }
    if let Some(tid) = cap.strip_prefix("task:run:") {
        if task_ids.contains(tid) {
            return Ok(());
        }
        return Err(PackageError::InvalidUiManifest(format!(
            "ui capability `{cap}` references undeclared task `{tid}`"
        )));
    }
    Err(PackageError::InvalidUiManifest(format!(
        "disallowed ui capability `{cap}`"
    )))
}
