mod common;

use common::sha256_hex;
use plugin_package::{
    ArchiveEntryMeta, PackageError, SecretWriteAction, validate_config_schema,
    validate_manifest_toml, validate_ui_manifest,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

fn base_hook_manifest(extra: &str) -> String {
    format!(
        r#"manifest_version = 1
id = "org.example.hardening"
name = "Hardening Test"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Tester"]
description = "Hardening test plugin"
config_schema = "schemas/config.schema.json"
state_schema = 1
{extra}
[[components]]
id = "policy"
entry = "components/policy.wasm"
world = "crawler:plugin/hook-plugin@1.0.0"
permissions = ["hooks:search.after"]

[[components.hooks]]
id = "search.after"
priority = 100
"#
    )
}

fn base_ui_manifest(remote_name: &str, expose: &str) -> String {
    format!(
        r#"manifest_version = 1
id = "org.example.ui-hardening"
name = "UI Hardening"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Tester"]
description = "UI hardening test plugin"
config_schema = "schemas/config.schema.json"
state_schema = 1

[ui]
kind = "federation-react"
contract = "1.0.0"
remote_name = "{remote_name}"
entry = "ui/assets/remoteEntry.js"
asset_manifest = "ui/asset-manifest.json"
react = "19.3.0"
react_dom = "19.3.0"
capabilities = ["read:sites"]

[[ui.surfaces]]
id = "config"
kind = "config"
expose = "{expose}"
title = "Settings"
"#
    )
}

#[test]
fn hard_01_ref_sibling_property_redeclaration_rejected() {
    let nested_shadow = json!({
        "$defs": {
            "AuthBase": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "token": { "type": "string", "writeOnly": true, "minLength": 16 }
                }
            }
        },
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "auth": {
                "$ref": "#/$defs/AuthBase",
                "properties": {
                    "token": { "type": "string" }
                }
            }
        }
    });
    assert!(matches!(
        validate_config_schema(&nested_shadow),
        Err(PackageError::InvalidSchema(_))
    ));

    let nested_shadow_default = json!({
        "$defs": {
            "AuthBase": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "token": { "type": "string", "writeOnly": true, "minLength": 16 }
                }
            }
        },
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "auth": {
                "$ref": "#/$defs/AuthBase",
                "default": { "token": "leaked" },
                "properties": {
                    "token": { "type": "string" }
                }
            }
        }
    });
    assert!(matches!(
        validate_config_schema(&nested_shadow_default),
        Err(PackageError::InvalidSchema(_))
    ));

    let root_shadow = json!({
        "$defs": {
            "RootBase": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "token": { "type": "string", "writeOnly": true }
                }
            }
        },
        "$ref": "#/$defs/RootBase",
        "properties": {
            "token": { "type": "string" }
        }
    });
    assert!(matches!(
        validate_config_schema(&root_shadow),
        Err(PackageError::InvalidSchema(_))
    ));

    let array_items_shadow = json!({
        "$defs": {
            "TagList": {
                "type": "array",
                "items": { "type": "string", "minLength": 2 }
            }
        },
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "tags": {
                "$ref": "#/$defs/TagList",
                "items": { "type": "string" }
            }
        }
    });
    assert!(matches!(
        validate_config_schema(&array_items_shadow),
        Err(PackageError::InvalidSchema(_))
    ));
}

#[test]
fn hard_02_ref_disjoint_sibling_properties_and_target_required() {
    let nested_raw = json!({
        "$defs": {
            "Base": {
                "type": "object",
                "additionalProperties": false,
                "required": ["token"],
                "properties": {
                    "host": { "type": "string" }
                }
            }
        },
        "type": "object",
        "additionalProperties": false,
        "required": ["auth"],
        "properties": {
            "auth": {
                "$ref": "#/$defs/Base",
                "properties": {
                    "token": { "type": "string", "writeOnly": true, "minLength": 16 }
                }
            }
        }
    });
    let nested = validate_config_schema(&nested_raw).expect("disjoint extension must pass");
    assert_eq!(nested.secret_fields().len(), 1);
    assert_eq!(nested.secret_fields()[0].key, "auth.token");
    assert!(nested.secret_fields()[0].required);

    let err_req = nested
        .resolve_config_write(
            &json!({ "auth": { "host": "https://a.test" } }),
            &BTreeMap::new(),
            &BTreeSet::new(),
        )
        .unwrap_err();
    assert!(
        err_req
            .iter()
            .any(|e| e.code == "required" && e.path == "/secrets/auth.token")
    );

    let mut short_actions = BTreeMap::new();
    short_actions.insert(
        "auth.token".into(),
        SecretWriteAction::Replace("short".into()),
    );
    let err_min = nested
        .resolve_config_write(
            &json!({ "auth": { "host": "https://a.test" } }),
            &short_actions,
            &BTreeSet::new(),
        )
        .unwrap_err();
    assert!(
        err_min
            .iter()
            .any(|e| e.code == "minLength" && e.path == "/secrets/auth.token")
    );

    let mut ok_actions = BTreeMap::new();
    ok_actions.insert(
        "auth.token".into(),
        SecretWriteAction::Replace("valid-16-char-tok".into()),
    );
    nested
        .resolve_config_write(
            &json!({ "auth": { "host": "https://a.test" } }),
            &ok_actions,
            &BTreeSet::new(),
        )
        .expect("valid secret length must pass");

    let root_raw = json!({
        "$defs": {
            "RootBase": {
                "type": "object",
                "additionalProperties": false,
                "required": ["api_key"]
            }
        },
        "$ref": "#/$defs/RootBase",
        "properties": {
            "api_key": { "type": "string", "writeOnly": true }
        }
    });
    let root_schema = validate_config_schema(&root_raw).expect("root disjoint ref must pass");
    assert_eq!(root_schema.secret_fields().len(), 1);
    assert!(root_schema.secret_fields()[0].required);
    assert!(
        root_schema
            .resolve_config_write(&json!({}), &BTreeMap::new(), &BTreeSet::new())
            .is_err()
    );
}

#[test]
fn hard_02b_ref_multihop_target_required() {
    let multihop_raw = json!({
        "$defs": {
            "Base": {
                "type": "object",
                "additionalProperties": false,
                "required": ["token"]
            },
            "Mid": {
                "$ref": "#/$defs/Base"
            }
        },
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "auth": {
                "$ref": "#/$defs/Mid",
                "properties": {
                    "token": { "type": "string", "writeOnly": true }
                }
            }
        }
    });
    let multihop = validate_config_schema(&multihop_raw).expect("multi-hop ref must pass");
    assert!(multihop.secret_fields()[0].required);
}

#[test]
fn hard_03_http_origin_metadata_invisible_port0_and_private_allowed() {
    let bad_origins = [
        "http://[fd00:ec2::254]",
        "http://metadata.google.internal",
        "http://metadata.google.internal.",
        "http://sub.metadata.goog",
        "http://100.100.100.200",
        "http://168.63.129.16",
        "http://0.0.0.0",
        "http://[::]",
        "http://[::169.254.169.254]",
        "https://example.com:0",
        "https://example.com.",
        "https://example.com\u{200B}",
        "https://\u{FEFF}example.com",
        "https://\u{FF45}\u{FF58}\u{FF41}\u{FF4D}\u{FF50}\u{FF4C}\u{FF45}.com",
        "http://2852039166",
    ];
    for origin in bad_origins {
        let toml = base_hook_manifest(&format!(
            "[[http]]\norigin = \"{origin}\"\npath_prefix = \"/api\"\nmethods = [\"GET\"]\n"
        ));
        let err = validate_manifest_toml(&toml)
            .expect_err(&format!("expected rejection for http origin `{origin}`"));
        assert!(
            matches!(err, PackageError::InvalidManifest(_)),
            "expected InvalidManifest for `{origin}`, got {err:?}"
        );
    }

    let ok_origins = [
        "http://127.0.0.1:8080",
        "http://10.0.0.1",
        "http://[::1]:8080",
        "https://example.com:443",
        "https://m\u{00FC}nchen.de",
    ];
    for origin in ok_origins {
        let toml = base_hook_manifest(&format!(
            "[[http]]\norigin = \"{origin}\"\npath_prefix = \"/api\"\nmethods = [\"GET\"]\n"
        ));
        validate_manifest_toml(&toml)
            .unwrap_or_else(|e| panic!("valid origin `{origin}` failed: {e:?}"));
    }
}

#[test]
fn hard_04_http_path_prefix_control_whitespace_backslash_rejected() {
    let bad_prefixes = [
        r#""/api\nX-Injected: 1""#,
        r#""/api\r\n""#,
        r#""/api\\evil""#,
        r#""/api\u0000""#,
        r#""/api foo""#,
        "\"/api\u{200B}\"",
    ];
    for prefix in bad_prefixes {
        let toml = base_hook_manifest(&format!(
            "[[http]]\norigin = \"https://example.com\"\npath_prefix = {prefix}\nmethods = [\"GET\"]\n"
        ));
        let err = validate_manifest_toml(&toml)
            .expect_err(&format!("expected rejection for path_prefix {prefix}"));
        assert!(
            matches!(err, PackageError::InvalidManifest(_)),
            "expected InvalidManifest for path_prefix {prefix}, got {err:?}"
        );
    }
}

#[test]
fn hard_05_ui_expose_asset_query_frag_and_js_globals_rejected() {
    for bad_expose in [
        "./../evil",
        "./Config\\n",
        "./Config ",
        "./a\\\\b",
        "./Config?x=1",
    ] {
        let toml = base_ui_manifest("cm_valid_name", bad_expose);
        let err = validate_manifest_toml(&toml)
            .expect_err(&format!("expected rejection for expose `{bad_expose}`"));
        assert!(
            matches!(err, PackageError::InvalidUiManifest(_)),
            "expected InvalidUiManifest for expose `{bad_expose}`, got {err:?}"
        );
    }

    for bad_asset_path in ["ui/assets/foo?x=1.js", "ui/assets/foo#bar.js"] {
        let payload = b"export default 1;";
        let metas = vec![ArchiveEntryMeta {
            path: bad_asset_path.to_string(),
            size: payload.len() as u64,
            sha256_hex: sha256_hex(payload),
        }];
        let manifest_json = json!({
            "version": 1,
            "files": [
                {
                    "path": bad_asset_path,
                    "sha256": sha256_hex(payload),
                    "size": payload.len(),
                    "content_type": "application/javascript"
                }
            ]
        });
        let err = validate_ui_manifest(&manifest_json, &metas)
            .expect_err(&format!("expected rejection for asset `{bad_asset_path}`"));
        assert!(
            matches!(err, PackageError::UnsafePath { .. }),
            "expected UnsafePath for asset `{bad_asset_path}`, got {err:?}"
        );
    }

    for global_name in [
        "Object", "Array", "Function", "Promise", "fetch", "console", "require", "module",
        "exports",
    ] {
        let toml = base_ui_manifest(global_name, "./Config");
        let err = validate_manifest_toml(&toml).expect_err(&format!(
            "expected rejection for remote_name `{global_name}`"
        ));
        assert!(
            matches!(err, PackageError::InvalidUiManifest(_)),
            "expected InvalidUiManifest for remote_name `{global_name}`, got {err:?}"
        );
    }
}

#[test]
fn hard_06_nat64_and_6to4_forbidden_ipv6_origins_rejected() {
    for bad_origin in [
        "http://[64:ff9b::a9fe:a9fe]",
        "http://[64:ff9b:1::a9fe:a9fe]",
        "http://[2002:a9fe:a9fe::1]",
        "http://[64:ff9b::6464:64c8]",
        "http://[2002:a83f:8110::1]",
    ] {
        let toml = base_hook_manifest(&format!(
            "[[http]]\norigin = \"{bad_origin}\"\npath_prefix = \"/\"\nmethods = [\"GET\"]\n"
        ));
        let err = validate_manifest_toml(&toml).expect_err(&format!(
            "expected rejection for forbidden origin `{bad_origin}`"
        ));
        assert!(
            matches!(err, PackageError::InvalidManifest(_)),
            "expected InvalidManifest for `{bad_origin}`, got {err:?}"
        );
    }
}
