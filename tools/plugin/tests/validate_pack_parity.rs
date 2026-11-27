use plugin_cli::run;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn write_site_project(dir: &Path, site_id: &str, framework: &str) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas dir");
    fs::create_dir_all(dir.join("sites")).expect("sites dir");
    fs::write(
        dir.join("plugin.toml"),
        r#"manifest_version = 1
id = "org.example.site-check"
name = "Site Check"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Tester"]
description = "Site validation parity test"
config_schema = "schemas/config.schema.json"
state_schema = 1
sites = ["sites/custom.yaml"]
"#,
    )
    .expect("write plugin.toml");
    fs::write(
        dir.join("schemas/config.schema.json"),
        r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#,
    )
    .expect("write schema");
    let yaml = format!(
        "id: {site_id}\nframework: {framework}\nsearch:\n  path: /torrents.php\n  query_param: search\n"
    );
    fs::write(dir.join("sites/custom.yaml"), yaml).expect("write site yaml");
}

fn setup_ui_only_built_project(dir: &Path, font_bytes: &[u8]) {
    run(&args(&[
        "scaffold",
        "--api",
        "1.0.0",
        "--ui",
        "react",
        "--id",
        "org.example.ui-font",
        &dir.to_string_lossy(),
    ]))
    .expect("scaffold ui");
    fs::create_dir_all(dir.join("ui/assets")).expect("assets dir");
    fs::create_dir_all(dir.join("ui/fonts")).expect("fonts dir");
    let entry_js = b"export const remote = true;\n";
    fs::write(dir.join("ui/assets/remoteEntry.js"), entry_js).expect("write remoteEntry");
    fs::write(dir.join("ui/fonts/inter.woff2"), font_bytes).expect("write font");

    let manifest_json = serde_json::json!({
        "version": 1,
        "files": [
            {
                "path": "ui/assets/remoteEntry.js",
                "sha256": sha256_hex(entry_js),
                "size": entry_js.len(),
                "content_type": "application/javascript"
            },
            {
                "path": "ui/fonts/inter.woff2",
                "sha256": sha256_hex(font_bytes),
                "size": font_bytes.len(),
                "content_type": "font/woff2"
            }
        ]
    });
    fs::write(
        dir.join("ui/asset-manifest.json"),
        serde_json::to_vec_pretty(&manifest_json).expect("json"),
    )
    .expect("write asset-manifest");
}

#[test]
fn validate_dir_rejects_invalid_site_framework_and_builtin_profile_collisions() {
    let tmp = TempDir::new().expect("tmp");

    let bad_fw = tmp.path().join("bad_fw");
    write_site_project(&bad_fw, "custom-tracker", "gazelle");
    assert!(
        run(&args(&["validate", &bad_fw.to_string_lossy()])).is_err(),
        "unsupported framework `gazelle` must fail validate <dir>"
    );

    for builtin_id in ["mteam", "demo"] {
        let collision = tmp.path().join(format!("collision_{builtin_id}"));
        write_site_project(&collision, builtin_id, "nexusphp");
        assert!(
            run(&args(&["validate", &collision.to_string_lossy()])).is_err(),
            "built-in profile id `{builtin_id}` must fail validate <dir>"
        );
    }

    let valid = tmp.path().join("valid_site");
    write_site_project(&valid, "custom-nexus", "nexusphp");
    run(&args(&["validate", &valid.to_string_lossy()])).expect("valid custom site profile passes");
}

#[test]
fn validate_dir_and_pack_accept_ui_font_outside_assets_and_reject_symlinks_under_ui() {
    let tmp = TempDir::new().expect("tmp");
    let proj = tmp.path().join("ui_font_proj");
    let font_bytes = b"wOF2-mock-font-payload";
    setup_ui_only_built_project(&proj, font_bytes);

    run(&args(&["validate", &proj.to_string_lossy()]))
        .expect("valid ui/fonts/inter.woff2 outside ui/assets/ must pass validate <dir>");
    let ok_pkg = tmp.path().join("ok.cmplugin");
    run(&args(&[
        "pack",
        &proj.to_string_lossy(),
        "--output",
        &ok_pkg.to_string_lossy(),
    ]))
    .expect("valid ui/fonts/inter.woff2 must pack cleanly");
    assert!(ok_pkg.is_file());

    #[cfg(unix)]
    {
        let outside = tmp.path().join("outside.woff2");
        fs::write(&outside, font_bytes).expect("write outside");
        fs::remove_file(proj.join("ui/fonts/inter.woff2")).expect("rm font");
        std::os::unix::fs::symlink(&outside, proj.join("ui/fonts/inter.woff2")).expect("symlink");

        assert!(
            run(&args(&["validate", &proj.to_string_lossy()])).is_err(),
            "symlinked ui/fonts/inter.woff2 must be rejected in validate <dir>"
        );
        let bad_pkg = tmp.path().join("bad_symlink.cmplugin");
        assert!(
            run(&args(&[
                "pack",
                &proj.to_string_lossy(),
                "--output",
                &bad_pkg.to_string_lossy(),
            ]))
            .is_err(),
            "symlinked ui/fonts/inter.woff2 must be rejected in pack"
        );
        assert!(!bad_pkg.exists());

        // Symlinked directory `ui/fonts -> outside_fonts` must also be rejected.
        fs::remove_dir_all(proj.join("ui/fonts")).expect("rm ui/fonts");
        let outside_dir = tmp.path().join("outside_fonts");
        fs::create_dir_all(&outside_dir).expect("mkdir outside_fonts");
        fs::write(outside_dir.join("inter.woff2"), font_bytes).expect("write font");
        std::os::unix::fs::symlink(&outside_dir, proj.join("ui/fonts")).expect("symlink dir");

        assert!(run(&args(&["validate", &proj.to_string_lossy()])).is_err());
        assert!(
            run(&args(&[
                "pack",
                &proj.to_string_lossy(),
                "--output",
                &bad_pkg.to_string_lossy(),
            ]))
            .is_err()
        );
        assert!(!bad_pkg.exists());
    }
}

#[cfg(unix)]
#[test]
fn validate_pack_and_build_reject_symlinked_schemas_and_sites() {
    let tmp = TempDir::new().expect("tmp");
    let valid_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#;
    let valid_site = "id: custom-nexus\nframework: nexusphp\nsearch:\n  path: /torrents.php\n  query_param: search\n";

    let site_proj = tmp.path().join("sym_schema_site");
    write_site_project(&site_proj, "custom-nexus", "nexusphp");
    let ext_schema = tmp.path().join("ext_config.schema.json");
    fs::write(&ext_schema, valid_schema).expect("ext schema");
    fs::remove_file(site_proj.join("schemas/config.schema.json")).expect("rm schema");
    std::os::unix::fs::symlink(&ext_schema, site_proj.join("schemas/config.schema.json"))
        .expect("symlink schema");

    let out_pkg = tmp.path().join("sym_schema.cmplugin");
    assert!(
        run(&args(&["validate", &site_proj.to_string_lossy()])).is_err(),
        "symlinked schemas/config.schema.json must fail validate"
    );
    assert!(
        run(&args(&[
            "pack",
            &site_proj.to_string_lossy(),
            "--output",
            &out_pkg.to_string_lossy()
        ]))
        .is_err(),
        "symlinked schemas/config.schema.json must fail pack"
    );
    assert!(
        run(&args(&["build", &site_proj.to_string_lossy()])).is_err(),
        "symlinked schemas/config.schema.json must fail build"
    );
    assert!(!out_pkg.exists());

    fs::remove_file(site_proj.join("schemas/config.schema.json")).expect("rm symlink schema");
    fs::write(site_proj.join("schemas/config.schema.json"), valid_schema).expect("restore schema");
    let ext_site = tmp.path().join("ext_site.yaml");
    fs::write(&ext_site, valid_site).expect("ext site");
    fs::remove_file(site_proj.join("sites/custom.yaml")).expect("rm site");
    std::os::unix::fs::symlink(&ext_site, site_proj.join("sites/custom.yaml"))
        .expect("symlink site");

    assert!(
        run(&args(&["validate", &site_proj.to_string_lossy()])).is_err(),
        "symlinked sites/custom.yaml must fail validate"
    );
    assert!(
        run(&args(&[
            "pack",
            &site_proj.to_string_lossy(),
            "--output",
            &out_pkg.to_string_lossy()
        ]))
        .is_err(),
        "symlinked sites/custom.yaml must fail pack"
    );
    assert!(
        run(&args(&["build", &site_proj.to_string_lossy()])).is_err(),
        "symlinked sites/custom.yaml must fail build"
    );
}

#[cfg(unix)]
#[test]
fn validate_pack_and_build_reject_symlinked_components() {
    let tmp = TempDir::new().expect("tmp");
    let empty_component_wasm = b"\0asm\x0d\x00\x01\x00";
    let out_pkg = tmp.path().join("sym_wasm.cmplugin");

    let wasm_proj = tmp.path().join("sym_wasm_proj");
    run(&args(&[
        "scaffold",
        "--world",
        "hook-plugin",
        "--api",
        "1.0.0",
        "--id",
        "org.example.sym-wasm",
        &wasm_proj.to_string_lossy(),
    ]))
    .expect("scaffold hook-plugin");
    fs::create_dir_all(wasm_proj.join("components")).expect("components dir");
    let ext_wasm = tmp.path().join("ext_main.wasm");
    fs::write(&ext_wasm, empty_component_wasm).expect("write ext wasm");
    std::os::unix::fs::symlink(&ext_wasm, wasm_proj.join("components/main.wasm"))
        .expect("symlink main.wasm");

    assert!(
        run(&args(&["validate", &wasm_proj.to_string_lossy()])).is_err(),
        "symlinked components/main.wasm must fail validate"
    );
    assert!(
        run(&args(&[
            "pack",
            &wasm_proj.to_string_lossy(),
            "--output",
            &out_pkg.to_string_lossy()
        ]))
        .is_err(),
        "symlinked components/main.wasm must fail pack"
    );
    assert!(
        run(&args(&["build", &wasm_proj.to_string_lossy()])).is_err(),
        "symlinked components/main.wasm must fail build"
    );
    assert_eq!(
        fs::read(&ext_wasm).expect("read ext wasm"),
        empty_component_wasm,
        "build must not overwrite external symlink target"
    );

    fs::remove_dir_all(wasm_proj.join("components")).expect("rm components");
    let ext_comp_dir = tmp.path().join("ext_comp_dir");
    fs::create_dir_all(&ext_comp_dir).expect("mkdir ext_comp_dir");
    std::os::unix::fs::symlink(&ext_comp_dir, wasm_proj.join("components"))
        .expect("symlink components dir");
    assert!(
        run(&args(&["validate", &wasm_proj.to_string_lossy()])).is_err(),
        "symlinked components/ directory must fail validate"
    );
    assert!(
        run(&args(&["build", &wasm_proj.to_string_lossy()])).is_err(),
        "symlinked components/ directory must fail build"
    );
    assert!(!ext_comp_dir.join("main.wasm").exists());
}

#[cfg(unix)]
#[test]
fn validate_pack_and_build_reject_trailing_slash_root_symlink_and_post_vite_assets_symlink() {
    let tmp = TempDir::new().expect("tmp");
    let real_proj = tmp.path().join("real_site_proj");
    write_site_project(&real_proj, "custom-trailing", "nexusphp");

    // Real directory under `/tmp` (system ancestor symlink on macOS) with trailing `/` or `/.` passes.
    for ok_suffix in ["", "/", "/."] {
        let arg = format!("{}{ok_suffix}", real_proj.display());
        run(&args(&["validate", &arg])).expect("real project dir with suffix passes validate");
    }

    let sym_root = tmp.path().join("sym_root_proj");
    std::os::unix::fs::symlink(&real_proj, &sym_root).expect("symlink root dir");
    let out_pkg = tmp.path().join("sym_root.cmplugin");

    for suffix in ["", "/", "/."] {
        let arg = format!("{}{suffix}", sym_root.display());
        assert!(
            run(&args(&["validate", &arg])).is_err(),
            "symlinked root `{arg}` must fail validate"
        );
        assert!(
            run(&args(&[
                "pack",
                &arg,
                "--output",
                &out_pkg.to_string_lossy()
            ]))
            .is_err(),
            "symlinked root `{arg}` must fail pack"
        );
        assert!(
            run(&args(&["build", &arg])).is_err(),
            "symlinked root `{arg}` must fail build"
        );
        assert!(!out_pkg.exists());
    }

    // Post-Vite `ui/assets` symlink created during `plugin build` must be rejected with UnsafePath
    // before collecting assets or writing `ui/asset-manifest.json`.
    let ui_proj = tmp.path().join("post_vite_sym_assets");
    run(&args(&[
        "scaffold",
        "--api",
        "1.0.0",
        "--ui",
        "react",
        "--id",
        "org.example.post-vite-sym",
        &ui_proj.to_string_lossy(),
    ]))
    .expect("scaffold ui");
    let outside_assets = tmp.path().join("outside_built_assets");
    fs::create_dir_all(&outside_assets).expect("outside_assets");
    fs::write(
        outside_assets.join("remoteEntry.js"),
        b"export const r = 1;\n",
    )
    .expect("write outside remoteEntry.js");
    let fake_vite_dir = ui_proj.join("ui/node_modules/vite/bin");
    fs::create_dir_all(&fake_vite_dir).expect("fake vite dir");
    fs::write(
        fake_vite_dir.join("vite.js"),
        "require('node:fs').symlinkSync('../../outside_built_assets', 'assets');\n",
    )
    .expect("write fake vite.js");

    let err = run(&args(&["build", &ui_proj.to_string_lossy()]))
        .expect_err("post-Vite symlinked ui/assets must fail build");
    assert!(
        err.to_string().contains("symlink"),
        "expected symlink error, got: {err}"
    );
    assert!(
        !ui_proj.join("ui/asset-manifest.json").exists(),
        "must not write ui/asset-manifest.json when post-Vite ui/assets is a symlink"
    );
}
