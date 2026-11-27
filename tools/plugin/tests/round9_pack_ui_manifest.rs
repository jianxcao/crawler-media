use plugin_cli::run;
use sha2::{Digest, Sha256};
use std::fs;
use tempfile::TempDir;

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn pack_and_validate_accept_asset_manifest_inside_ui_assets_dir() {
    let tmp = TempDir::new_in("/tmp").expect("tempdir");
    let proj = tmp.path().join("ui-nested-manifest");
    run(&args(&[
        "scaffold",
        "--api",
        "1.0.0",
        "--ui",
        "react",
        "--id",
        "org.example.ui-nested-manifest",
        &proj.to_string_lossy(),
    ]))
    .expect("scaffold ui");

    let manifest_toml = fs::read_to_string(proj.join("plugin.toml")).expect("read plugin.toml");
    let updated_toml = manifest_toml.replace(
        "asset_manifest = \"ui/asset-manifest.json\"",
        "asset_manifest = \"ui/assets/asset-manifest.json\"",
    );
    assert_ne!(manifest_toml, updated_toml);
    fs::write(proj.join("plugin.toml"), updated_toml).expect("write plugin.toml");

    fs::create_dir_all(proj.join("ui/assets")).expect("create ui/assets");
    let entry_js = b"export const remote = 1;\n";
    fs::write(proj.join("ui/assets/remoteEntry.js"), entry_js).expect("write remoteEntry");

    let asset_manifest = serde_json::json!({
        "version": 1,
        "files": [
            {
                "path": "ui/assets/remoteEntry.js",
                "sha256": sha256_hex(entry_js),
                "size": entry_js.len(),
                "content_type": "application/javascript"
            }
        ]
    });
    fs::write(
        proj.join("ui/assets/asset-manifest.json"),
        serde_json::to_vec_pretty(&asset_manifest).expect("serialize asset manifest"),
    )
    .expect("write ui/assets/asset-manifest.json");

    run(&args(&["validate", &proj.to_string_lossy()]))
        .expect("validate should accept asset_manifest under ui/assets/");

    let out_pkg = tmp.path().join("out.cmplugin");
    run(&args(&[
        "pack",
        &proj.to_string_lossy(),
        "--output",
        &out_pkg.to_string_lossy(),
    ]))
    .expect("pack should accept asset_manifest under ui/assets/");
    assert!(out_pkg.is_file());
}
