use crate::error::CliError;
use crate::validate::{
    collect_built_ui_assets, ensure_no_symlink_components, read_project_file_no_symlinks,
    validate_project_dir,
};
use plugin_package::{
    PackageError, PackagePolicy, PluginManifest, UiAssetManifest, build_deterministic_cmplugin_zip,
    validate_cmplugin_bytes,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub fn execute_pack(dir: &Path, output: &Path) -> Result<String, CliError> {
    let clean_out = ensure_no_symlink_components(output, false)?;
    let (manifest, bytes) = pack_project_to_bytes(dir)?;
    let clean_out = ensure_no_symlink_components(&clean_out, false)?;
    if let Some(parent) = clean_out.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let clean_out = ensure_no_symlink_components(&clean_out, false)?;
    fs::write(&clean_out, &bytes)?;
    Ok(format!(
        "Packed {}@{} ({} bytes) -> {}\n",
        manifest.id,
        manifest.version,
        bytes.len(),
        output.display()
    ))
}

pub fn pack_project_to_bytes(dir: &Path) -> Result<(PluginManifest, Vec<u8>), CliError> {
    let manifest = validate_project_dir(dir, true)?;

    let mut entries = BTreeMap::new();
    entries.insert(
        "plugin.toml".to_string(),
        read_project_file_no_symlinks(dir, "plugin.toml")?,
    );
    entries.insert(
        manifest.config_schema.clone(),
        read_project_file_no_symlinks(dir, &manifest.config_schema)?,
    );

    for comp in &manifest.components {
        entries.insert(
            comp.entry.clone(),
            read_project_file_no_symlinks(dir, &comp.entry)?,
        );
        for task in &comp.tasks {
            entries.insert(
                task.input_schema.clone(),
                read_project_file_no_symlinks(dir, &task.input_schema)?,
            );
        }
        for ep in &comp.endpoints {
            entries.insert(
                ep.input_schema.clone(),
                read_project_file_no_symlinks(dir, &ep.input_schema)?,
            );
        }
        for idx in &comp.indexers {
            entries.insert(
                idx.site_schema.clone(),
                read_project_file_no_symlinks(dir, &idx.site_schema)?,
            );
        }
    }

    for site_path in &manifest.sites {
        entries.insert(
            site_path.clone(),
            read_project_file_no_symlinks(dir, site_path)?,
        );
    }

    if let Some(ui) = &manifest.ui {
        let asset_manifest_bytes = read_project_file_no_symlinks(dir, &ui.asset_manifest)?;
        let parsed_assets: UiAssetManifest = serde_json::from_slice(&asset_manifest_bytes)
            .map_err(|e| CliError::Validation(format!("invalid `{}`: {e}", ui.asset_manifest)))?;
        entries.insert(ui.asset_manifest.clone(), asset_manifest_bytes);

        let mut disk_assets = BTreeMap::new();
        let assets_dir = dir.join("ui/assets");
        if let Ok(assets_meta) = fs::symlink_metadata(&assets_dir) {
            if assets_meta.file_type().is_symlink() {
                return Err(CliError::Package(PackageError::UnsafePath {
                    path: assets_dir.display().to_string(),
                    reason: "symlinks are forbidden in plugin project".into(),
                }));
            }
            if assets_meta.is_dir() {
                collect_built_ui_assets(dir, &assets_dir, &mut disk_assets)?;
            }
        }
        disk_assets.remove(&ui.asset_manifest);
        for declared in &parsed_assets.files {
            let bytes = match disk_assets.remove(&declared.path) {
                Some(b) => b,
                None => read_project_file_no_symlinks(dir, &declared.path)?,
            };
            entries.insert(declared.path.clone(), bytes);
        }
        if let Some((undeclared, _)) = disk_assets.into_iter().next() {
            return Err(CliError::Validation(format!(
                "undeclared file `{undeclared}` under `ui/assets/`"
            )));
        }
    }

    let zip_bytes = build_deterministic_cmplugin_zip(&entries)?;
    let dev_policy = PackagePolicy {
        dev_mode: true,
        ..PackagePolicy::default()
    };
    validate_cmplugin_bytes(&zip_bytes, &dev_policy)?;

    Ok((manifest, zip_bytes))
}
