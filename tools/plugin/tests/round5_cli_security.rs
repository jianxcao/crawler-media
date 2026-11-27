use plugin_cli::{CliError, run};
use plugin_package::{PackageError, PackagePolicy, validate_manifest_toml_with_policy};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

const MIN_SCHEMA: &str = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#;
const VALID_SITE_YAML: &str = "id: custom-r5-tracker\nframework: nexusphp\nsearch:\n  path: /torrents.php\n  query_param: search\n";

#[cfg(unix)]
#[test]
fn test_cmd_rejects_symlinked_cargo_toml_before_spawning_cargo() {
    let tmp = TempDir::new().expect("tmp");
    let marker = tmp.path().join("pwned_by_cargo_test");
    let evil_crate = tmp.path().join("evil_crate");
    fs::create_dir_all(evil_crate.join("src")).expect("evil src");
    fs::write(
        evil_crate.join("Cargo.toml"),
        "[package]\nname = \"evil_crate\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("write evil Cargo.toml");
    fs::write(
        evil_crate.join("build.rs"),
        format!("fn main() {{ let _ = std::fs::write({marker:?}, b\"pwned\"); }}\n"),
    )
    .expect("write evil build.rs");
    fs::write(evil_crate.join("src/lib.rs"), "").expect("write evil lib.rs");

    let proj = tmp.path().join("ui_proj");
    run(&args(&[
        "scaffold",
        "--api",
        "1.0.0",
        "--ui",
        "react",
        "--id",
        "org.example.r5-test-sym",
        &proj.to_string_lossy(),
    ]))
    .expect("scaffold ui");
    std::os::unix::fs::symlink(evil_crate.join("Cargo.toml"), proj.join("Cargo.toml"))
        .expect("symlink Cargo.toml");

    let err = run(&args(&["test", &proj.to_string_lossy(), "--offline"]))
        .expect_err("symlinked Cargo.toml must fail plugin test");
    assert!(
        matches!(err, CliError::Package(PackageError::UnsafePath { .. })),
        "expected UnsafePath, got: {err:?}"
    );
    assert!(
        !marker.exists(),
        "cargo test must not execute external build.rs via symlinked Cargo.toml"
    );
}

#[cfg(unix)]
#[test]
fn build_cmd_rejects_symlinked_vite_config_and_symlinked_local_vite_before_spawning_node() {
    let tmp = TempDir::new().expect("tmp");
    let proj = tmp.path().join("ui_build_sym");
    run(&args(&[
        "scaffold",
        "--api",
        "1.0.0",
        "--ui",
        "react",
        "--id",
        "org.example.r5-build-sym",
        &proj.to_string_lossy(),
    ]))
    .expect("scaffold ui");

    let real_vite_cfg = fs::read_to_string(proj.join("ui/vite.config.mjs")).expect("read vite cfg");
    let cfg_marker = tmp.path().join("pwned_by_vite_cfg");
    let evil_cfg = tmp.path().join("evil_vite.config.mjs");
    fs::write(
        &evil_cfg,
        format!("import fs from 'node:fs'; fs.writeFileSync({cfg_marker:?}, 'x'); export default {{}};\n"),
    )
    .expect("write evil_cfg");
    fs::remove_file(proj.join("ui/vite.config.mjs")).expect("rm vite.config.mjs");
    std::os::unix::fs::symlink(&evil_cfg, proj.join("ui/vite.config.mjs"))
        .expect("symlink vite.config.mjs");

    let err_cfg = run(&args(&["build", &proj.to_string_lossy()]))
        .expect_err("symlinked ui/vite.config.mjs must fail build");
    assert!(
        matches!(err_cfg, CliError::Package(PackageError::UnsafePath { .. })),
        "expected UnsafePath for vite.config.mjs, got: {err_cfg:?}"
    );
    assert!(
        !cfg_marker.exists(),
        "node must not execute symlinked vite.config.mjs"
    );

    // Restore real vite.config.mjs and symlink ui/node_modules/vite/bin/vite.js
    fs::remove_file(proj.join("ui/vite.config.mjs")).expect("rm symlink cfg");
    fs::write(proj.join("ui/vite.config.mjs"), &real_vite_cfg).expect("restore cfg");

    let bin_marker = tmp.path().join("pwned_by_local_vite");
    let evil_vite_bin = tmp.path().join("evil_vite.js");
    fs::write(
        &evil_vite_bin,
        format!("require('node:fs').writeFileSync({bin_marker:?}, 'x');\n"),
    )
    .expect("write evil_vite_bin");
    let local_bin_dir = proj.join("ui/node_modules/vite/bin");
    fs::create_dir_all(&local_bin_dir).expect("mkdir local vite bin");
    std::os::unix::fs::symlink(&evil_vite_bin, local_bin_dir.join("vite.js"))
        .expect("symlink vite.js");

    let err_bin = run(&args(&["build", &proj.to_string_lossy()]))
        .expect_err("symlinked local vite.js must fail build");
    assert!(
        matches!(err_bin, CliError::Package(PackageError::UnsafePath { .. })),
        "expected UnsafePath for local vite.js, got: {err_bin:?}"
    );
    assert!(
        !bin_marker.exists(),
        "node must not execute symlinked local vite.js"
    );
}

#[cfg(unix)]
#[test]
fn scaffold_rejects_symlinked_dir_and_ancestors_and_escapes_toml_strings() {
    let tmp = TempDir::new().expect("tmp");

    // 1. Symlinked target directory pointing to empty directory (with & without trailing slash)
    let outside_empty = tmp.path().join("outside_empty");
    fs::create_dir_all(&outside_empty).expect("mkdir outside_empty");
    let sym_dir = tmp.path().join("sym_dir");
    std::os::unix::fs::symlink(&outside_empty, &sym_dir).expect("symlink sym_dir");

    for target in [
        sym_dir.to_string_lossy().into_owned(),
        format!("{}/", sym_dir.display()),
    ] {
        let err = run(&args(&[
            "scaffold", "--api", "1.0.0", "--ui", "react", &target,
        ]))
        .expect_err("symlinked scaffold target must fail");
        assert!(
            matches!(err, CliError::Package(PackageError::UnsafePath { .. })),
            "expected UnsafePath for `{target}`, got: {err:?}"
        );
        assert!(
            fs::read_dir(&outside_empty)
                .expect("read_dir")
                .next()
                .is_none(),
            "outside_empty must remain untouched"
        );
    }

    // 2. Dangling symlink target directory
    let dangling = tmp.path().join("dangling_sym");
    let nonexistent = tmp.path().join("nonexistent_target");
    std::os::unix::fs::symlink(&nonexistent, &dangling).expect("symlink dangling");
    let err_dangling = run(&args(&[
        "scaffold",
        "--api",
        "1.0.0",
        "--ui",
        "react",
        &dangling.to_string_lossy(),
    ]))
    .expect_err("dangling symlink must fail scaffold");
    assert!(matches!(
        err_dangling,
        CliError::Package(PackageError::UnsafePath { .. })
    ));
    assert!(!nonexistent.exists());

    // 3. Symlinked ancestor component `sym_parent/nested/plugin`
    let outside_parent = tmp.path().join("outside_parent");
    fs::create_dir_all(&outside_parent).expect("mkdir outside_parent");
    let sym_parent = tmp.path().join("sym_parent");
    std::os::unix::fs::symlink(&outside_parent, &sym_parent).expect("symlink sym_parent");
    let via_ancestor = sym_parent.join("nested/my_plugin");
    let err_anc = run(&args(&[
        "scaffold",
        "--api",
        "1.0.0",
        "--ui",
        "react",
        &via_ancestor.to_string_lossy(),
    ]))
    .expect_err("symlinked ancestor must fail scaffold");
    assert!(matches!(
        err_anc,
        CliError::Package(PackageError::UnsafePath { .. })
    ));
    assert!(
        fs::read_dir(&outside_parent)
            .expect("read_dir")
            .next()
            .is_none()
    );

    // 4. TOML escaping of quotes and backslashes in --name
    let quoted_proj = tmp.path().join("quoted_name_proj");
    let tricky_name = r#"Alpha "Quoted" \ Plugin"#;
    run(&args(&[
        "scaffold",
        "--api",
        "1.0.0",
        "--ui",
        "react",
        "--id",
        "org.example.quoted-name",
        "--name",
        tricky_name,
        &quoted_proj.to_string_lossy(),
    ]))
    .expect("scaffold with quotes & backslash in --name must succeed");
    run(&args(&["validate", &quoted_proj.to_string_lossy()]))
        .expect("validate escaped plugin.toml");
    let raw_toml = fs::read_to_string(quoted_proj.join("plugin.toml")).expect("read plugin.toml");
    let parsed = validate_manifest_toml_with_policy(&raw_toml, &PackagePolicy::default())
        .expect("parse manifest");
    assert_eq!(parsed.name, tricky_name);
}

fn write_custom_project(dir: &Path, toml_body: &str, files: &[(&str, &[u8])]) {
    fs::create_dir_all(dir).expect("mkdir root");
    fs::write(dir.join("plugin.toml"), toml_body).expect("write plugin.toml");
    for (rel, content) in files {
        let full = dir.join(rel);
        if let Some(p) = full.parent() {
            fs::create_dir_all(p).expect("mkdir parent");
        }
        fs::write(full, content).expect("write file");
    }
}

#[test]
fn validate_rejects_ui_prefixed_non_ui_manifest_paths_matching_pack() {
    let tmp = TempDir::new().expect("tmp");

    // 1. config_schema under ui/ -> ForbiddenUiAsset
    let p_ui_schema = tmp.path().join("ui_schema");
    write_custom_project(
        &p_ui_schema,
        r#"manifest_version = 1
id = "org.example.ui-schema"
name = "UI Schema"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Tester"]
description = "Test"
config_schema = "ui/config.schema.json"
state_schema = 1
sites = ["sites/custom.yaml"]
"#,
        &[
            ("ui/config.schema.json", MIN_SCHEMA.as_bytes()),
            ("sites/custom.yaml", VALID_SITE_YAML.as_bytes()),
        ],
    );
    let err1 = run(&args(&["validate", &p_ui_schema.to_string_lossy()]))
        .expect_err("ui/ config_schema must fail validate");
    assert!(
        matches!(err1, CliError::Package(PackageError::ForbiddenUiAsset(_))),
        "expected ForbiddenUiAsset, got: {err1:?}"
    );

    // 2. sites entry under ui/ -> ForbiddenUiAsset
    let p_ui_site = tmp.path().join("ui_site");
    write_custom_project(
        &p_ui_site,
        r#"manifest_version = 1
id = "org.example.ui-site"
name = "UI Site"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Tester"]
description = "Test"
config_schema = "schemas/config.schema.json"
state_schema = 1
sites = ["ui/custom.yaml"]
"#,
        &[
            ("schemas/config.schema.json", MIN_SCHEMA.as_bytes()),
            ("ui/custom.yaml", VALID_SITE_YAML.as_bytes()),
        ],
    );
    let err2 = run(&args(&["validate", &p_ui_site.to_string_lossy()]))
        .expect_err("ui/ site profile must fail validate");
    assert!(
        matches!(err2, CliError::Package(PackageError::ForbiddenUiAsset(_))),
        "expected ForbiddenUiAsset, got: {err2:?}"
    );

    // 3. component entry under ui/ -> ForbiddenUiAsset
    let p_ui_comp = tmp.path().join("ui_comp");
    write_custom_project(
        &p_ui_comp,
        r#"manifest_version = 1
id = "org.example.ui-comp"
name = "UI Comp"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Tester"]
description = "Test"
config_schema = "schemas/config.schema.json"
state_schema = 1

[[components]]
id = "main"
entry = "ui/main.wasm"
world = "crawler:plugin/hook-plugin@1.0.0"
permissions = []

[[components.hooks]]
id = "search.after"
priority = 100
"#,
        &[
            ("schemas/config.schema.json", MIN_SCHEMA.as_bytes()),
            (
                "Cargo.toml",
                b"[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
            ),
        ],
    );
    let err3 = run(&args(&["validate", &p_ui_comp.to_string_lossy()]))
        .expect_err("ui/ component entry must fail validate");
    assert!(
        matches!(err3, CliError::Package(PackageError::ForbiddenUiAsset(_))),
        "expected ForbiddenUiAsset, got: {err3:?}"
    );
}

#[test]
fn validate_rejects_js_suffixed_non_ui_manifest_paths_matching_pack() {
    let tmp = TempDir::new().expect("tmp");

    for ext in ["js", "mjs", "cjs"] {
        let p_script = tmp.path().join(format!("script_schema_{ext}"));
        let schema_rel = format!("schemas/config.{ext}");
        write_custom_project(
            &p_script,
            &format!(
                r#"manifest_version = 1
id = "org.example.script-{ext}"
name = "Script {ext}"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Tester"]
description = "Test"
config_schema = "{schema_rel}"
state_schema = 1
sites = ["sites/custom.yaml"]
"#
            ),
            &[
                (&schema_rel, MIN_SCHEMA.as_bytes()),
                ("sites/custom.yaml", VALID_SITE_YAML.as_bytes()),
            ],
        );
        let err_s = run(&args(&["validate", &p_script.to_string_lossy()]))
            .expect_err("script-suffixed schema path must fail validate");
        assert!(
            matches!(
                err_s,
                CliError::Package(PackageError::ForbiddenExecutable(_))
            ),
            "expected ForbiddenExecutable for .{ext}, got: {err_s:?}"
        );
    }
}
