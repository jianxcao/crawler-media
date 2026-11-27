mod common;

use common::{ZipEntrySpec, build_zip, dev_policy, minimal_config_schema_json, sha256_hex};
use plugin_package::{
    ArchiveEntryMeta, PackageError, PackagePolicy, validate_cmplugin_bytes, validate_manifest_toml,
    validate_ui_manifest,
};
use serde_json::json;

fn valid_ui_toml() -> String {
    r#"manifest_version = 1
id = "org.example.checkin-ui"
name = "Checkin UI"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Example"]
description = "UI-only plugin"
config_schema = "schemas/config.schema.json"
state_schema = 1

[ui]
kind = "federation-react"
contract = "1.0.0"
remote_name = "cm_example_checkin"
entry = "ui/assets/remoteEntry.js"
asset_manifest = "ui/asset-manifest.json"
react = "19.3.0"
react_dom = "19.3.0"
capabilities = ["read:sites", "read:history"]

[[ui.surfaces]]
id = "config"
kind = "config"
expose = "./Config"
title = "Settings"

[[ui.surfaces]]
id = "checkin"
kind = "app"
expose = "./AppPage"
title = "Check-in"
icon = "calendar-check"
"#
    .to_string()
}

fn build_ui_only_zip(extra_entries: Vec<ZipEntrySpec>) -> Vec<u8> {
    let remote_js = b"export const init = () => {};";
    let chunk_js = b"export default function Page() { return null; }";
    let style_css = b"body { color: #fff; }";
    let asset_manifest = json!({
        "version": 1,
        "files": [
            {
                "path": "ui/assets/remoteEntry.js",
                "sha256": sha256_hex(remote_js),
                "size": remote_js.len(),
                "content_type": "application/javascript"
            },
            {
                "path": "ui/assets/page.mjs",
                "sha256": sha256_hex(chunk_js),
                "size": chunk_js.len(),
                "content_type": "text/javascript"
            },
            {
                "path": "ui/assets/style.css",
                "sha256": sha256_hex(style_css),
                "size": style_css.len(),
                "content_type": "text/css"
            }
        ]
    });
    let mut entries = vec![
        ZipEntrySpec::stored("plugin.toml", valid_ui_toml().as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored(
            "ui/asset-manifest.json",
            serde_json::to_vec(&asset_manifest)
                .expect("json")
                .as_slice(),
        ),
        ZipEntrySpec::stored("ui/assets/remoteEntry.js", remote_js),
        ZipEntrySpec::stored("ui/assets/page.mjs", chunk_js),
        ZipEntrySpec::stored("ui/assets/style.css", style_css),
    ];
    entries.extend(extra_entries);
    build_zip(&entries)
}

#[test]
fn ui_01_and_man_10_valid_ui_only_package() {
    let zip = build_ui_only_zip(vec![]);
    let pkg = validate_cmplugin_bytes(&zip, dev_policy()).expect("valid ui-only package");
    assert!(pkg.components.is_empty());
    let ui = pkg.ui.expect("ui bundle");
    assert_eq!(ui.declaration.remote_name, "cm_example_checkin");
    assert_eq!(ui.asset_manifest.files.len(), 3);
    assert_eq!(ui.assets.len(), 3);
}

#[test]
fn ui_02_contract_and_react_version_mismatch_rejected() {
    for (field, val) in [
        ("kind = \"federation-react\"", "kind = \"iframe\""),
        ("contract = \"1.0.0\"", "contract = \"^1.0.0\""),
        ("contract = \"1.0.0\"", "contract = \"2.0.0\""),
        ("react = \"19.3.0\"", "react = \"19.1.1\""),
        ("react_dom = \"19.3.0\"", "react_dom = \"19.2.0\""),
    ] {
        let toml = valid_ui_toml().replace(field, val);
        assert!(matches!(
            validate_manifest_toml(&toml),
            Err(PackageError::InvalidUiManifest(_))
        ));
    }
}

#[test]
fn ui_03_remote_name_js_identifier_and_reserved_rejected() {
    for bad in [
        "cm-bad",
        "123abc",
        "",
        "host",
        "web",
        "default",
        "window",
        "undefined",
        "NaN",
        "Infinity",
    ] {
        let toml = valid_ui_toml().replace("cm_example_checkin", bad);
        assert!(
            matches!(
                validate_manifest_toml(&toml),
                Err(PackageError::InvalidUiManifest(_))
            ),
            "expected InvalidUiManifest for remote_name `{bad}`"
        );
    }
}

#[test]
fn ui_04_surface_rules_enforced() {
    let two_config = format!(
        "{}\n[[ui.surfaces]]\nid = \"cfg2\"\nkind = \"config\"\nexpose = \"./Config2\"\ntitle = \"Cfg2\"\n",
        valid_ui_toml()
    );
    assert!(matches!(
        validate_manifest_toml(&two_config),
        Err(PackageError::InvalidUiManifest(_))
    ));

    let bad_title = valid_ui_toml().replace("title = \"Check-in\"", "title = \"<b>Check-in</b>\"");
    assert!(matches!(
        validate_manifest_toml(&bad_title),
        Err(PackageError::InvalidUiManifest(_))
    ));

    let long_title = format!("title = \"{}\"", "a".repeat(81));
    let too_long_title = valid_ui_toml().replace("title = \"Check-in\"", &long_title);
    assert!(matches!(
        validate_manifest_toml(&too_long_title),
        Err(PackageError::InvalidUiManifest(_))
    ));

    let bad_icon =
        valid_ui_toml().replace("icon = \"calendar-check\"", "icon = \"nonexistent-icon\"");
    assert!(matches!(
        validate_manifest_toml(&bad_icon),
        Err(PackageError::InvalidUiManifest(_))
    ));

    let bad_expose = valid_ui_toml().replace("expose = \"./Config\"", "expose = \"Config\"");
    assert!(matches!(
        validate_manifest_toml(&bad_expose),
        Err(PackageError::InvalidUiManifest(_))
    ));

    let bad_surface_id = valid_ui_toml().replace("id = \"checkin\"", "id = \"Bad_Id\"");
    assert!(matches!(
        validate_manifest_toml(&bad_surface_id),
        Err(PackageError::InvalidUiManifest(_))
    ));

    let mut seventeen = valid_ui_toml();
    for i in 0..16 {
        seventeen.push_str(&format!(
            "\n[[ui.surfaces]]\nid = \"p{i}\"\nkind = \"page\"\nexpose = \"./Page{i}\"\ntitle = \"P{i}\"\n"
        ));
    }
    assert!(matches!(
        validate_manifest_toml(&seventeen),
        Err(PackageError::InvalidUiManifest(_))
    ));
}

#[test]
fn ui_05_ui_capabilities_and_task_run_cross_check() {
    let missing_task = valid_ui_toml().replace(
        "capabilities = [\"read:sites\", \"read:history\"]",
        "capabilities = [\"task:run:missing-task\"]",
    );
    assert!(matches!(
        validate_manifest_toml(&missing_task),
        Err(PackageError::InvalidUiManifest(_) | PackageError::UnknownCapability(_))
    ));

    let bad_ui_cap = valid_ui_toml().replace(
        "capabilities = [\"read:sites\", \"read:history\"]",
        "capabilities = [\"http:site-auth\"]",
    );
    assert!(matches!(
        validate_manifest_toml(&bad_ui_cap),
        Err(PackageError::InvalidUiManifest(_) | PackageError::UnknownCapability(_))
    ));
}

#[test]
fn ui_06_asset_manifest_integrity_checks() {
    let remote_js = b"export const init = () => {};";
    let entries = vec![ArchiveEntryMeta {
        path: "ui/assets/remoteEntry.js".into(),
        size: remote_js.len() as u64,
        sha256_hex: sha256_hex(remote_js),
    }];

    // Bad version
    let bad_ver = json!({ "version": 2, "files": [] });
    assert!(matches!(
        validate_ui_manifest(&bad_ver, &entries),
        Err(PackageError::InvalidUiManifest(_))
    ));

    // Wrong sha256
    let wrong_hash = json!({
        "version": 1,
        "files": [{
            "path": "ui/assets/remoteEntry.js",
            "sha256": "0".repeat(64),
            "size": remote_js.len(),
            "content_type": "application/javascript"
        }]
    });
    assert!(matches!(
        validate_ui_manifest(&wrong_hash, &entries),
        Err(PackageError::InvalidUiManifest(_))
    ));

    // Size mismatch
    let wrong_size = json!({
        "version": 1,
        "files": [{
            "path": "ui/assets/remoteEntry.js",
            "sha256": sha256_hex(remote_js),
            "size": remote_js.len() + 1,
            "content_type": "application/javascript"
        }]
    });
    assert!(matches!(
        validate_ui_manifest(&wrong_size, &entries),
        Err(PackageError::InvalidUiManifest(_))
    ));

    // Extra undeclared file under ui/ inside archive
    let zip_extra = build_ui_only_zip(vec![ZipEntrySpec::stored("ui/assets/undeclared.js", b"1")]);
    assert!(matches!(
        validate_cmplugin_bytes(&zip_extra, dev_policy()),
        Err(PackageError::InvalidUiManifest(_) | PackageError::ForbiddenExecutable(_))
    ));
}

#[test]
fn ui_07_forbidden_asset_extensions_and_mime_mismatch() {
    let bad_files = [
        ("ui/index.html", "text/html"),
        ("ui/assets/remoteEntry.js.map", "application/json"),
        ("ui/src/App.tsx", "text/plain"),
        ("ui/package.json", "application/json"),
    ];
    for (path, mime) in bad_files {
        let data = b"bad";
        let manifest = json!({
            "version": 1,
            "files": [{
                "path": path,
                "sha256": sha256_hex(data),
                "size": data.len(),
                "content_type": mime
            }]
        });
        let entries = vec![ArchiveEntryMeta {
            path: path.into(),
            size: data.len() as u64,
            sha256_hex: sha256_hex(data),
        }];
        let err = validate_ui_manifest(&manifest, &entries).unwrap_err();
        assert!(matches!(
            err,
            PackageError::ForbiddenUiAsset(_) | PackageError::InvalidUiManifest(_)
        ));
    }

    // MIME mismatch on allowed extension
    let js = b"export default 1;";
    let mime_mismatch = json!({
        "version": 1,
        "files": [{
            "path": "ui/assets/remoteEntry.js",
            "sha256": sha256_hex(js),
            "size": js.len(),
            "content_type": "text/html"
        }]
    });
    let entries = vec![ArchiveEntryMeta {
        path: "ui/assets/remoteEntry.js".into(),
        size: js.len() as u64,
        sha256_hex: sha256_hex(js),
    }];
    assert!(matches!(
        validate_ui_manifest(&mime_mismatch, &entries),
        Err(PackageError::ForbiddenUiAsset(_) | PackageError::InvalidUiManifest(_))
    ));
}

#[test]
fn ui_08_ui_asset_count_and_size_caps() {
    let mut files = Vec::new();
    let mut entries = Vec::new();
    let hash = sha256_hex(b"x");
    for i in 0..1025 {
        let p = format!("ui/assets/f{i}.js");
        files.push(json!({
            "path": p,
            "sha256": hash,
            "size": 1,
            "content_type": "application/javascript"
        }));
        entries.push(ArchiveEntryMeta {
            path: format!("ui/assets/f{i}.js"),
            size: 1,
            sha256_hex: hash.clone(),
        });
    }
    let too_many = json!({ "version": 1, "files": files });
    assert!(matches!(
        validate_ui_manifest(&too_many, &entries),
        Err(PackageError::UiAssetLimitExceeded(_))
    ));

    // > 8 MiB single file
    let big_manifest = json!({
        "version": 1,
        "files": [{
            "path": "ui/assets/big.js",
            "sha256": hash,
            "size": 8 * 1024 * 1024 + 1,
            "content_type": "application/javascript"
        }]
    });
    let big_entries = vec![ArchiveEntryMeta {
        path: "ui/assets/big.js".into(),
        size: 8 * 1024 * 1024 + 1,
        sha256_hex: hash.clone(),
    }];
    assert!(matches!(
        validate_ui_manifest(&big_manifest, &big_entries),
        Err(PackageError::UiAssetLimitExceeded(_))
    ));

    // > 32 MiB total across 5 files (each <= 8 MiB)
    let seven_mib = 7 * 1024 * 1024;
    let mut total_files = Vec::new();
    let mut total_entries = Vec::new();
    for i in 0..5 {
        let p = format!("ui/assets/chunk{i}.js");
        total_files.push(json!({
            "path": p,
            "sha256": hash,
            "size": seven_mib,
            "content_type": "application/javascript"
        }));
        total_entries.push(ArchiveEntryMeta {
            path: format!("ui/assets/chunk{i}.js"),
            size: seven_mib,
            sha256_hex: hash.clone(),
        });
    }
    let over_total = json!({ "version": 1, "files": total_files });
    assert!(matches!(
        validate_ui_manifest(&over_total, &total_entries),
        Err(PackageError::UiAssetLimitExceeded(_))
    ));
}

#[test]
fn ui_09_path_traversal_in_asset_manifest_rejected() {
    let hash = sha256_hex(b"x");
    for bad in [
        "../components/policy.wasm",
        "/abs/remoteEntry.js",
        "ui/assets/../remoteEntry.js",
    ] {
        let m = json!({
            "version": 1,
            "files": [{
                "path": bad,
                "sha256": hash,
                "size": 1,
                "content_type": "application/javascript"
            }]
        });
        assert!(matches!(
            validate_ui_manifest(&m, &[]),
            Err(PackageError::UnsafePath { .. })
        ));
    }
    let _ = PackagePolicy::default();
}

#[test]
fn ui_10_task_run_with_declared_task_and_reserved_remote_name_policy() {
    let toml_with_task = format!(
        "{}\n[[components]]\nid = \"tasks\"\nentry = \"components/tasks.wasm\"\nworld = \"crawler:plugin/task-plugin@1.0.0\"\n[[components.tasks]]\nid = \"daily\"\ninput_schema = \"schemas/config.schema.json\"\nschedule = {{ kind = \"interval\", seconds = 3600 }}\n",
        valid_ui_toml().replace(
            "capabilities = [\"read:sites\", \"read:history\"]",
            "capabilities = [\"read:sites\", \"task:run:daily\"]",
        )
    );
    let parsed = validate_manifest_toml(&toml_with_task).expect("task:run:daily allowed");
    assert!(parsed.ui.is_some());

    let mut policy = PackagePolicy::default();
    policy
        .reserved_remote_names
        .push("cm_example_checkin".into());
    let err =
        plugin_package::validate_manifest_toml_with_policy(&toml_with_task, &policy).unwrap_err();
    assert!(matches!(err, PackageError::InvalidUiManifest(_)));
}

#[test]
fn ui_11_reserved_remote_names_js_keywords_and_globals_rejected() {
    let reserved = [
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "function",
        "if",
        "import",
        "in",
        "instanceof",
        "new",
        "null",
        "return",
        "super",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "typeof",
        "var",
        "void",
        "while",
        "with",
        "let",
        "static",
        "yield",
        "await",
        "implements",
        "interface",
        "package",
        "private",
        "protected",
        "public",
        "eval",
        "arguments",
        "constructor",
        "__proto__",
        "prototype",
        "toString",
        "valueOf",
        "hasOwnProperty",
        "window",
        "globalThis",
        "self",
        "document",
        "top",
        "parent",
        "frames",
        "location",
        "navigator",
        "host",
        "web",
    ];
    for word in reserved {
        let toml = valid_ui_toml().replace("cm_example_checkin", word);
        assert!(
            matches!(
                validate_manifest_toml(&toml),
                Err(PackageError::InvalidUiManifest(_))
            ),
            "expected InvalidUiManifest for reserved remote_name `{word}`"
        );
    }
}

#[test]
fn ui_12_asset_manifest_must_use_ui_prefix_and_js_entry_and_valid_mime() {
    // 1. files[].path outside ui/ must fail
    let outside = b"export default 1;";
    let manifest_outside = json!({
        "version": 1,
        "files": [{
            "path": "schemas/evil.js",
            "sha256": sha256_hex(outside),
            "size": outside.len(),
            "content_type": "application/javascript"
        }]
    });
    let entries_outside = vec![ArchiveEntryMeta {
        path: "schemas/evil.js".into(),
        size: outside.len() as u64,
        sha256_hex: sha256_hex(outside),
    }];
    assert!(matches!(
        validate_ui_manifest(&manifest_outside, &entries_outside),
        Err(PackageError::ForbiddenUiAsset(_) | PackageError::InvalidUiManifest(_))
    ));

    // 2. Case-variant UI/ files in archive must fail
    for bad_path in ["UI/evil.html", "UI/extra.txt"] {
        let zip = build_ui_only_zip(vec![ZipEntrySpec::stored(bad_path, b"evil")]);
        assert!(matches!(
            validate_cmplugin_bytes(&zip, dev_policy()),
            Err(PackageError::ForbiddenUiAsset(_) | PackageError::InvalidUiManifest(_))
        ));
    }

    // 3. Non-JS ui.entry rejected in manifest
    let css_entry_toml = valid_ui_toml().replace(
        "entry = \"ui/assets/remoteEntry.js\"",
        "entry = \"ui/assets/style.css\"",
    );
    assert!(matches!(
        validate_manifest_toml(&css_entry_toml),
        Err(PackageError::InvalidUiManifest(_))
    ));

    // 4. Malformed MIME string rejected
    let js = b"export default 1;";
    for bad_mime in [
        "application/javascript;",
        "application/javascript; evil\nheader: x",
        "application/javascript; badparam",
    ] {
        let bad_manifest = json!({
            "version": 1,
            "files": [{
                "path": "ui/assets/remoteEntry.js",
                "sha256": sha256_hex(js),
                "size": js.len(),
                "content_type": bad_mime
            }]
        });
        let entries = vec![ArchiveEntryMeta {
            path: "ui/assets/remoteEntry.js".into(),
            size: js.len() as u64,
            sha256_hex: sha256_hex(js),
        }];
        assert!(
            matches!(
                validate_ui_manifest(&bad_manifest, &entries),
                Err(PackageError::ForbiddenUiAsset(_) | PackageError::InvalidUiManifest(_))
            ),
            "expected error for malformed MIME `{bad_mime}`"
        );
    }
}

#[test]
fn ui_13_phase2_commands_rejected_in_ui_capabilities_by_default() {
    let deferred_cmds = [
        "library.refresh",
        "transfer.request",
        "scrape.request",
        "site.create",
        "site.update",
        "site.delete",
        "subscribe.create",
        "subscribe.update",
        "subscribe.delete",
        "subscribe.complete",
    ];
    for cmd in deferred_cmds {
        let toml = valid_ui_toml().replace(
            "capabilities = [\"read:sites\", \"read:history\"]",
            &format!("capabilities = [\"read:sites\", \"command:{cmd}\"]"),
        );
        let err = validate_manifest_toml(&toml).unwrap_err();
        assert!(
            matches!(err, PackageError::InvalidUiManifest(_)),
            "expected InvalidUiManifest for deferred UI command `{cmd}`, got {err:?}"
        );

        let remote_js = b"export const init = () => {};";
        let asset_manifest = json!({
            "version": 1,
            "files": [{
                "path": "ui/assets/remoteEntry.js",
                "sha256": sha256_hex(remote_js),
                "size": remote_js.len(),
                "content_type": "application/javascript"
            }]
        });
        let zip = build_zip(&[
            ZipEntrySpec::stored("plugin.toml", toml.as_bytes()),
            ZipEntrySpec::stored(
                "schemas/config.schema.json",
                minimal_config_schema_json().as_bytes(),
            ),
            ZipEntrySpec::stored(
                "ui/asset-manifest.json",
                serde_json::to_vec(&asset_manifest)
                    .expect("json")
                    .as_slice(),
            ),
            ZipEntrySpec::stored("ui/assets/remoteEntry.js", remote_js),
        ]);
        assert!(
            matches!(
                validate_cmplugin_bytes(&zip, dev_policy()),
                Err(PackageError::InvalidUiManifest(_))
            ),
            "expected validate_cmplugin_bytes to reject UI command `{cmd}`"
        );
    }

    let phase1_cmds = [
        "indexer.search",
        "indexer.rss",
        "site.maintenance",
        "downloader.add",
        "downloader.control",
        "downloader.delete-task",
        "downloader.prepare-cleanup",
        "downloader.delete-files",
        "boost.run",
        "notify.send",
        "schedule.set",
        "schedule.disable",
    ];
    for cmd in phase1_cmds {
        let toml = valid_ui_toml().replace(
            "capabilities = [\"read:sites\", \"read:history\"]",
            &format!("capabilities = [\"command:{cmd}\"]"),
        );
        validate_manifest_toml(&toml)
            .unwrap_or_else(|e| panic!("Phase 1 UI command `{cmd}` failed: {e:?}"));
    }

    let mut widened = PackagePolicy::default();
    widened.allowed_commands.push("library.refresh".into());
    let widened_toml = valid_ui_toml().replace(
        "capabilities = [\"read:sites\", \"read:history\"]",
        "capabilities = [\"command:library.refresh\"]",
    );
    plugin_package::validate_manifest_toml_with_policy(&widened_toml, &widened)
        .expect("widened policy.allowed_commands allows extra UI command");
}
