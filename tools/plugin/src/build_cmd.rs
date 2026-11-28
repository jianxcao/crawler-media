use crate::error::CliError;
use crate::validate::{
    collect_built_ui_assets, ensure_safe_output_subpath_no_symlinks,
    read_project_file_no_symlinks_optional, sha256_hex, validate_project_dir,
    validate_wasm_component_bytes,
};
use plugin_package::{
    ArchiveEntryMeta, PackageError, PackagePolicy, PluginManifest, UiAssetEntry, UiAssetManifest,
    UiManifest, validate_ui_manifest_with_policy,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn execute_build(dir: &Path) -> Result<String, CliError> {
    let manifest = validate_project_dir(dir, false)?;

    if !manifest.components.is_empty()
        && read_project_file_no_symlinks_optional(dir, "Cargo.toml")?.is_some()
    {
        build_wasm_components(dir, &manifest)?;
    }

    if let Some(ui) = &manifest.ui {
        build_ui_bundle(dir, ui)?;
    }

    validate_project_dir(dir, true)?;
    Ok(format!(
        "Built plugin {}@{}\n",
        manifest.id, manifest.version
    ))
}

struct BuiltWasmArtifact {
    target_name: String,
    path: PathBuf,
}

fn build_wasm_components(dir: &Path, manifest: &PluginManifest) -> Result<(), CliError> {
    for comp in &manifest.components {
        ensure_safe_output_subpath_no_symlinks(dir, &comp.entry)?;
    }
    let cargo_toml = ensure_safe_output_subpath_no_symlinks(dir, "Cargo.toml")?;

    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .arg("--manifest-path")
        .arg(&cargo_toml)
        .arg("--target")
        .arg("wasm32-wasip2")
        .arg("--release")
        .arg("--message-format=json-render-diagnostics");
    if read_project_file_no_symlinks_optional(dir, "Cargo.lock")?.is_some() {
        cmd.arg("--locked");
    }

    let output = cmd.output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CliError::CommandFailed(format!(
            "wasm32-wasip2 build failed:\n{stderr}"
        )));
    }

    let artifacts = parse_cargo_wasm_artifacts(&output.stdout);
    let planned_writes = plan_component_writes(dir, manifest, &artifacts)?;
    for (entry, bytes) in planned_writes {
        let dest = ensure_safe_output_subpath_no_symlinks(dir, &entry)?;
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let dest = ensure_safe_output_subpath_no_symlinks(dir, &entry)?;
        fs::write(&dest, &bytes)?;
    }
    Ok(())
}

fn parse_cargo_wasm_artifacts(stdout: &[u8]) -> Vec<BuiltWasmArtifact> {
    let mut out = Vec::new();
    let text = String::from_utf8_lossy(stdout);
    for line in text.lines() {
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if val.get("reason").and_then(|v| v.as_str()) != Some("compiler-artifact") {
            continue;
        }
        let target_name = val
            .pointer("/target/name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let Some(filenames) = val.get("filenames").and_then(|v| v.as_array()) else {
            continue;
        };
        for file in filenames {
            let Some(path_str) = file.as_str() else {
                continue;
            };
            if path_str.ends_with(".wasm") {
                out.push(BuiltWasmArtifact {
                    target_name: target_name.clone(),
                    path: PathBuf::from(path_str),
                });
            }
        }
    }
    out
}

fn normalize_component_key(raw: &str) -> String {
    raw.replace('-', "_")
}

fn component_matches_artifact(
    comp: &plugin_package::ComponentManifest,
    artifact: &BuiltWasmArtifact,
) -> bool {
    let target_norm = normalize_component_key(&artifact.target_name);
    let file_stem_norm = artifact
        .path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(normalize_component_key);
    let id_norm = normalize_component_key(&comp.id);
    let entry_stem_norm = Path::new(&comp.entry)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(normalize_component_key);

    (!target_norm.is_empty() && target_norm == id_norm)
        || file_stem_norm.as_deref() == Some(id_norm.as_str())
        || (!target_norm.is_empty() && entry_stem_norm.as_deref() == Some(target_norm.as_str()))
        || (entry_stem_norm.is_some() && entry_stem_norm == file_stem_norm)
}

fn plan_component_writes(
    dir: &Path,
    manifest: &PluginManifest,
    artifacts: &[BuiltWasmArtifact],
) -> Result<Vec<(String, Vec<u8>)>, CliError> {
    if artifacts.is_empty() {
        return Err(CliError::CommandFailed(
            "cargo build produced no .wasm component artifacts".into(),
        ));
    }

    if manifest.components.len() == 1 && artifacts.len() == 1 {
        let comp = &manifest.components[0];
        let bytes = read_and_validate_artifact(&artifacts[0], &comp.id, &comp.entry)?;
        return Ok(vec![(comp.entry.clone(), bytes)]);
    }

    let mut comp_matches: Vec<Vec<usize>> = vec![Vec::new(); manifest.components.len()];
    for (art_idx, artifact) in artifacts.iter().enumerate() {
        let matched: Vec<usize> = manifest
            .components
            .iter()
            .enumerate()
            .filter(|(_, comp)| component_matches_artifact(comp, artifact))
            .map(|(idx, _)| idx)
            .collect();
        if matched.is_empty() {
            return Err(CliError::CommandFailed(format!(
                "built WASM artifact `{}` (target `{}`) did not match any component id or entry stem in plugin.toml",
                artifact.path.display(),
                artifact.target_name
            )));
        }
        if matched.len() > 1 {
            let ids = matched
                .iter()
                .map(|&i| manifest.components[i].id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(CliError::CommandFailed(format!(
                "ambiguous WASM artifact `{}` matched multiple components: {ids}",
                artifact.path.display()
            )));
        }
        comp_matches[matched[0]].push(art_idx);
    }

    let mut writes = Vec::new();
    for (comp_idx, comp) in manifest.components.iter().enumerate() {
        match comp_matches[comp_idx].as_slice() {
            [art_idx] => {
                let bytes =
                    read_and_validate_artifact(&artifacts[*art_idx], &comp.id, &comp.entry)?;
                writes.push((comp.entry.clone(), bytes));
            }
            [] => {
                let Some(existing) = read_project_file_no_symlinks_optional(dir, &comp.entry)?
                else {
                    return Err(CliError::CommandFailed(format!(
                        "component `{}` (`{}`) was not produced by cargo build and has no prebuilt WASM on disk",
                        comp.id, comp.entry
                    )));
                };
                validate_wasm_component_bytes(&existing, &comp.id, &comp.entry)?;
            }
            many => {
                let paths = many
                    .iter()
                    .map(|&i| artifacts[i].path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(CliError::CommandFailed(format!(
                    "component `{}` matched multiple built WASM artifacts: {paths}",
                    comp.id
                )));
            }
        }
    }
    Ok(writes)
}

fn read_and_validate_artifact(
    artifact: &BuiltWasmArtifact,
    comp_id: &str,
    comp_entry: &str,
) -> Result<Vec<u8>, CliError> {
    let bytes = fs::read(&artifact.path).map_err(|e| {
        CliError::CommandFailed(format!(
            "expected compiled WASM component at `{}`: {e}",
            artifact.path.display()
        ))
    })?;
    validate_wasm_component_bytes(&bytes, comp_id, comp_entry)?;
    Ok(bytes)
}

fn build_ui_bundle(dir: &Path, ui: &UiManifest) -> Result<(), CliError> {
    let vite_cfg = ensure_safe_output_subpath_no_symlinks(dir, "ui/vite.config.mjs")?;
    if let Ok(cfg_meta) = fs::symlink_metadata(&vite_cfg)
        && cfg_meta.file_type().is_symlink()
    {
        return Err(CliError::Package(PackageError::UnsafePath {
            path: vite_cfg.display().to_string(),
            reason: "symlinks are forbidden in plugin project".into(),
        }));
    }

    let ui_dir = dir.join("ui");
    let assets_dir = ui_dir.join("assets");
    if let Ok(meta) = fs::symlink_metadata(&assets_dir) {
        if meta.file_type().is_symlink() {
            return Err(CliError::Package(PackageError::UnsafePath {
                path: assets_dir.display().to_string(),
                reason: "symlinks are forbidden in plugin project".into(),
            }));
        }
        if meta.is_dir() {
            fs::remove_dir_all(&assets_dir)?;
        }
    }
    let manifest_path = ensure_safe_output_subpath_no_symlinks(dir, &ui.asset_manifest)?;
    if manifest_path.is_file() {
        fs::remove_file(&manifest_path)?;
    }

    let local_vite = ui_dir.join("node_modules/vite/bin/vite.js");
    let (vite_bin, maybe_web_root) = match resolve_local_vite(&ui_dir, &local_vite)? {
        Some(canon_vite) => (canon_vite, None),
        None => {
            let web_root = find_web_root(dir)?;
            (
                web_root.join("node_modules/vite/bin/vite.js"),
                Some(web_root),
            )
        }
    };

    let node_bin = std::env::var_os("NODE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("node"));

    let mut cmd = Command::new(&node_bin);
    cmd.arg(&vite_bin)
        .arg("build")
        .arg("--config")
        .arg("vite.config.mjs")
        .current_dir(&ui_dir);
    if let Some(web_root) = &maybe_web_root {
        cmd.env("CM_WEB_ROOT", web_root);
    }

    let output = cmd.output()?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CliError::CommandFailed(format!(
            "Vite federation UI build failed:\n{stdout}\n{stderr}"
        )));
    }

    write_and_validate_asset_manifest(dir, ui)
}

fn write_and_validate_asset_manifest(dir: &Path, ui: &UiManifest) -> Result<(), CliError> {
    let assets_dir = dir.join("ui/assets");
    let meta = fs::symlink_metadata(&assets_dir).map_err(|_| {
        CliError::CommandFailed("UI build produced no `ui/assets` directory".into())
    })?;
    if meta.file_type().is_symlink() {
        return Err(CliError::Package(PackageError::UnsafePath {
            path: assets_dir.display().to_string(),
            reason: "symlinks are forbidden inside UI assets".into(),
        }));
    }
    if !meta.is_dir() {
        return Err(CliError::CommandFailed(
            "UI build produced no `ui/assets` directory".into(),
        ));
    }

    let mut collected = BTreeMap::new();
    collect_built_ui_assets(dir, &assets_dir, &mut collected)?;

    let mut files = Vec::with_capacity(collected.len());
    let mut metas = Vec::with_capacity(collected.len() + 1);
    for (rel, bytes) in &collected {
        let sha = sha256_hex(bytes);
        let size = bytes.len() as u64;
        let content_type = infer_ui_content_type(rel)?;
        files.push(UiAssetEntry {
            path: rel.clone(),
            sha256: sha.clone(),
            size,
            content_type,
        });
        metas.push(ArchiveEntryMeta {
            path: rel.clone(),
            size,
            sha256_hex: sha,
        });
    }

    let asset_manifest = UiAssetManifest { version: 1, files };
    let json_val = serde_json::to_value(&asset_manifest)
        .map_err(|e| CliError::Validation(format!("serialize asset-manifest failed: {e}")))?;
    let json_bytes = serde_json::to_vec_pretty(&json_val)
        .map_err(|e| CliError::Validation(format!("serialize asset-manifest failed: {e}")))?;

    metas.push(ArchiveEntryMeta {
        path: ui.asset_manifest.clone(),
        size: json_bytes.len() as u64,
        sha256_hex: sha256_hex(&json_bytes),
    });

    validate_ui_manifest_with_policy(&json_val, Some(ui), &metas, &PackagePolicy::default())?;
    let manifest_dest = ensure_safe_output_subpath_no_symlinks(dir, &ui.asset_manifest)?;
    if let Some(parent) = manifest_dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let manifest_dest = ensure_safe_output_subpath_no_symlinks(dir, &ui.asset_manifest)?;
    fs::write(&manifest_dest, json_bytes)?;
    Ok(())
}

fn infer_ui_content_type(path: &str) -> Result<String, CliError> {
    let ext = path
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    let mime = match ext.as_str() {
        "js" | "mjs" => "application/javascript",
        "css" => "text/css",
        "woff2" => "font/woff2",
        "png" => "image/png",
        "jpg" => "image/jpeg",
        "webp" => "image/webp",
        _ => {
            return Err(CliError::Validation(format!(
                "disallowed UI asset extension on `{path}`"
            )));
        }
    };
    Ok(mime.to_string())
}

fn resolve_local_vite(ui_dir: &Path, local_vite: &Path) -> Result<Option<PathBuf>, CliError> {
    let nm_dir = ui_dir.join("node_modules");
    if let Ok(nm_meta) = fs::symlink_metadata(&nm_dir)
        && nm_meta.file_type().is_symlink()
    {
        return Err(CliError::Package(PackageError::UnsafePath {
            path: nm_dir.display().to_string(),
            reason: "symlinks are forbidden in plugin project".into(),
        }));
    }
    match fs::symlink_metadata(local_vite) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err(CliError::Package(PackageError::UnsafePath {
                path: local_vite.display().to_string(),
                reason: "symlinks are forbidden in plugin project".into(),
            }))
        }
        Ok(meta) if meta.is_file() => {
            let canon_ui = ui_dir.canonicalize()?;
            let canon_vite = local_vite.canonicalize()?;
            if !canon_vite.starts_with(&canon_ui) {
                return Err(CliError::Package(PackageError::UnsafePath {
                    path: local_vite.display().to_string(),
                    reason: "local Vite binary escapes UI directory via symlink".into(),
                }));
            }
            Ok(Some(canon_vite))
        }
        Ok(_) => Ok(None),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(CliError::Io(err)),
    }
}

fn find_web_root(project_dir: &Path) -> Result<PathBuf, CliError> {
    if let Some(explicit) = std::env::var_os("CRAWLER_PLUGIN_WEB_DIR").filter(|v| !v.is_empty()) {
        let candidate = PathBuf::from(explicit);
        if candidate.join("node_modules/vite/bin/vite.js").is_file() {
            return Ok(candidate.canonicalize()?);
        }
    }

    let compile_time_web = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web");
    if compile_time_web
        .join("node_modules/vite/bin/vite.js")
        .is_file()
    {
        return Ok(compile_time_web.canonicalize()?);
    }

    for start in [
        project_dir.canonicalize().ok(),
        std::env::current_dir().ok(),
        std::env::current_exe().ok(),
    ]
    .into_iter()
    .flatten()
    {
        for ancestor in start.ancestors() {
            let web_dir = ancestor.join("web");
            if web_dir.join("node_modules/vite/bin/vite.js").is_file() {
                return Ok(web_dir.canonicalize()?);
            }
        }
    }

    Err(CliError::CommandFailed(
        "could not locate Vite UI toolchain (`ui/node_modules` or workspace `web/node_modules`)"
            .into(),
    ))
}
