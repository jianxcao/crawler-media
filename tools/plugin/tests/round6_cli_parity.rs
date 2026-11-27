use plugin_cli::{CliError, run};
use plugin_package::{PackageError, PackagePolicy, validate_manifest_toml_with_policy};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

const MIN_SCHEMA: &str = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#;
const VALID_SITE_YAML: &str = "id: custom-r6-tracker\nframework: nexusphp\nsearch:\n  path: /torrents.php\n  query_param: search\n";

fn write_project(dir: &Path, toml_body: &str, files: &[(&str, &[u8])]) {
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
fn validate_rejects_forbidden_exec_extensions_across_declared_paths() {
    let tmp = TempDir::new().expect("tmp");

    for ext in [
        "sh", "SH", "bat", "cmd", "exe", "dll", "so", "dylib", "ps1", "py", "PY", "rb", "pl",
    ] {
        let proj = tmp.path().join(format!("exec_schema_{ext}"));
        let schema_rel = format!("schemas/config.{ext}");
        write_project(
            &proj,
            &format!(
                r#"manifest_version = 1
id = "org.example.exec-check"
name = "Exec Check"
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
        let err = run(&args(&["validate", &proj.to_string_lossy()]))
            .expect_err("forbidden exec extension on config_schema must fail validate");
        assert!(
            matches!(err, CliError::Package(PackageError::ForbiddenExecutable(_))),
            "expected ForbiddenExecutable for .{ext}, got: {err:?}"
        );
    }

    let ui_exec = tmp.path().join("ui_exec_manifest");
    write_project(
        &ui_exec,
        r#"manifest_version = 1
id = "org.example.ui-exec"
name = "UI Exec"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Tester"]
description = "Test"
config_schema = "schemas/config.schema.json"
state_schema = 1

[ui]
kind = "federation-react"
entry = "ui/assets/remoteEntry.js"
asset_manifest = "ui/asset-manifest.sh"
remote_name = "cmUiExec"
contract = "1.0.0"
react = "19.3.0"
react_dom = "19.3.0"
capabilities = []

[[ui.surfaces]]
id = "main"
kind = "page"
expose = "./Page"
title = "Main"
"#,
        &[
            ("schemas/config.schema.json", MIN_SCHEMA.as_bytes()),
            ("ui/package.json", b"{}"),
        ],
    );
    let err_ui = run(&args(&["validate", &ui_exec.to_string_lossy()]))
        .expect_err("ui.asset_manifest with .sh must fail validate");
    assert!(
        matches!(
            err_ui,
            CliError::Package(PackageError::ForbiddenExecutable(_))
        ),
        "expected ForbiddenExecutable for ui.asset_manifest.sh, got: {err_ui:?}"
    );
}

#[test]
fn validate_rejects_bad_ui_asset_manifest_before_ui_dist_is_built() {
    let tmp = TempDir::new().expect("tmp");

    // 1. ui.entry == ui.asset_manifest
    let eq_proj = tmp.path().join("ui_eq_entry_manifest");
    write_project(
        &eq_proj,
        r#"manifest_version = 1
id = "org.example.ui-eq"
name = "UI Eq"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Tester"]
description = "Test"
config_schema = "schemas/config.schema.json"
state_schema = 1

[ui]
kind = "federation-react"
entry = "ui/assets/remoteEntry.js"
asset_manifest = "ui/assets/remoteEntry.js"
remote_name = "cmUiEq"
contract = "1.0.0"
react = "19.3.0"
react_dom = "19.3.0"
capabilities = []

[[ui.surfaces]]
id = "main"
kind = "page"
expose = "./Page"
title = "Main"
"#,
        &[
            ("schemas/config.schema.json", MIN_SCHEMA.as_bytes()),
            ("ui/package.json", b"{}"),
        ],
    );
    let err_eq = run(&args(&["validate", &eq_proj.to_string_lossy()]))
        .expect_err("ui.entry == ui.asset_manifest must fail validate before build");
    assert!(
        matches!(
            err_eq,
            CliError::Package(PackageError::InvalidUiManifest(_))
        ),
        "expected InvalidUiManifest, got: {err_eq:?}"
    );

    // 2. is_forbidden_ui_asset_path(&ui.asset_manifest) (package.json, .html, .map)
    for bad_manifest in ["ui/package.json", "ui/index.html", "ui/bundle.js.map"] {
        let slug = bad_manifest.replace(['/', '.'], "-");
        let p = tmp.path().join(format!("ui_forbid_{slug}"));
        write_project(
            &p,
            &format!(
                r#"manifest_version = 1
id = "org.example.ui-forbid"
name = "UI Forbid"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Tester"]
description = "Test"
config_schema = "schemas/config.schema.json"
state_schema = 1

[ui]
kind = "federation-react"
entry = "ui/assets/remoteEntry.js"
asset_manifest = "{bad_manifest}"
remote_name = "cmUiForbid"
contract = "1.0.0"
react = "19.3.0"
react_dom = "19.3.0"
capabilities = []

[[ui.surfaces]]
id = "main"
kind = "page"
expose = "./Page"
title = "Main"
"#
            ),
            &[
                ("schemas/config.schema.json", MIN_SCHEMA.as_bytes()),
                ("ui/package.json", b"{}"),
            ],
        );
        let err = run(&args(&["validate", &p.to_string_lossy()]))
            .expect_err("forbidden ui.asset_manifest path must fail validate before build");
        assert!(
            matches!(err, CliError::Package(PackageError::ForbiddenUiAsset(_))),
            "expected ForbiddenUiAsset for `{bad_manifest}`, got: {err:?}"
        );
    }
}

#[test]
fn validate_rejects_nfc_and_case_folded_path_collisions() {
    let tmp = TempDir::new().expect("tmp");

    // 1. Case-folded collision against root `plugin.toml`
    let p_root_col = tmp.path().join("col_root");
    write_project(
        &p_root_col,
        r#"manifest_version = 1
id = "org.example.col-root"
name = "Col Root"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Tester"]
description = "Test"
config_schema = "Plugin.toml"
state_schema = 1
sites = ["sites/custom.yaml"]
"#,
        &[("sites/custom.yaml", VALID_SITE_YAML.as_bytes())],
    );
    let err_root = run(&args(&["validate", &p_root_col.to_string_lossy()]))
        .expect_err("Plugin.toml must collide with plugin.toml");
    assert!(
        matches!(
            err_root,
            CliError::Package(PackageError::PathCollision { .. })
        ),
        "expected PathCollision for Plugin.toml, got: {err_root:?}"
    );

    // 2. NFC vs NFD collision across declared schema paths
    let nfc_schema = "schemas/caf\u{00e9}.schema.json";
    let nfd_schema = "schemas/cafe\u{0301}.schema.json";
    let p_nfc = tmp.path().join("col_nfc");
    write_project(
        &p_nfc,
        &format!(
            r#"manifest_version = 1
id = "org.example.col-nfc"
name = "Col NFC"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Tester"]
description = "Test"
config_schema = "{nfc_schema}"
state_schema = 1

[[components]]
id = "main"
entry = "components/main.wasm"
world = "crawler:plugin/task-plugin@1.0.0"
permissions = []

[[components.tasks]]
id = "job-a"
input_schema = "{nfd_schema}"

[components.tasks.schedule]
kind = "interval"
seconds = 3600
"#
        ),
        &[
            (nfc_schema, MIN_SCHEMA.as_bytes()),
            (nfd_schema, MIN_SCHEMA.as_bytes()),
            (
                "Cargo.toml",
                b"[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
            ),
        ],
    );
    let err_nfc = run(&args(&["validate", &p_nfc.to_string_lossy()]))
        .expect_err("NFC vs NFD schema paths must fail with PathCollision");
    assert!(
        matches!(
            err_nfc,
            CliError::Package(PackageError::PathCollision { .. })
        ),
        "expected PathCollision for NFC/NFD, got: {err_nfc:?}"
    );
}

#[test]
fn validate_rejects_dir_conflicts_while_allowing_exact_shared_schemas() {
    let tmp = TempDir::new().expect("tmp");

    // 1. File-vs-parent-directory conflict across unbuilt component entries (case-folded)
    let p_dir_conflict = tmp.path().join("dir_conflict");
    write_project(
        &p_dir_conflict,
        r#"manifest_version = 1
id = "org.example.dir-conflict"
name = "Dir Conflict"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Tester"]
description = "Test"
config_schema = "schemas/config.schema.json"
state_schema = 1

[[components]]
id = "comp-a"
entry = "components/main.wasm"
world = "crawler:plugin/hook-plugin@1.0.0"
permissions = []

[[components.hooks]]
id = "search.after"
priority = 100

[[components]]
id = "comp-b"
entry = "Components/main.wasm/nested.wasm"
world = "crawler:plugin/task-plugin@1.0.0"
permissions = []

[[components.tasks]]
id = "job-dir"
input_schema = "schemas/config.schema.json"

[components.tasks.schedule]
kind = "interval"
seconds = 3600
"#,
        &[
            ("schemas/config.schema.json", MIN_SCHEMA.as_bytes()),
            (
                "Cargo.toml",
                b"[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
            ),
        ],
    );
    let err_dir = run(&args(&["validate", &p_dir_conflict.to_string_lossy()]))
        .expect_err("file-vs-parent-directory conflict must fail validate");
    assert!(
        matches!(err_dir, CliError::Package(PackageError::InvalidArchive(_))),
        "expected InvalidArchive for file-vs-parent-dir conflict, got: {err_dir:?}"
    );

    // 2. Positive control: two tasks sharing the EXACT same input_schema path must still pass
    let p_shared = tmp.path().join("shared_exact_schema");
    write_project(
        &p_shared,
        r#"manifest_version = 1
id = "org.example.shared-schema"
name = "Shared Schema"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Tester"]
description = "Test"
config_schema = "schemas/config.schema.json"
state_schema = 1

[[components]]
id = "main"
entry = "components/main.wasm"
world = "crawler:plugin/task-plugin@1.0.0"
permissions = []

[[components.tasks]]
id = "job-a"
input_schema = "schemas/shared.schema.json"

[components.tasks.schedule]
kind = "interval"
seconds = 3600

[[components.tasks]]
id = "job-b"
input_schema = "schemas/shared.schema.json"

[components.tasks.schedule]
kind = "interval"
seconds = 3600
"#,
        &[
            ("schemas/config.schema.json", MIN_SCHEMA.as_bytes()),
            ("schemas/shared.schema.json", MIN_SCHEMA.as_bytes()),
            (
                "Cargo.toml",
                b"[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
            ),
        ],
    );
    run(&args(&["validate", &p_shared.to_string_lossy()]))
        .expect("identical shared schema path must remain valid");
}

#[test]
fn scaffold_trims_trailing_hyphen_after_32_char_slug_truncation() {
    let tmp = TempDir::new().expect("tmp");

    // 31 'a's followed by '-' at char 32 and suffix: truncating to 32 chars used to leave trailing '-'
    let tricky_dir = tmp.path().join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-overflow");
    run(&args(&[
        "scaffold",
        "--api",
        "1.0.0",
        "--ui",
        "react",
        &tricky_dir.to_string_lossy(),
    ]))
    .expect("scaffold must succeed when 32nd slug char is '-'");

    run(&args(&["validate", &tricky_dir.to_string_lossy()]))
        .expect("scaffolded 32-char hyphen-boundary project must validate");

    let raw_toml = fs::read_to_string(tricky_dir.join("plugin.toml")).expect("read plugin.toml");
    let manifest = validate_manifest_toml_with_policy(&raw_toml, &PackagePolicy::default())
        .expect("valid manifest");
    assert_eq!(manifest.id, "org.example.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    assert!(!manifest.id.ends_with('-'));

    // Punctuation-only dir name falls back cleanly to `org.example.plugin`
    let punct_dir = tmp.path().join("___");
    run(&args(&[
        "scaffold",
        "--api",
        "1.0.0",
        "--ui",
        "react",
        &punct_dir.to_string_lossy(),
    ]))
    .expect("scaffold punctuation-only dir must fall back to plugin");
    let punct_toml = fs::read_to_string(punct_dir.join("plugin.toml")).expect("read plugin.toml");
    let punct_manifest = validate_manifest_toml_with_policy(&punct_toml, &PackagePolicy::default())
        .expect("valid fallback manifest");
    assert_eq!(punct_manifest.id, "org.example.plugin");
}
