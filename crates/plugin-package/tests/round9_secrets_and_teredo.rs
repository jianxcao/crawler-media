use plugin_package::{
    PackagePolicy, SecretWriteAction, validate_config_schema, validate_manifest_toml_with_policy,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn optional_nested_object_rejects_partial_clear_when_sibling_secret_kept_configured() {
    let raw_schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "auth": {
                "type": "object",
                "additionalProperties": false,
                "required": ["token", "secret_key"],
                "properties": {
                    "token": { "type": "string", "writeOnly": true },
                    "secret_key": { "type": "string", "writeOnly": true }
                }
            }
        }
    });
    let schema = validate_config_schema(&raw_schema).expect("valid schema");

    let existing = BTreeSet::from(["auth.token".to_string(), "auth.secret_key".to_string()]);

    // Clearing only `auth.secret_key` while `auth.token` stays configured via `Keep` must fail `required`
    let mut partial_clear = BTreeMap::new();
    partial_clear.insert("auth.secret_key".to_string(), SecretWriteAction::Clear);
    let errs = schema
        .resolve_config_write(&json!({}), &partial_clear, &existing)
        .expect_err("partial clear of required sibling secret must fail");
    assert!(
        errs.iter()
            .any(|e| e.path == "/secrets/auth.secret_key" && e.code == "required"),
        "expected /secrets/auth.secret_key required error, got: {errs:?}"
    );

    // Clearing BOTH secrets in optional `auth` when `values = {}` omits `auth` cleanly and succeeds
    let mut full_clear = BTreeMap::new();
    full_clear.insert("auth.token".to_string(), SecretWriteAction::Clear);
    full_clear.insert("auth.secret_key".to_string(), SecretWriteAction::Clear);
    schema
        .resolve_config_write(&json!({}), &full_clear, &existing)
        .expect("clearing all secrets in optional object must succeed");
}

#[test]
fn manifest_rejects_teredo_cloud_metadata_and_link_local_origins() {
    for bad_origin in [
        // Teredo client IPv4 = 169.254.169.254 (0xa9fe ^ 0xffff = 0x5601)
        "http://[2001:0:4136:e378:8000:63bf:5601:5601]",
        // Teredo server IPv4 = 169.254.169.254
        "http://[2001:0:a9fe:a9fe::1]",
        // Teredo client IPv4 = 100.100.100.200 (0x6464 ^ 0xffff = 0x9b9b, 0x64c8 ^ 0xffff = 0x9b37)
        "http://[2001:0:4136:e378:8000:63bf:9b9b:9b37]",
        // Teredo client IPv4 = 0.0.0.0 (0x0000 ^ 0xffff = 0xffff)
        "http://[2001:0:4136:e378:8000:63bf:ffff:ffff]",
    ] {
        let toml = format!(
            r#"manifest_version = 1
id = "org.example.r9-teredo"
name = "R9 Teredo"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Tester"]
description = "Teredo test"
config_schema = "schemas/config.schema.json"
state_schema = 1

[[http]]
origin = "{bad_origin}"
path_prefix = "/api"
methods = ["GET"]

[[components]]
id = "policy"
entry = "components/policy.wasm"
world = "crawler:plugin/hook-plugin@1.0.0"
permissions = ["hooks:search.after"]

[[components.hooks]]
id = "search.after"
priority = 100
"#
        );
        let res = validate_manifest_toml_with_policy(&toml, &PackagePolicy::default());
        assert!(
            res.is_err(),
            "manifest must reject Teredo forbidden origin `{bad_origin}`"
        );
    }
}

#[test]
fn active_descendant_secret_enforces_required_non_secret_fields_on_omitted_optional_ancestor() {
    let raw_schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "auth": {
                "type": "object",
                "additionalProperties": false,
                "required": ["username", "token"],
                "properties": {
                    "username": { "type": "string", "minLength": 1 },
                    "token": { "type": "string", "writeOnly": true }
                }
            }
        }
    });
    let schema = validate_config_schema(&raw_schema).expect("valid schema");

    // 1. Replace `auth.token` while omitting `values.auth` must fail required `/values/auth/username`
    let mut rep = BTreeMap::new();
    rep.insert(
        "auth.token".to_string(),
        SecretWriteAction::Replace("s3cr3t".into()),
    );
    let errs_a = schema
        .resolve_config_write(&json!({}), &rep, &BTreeSet::new())
        .expect_err("Replace on auth.token with values={} must require /values/auth/username");
    assert!(
        errs_a
            .iter()
            .any(|e| e.path == "/values/auth/username" && e.code == "required"),
        "expected /values/auth/username required error, got: {errs_a:?}"
    );

    // 2. Keep configured `auth.token` while wiping `values = {}` must also fail required `/values/auth/username`
    let existing = BTreeSet::from(["auth.token".to_string()]);
    let errs_b = schema
        .resolve_config_write(&json!({}), &BTreeMap::new(), &existing)
        .expect_err(
            "Keep on configured auth.token with values={} must require /values/auth/username",
        );
    assert!(
        errs_b
            .iter()
            .any(|e| e.path == "/values/auth/username" && e.code == "required"),
        "expected /values/auth/username required error, got: {errs_b:?}"
    );

    // 3. Clearing `auth.token` while `values = {}` succeeds cleanly with `public_values = {}`
    let mut clr = BTreeMap::new();
    clr.insert("auth.token".to_string(), SecretWriteAction::Clear);
    let ok = schema
        .resolve_config_write(&json!({}), &clr, &existing)
        .expect("clearing auth.token with values={} must succeed");
    assert_eq!(ok.public_values, json!({}));
}

#[test]
fn manifest_rejects_subnetted_rfc8215_nat64_forbidden_origins() {
    for bad_origin in [
        "http://[64:ff9b:1:1:100::a9fe:a9fe]",
        "http://[64:ff9b:1:808::a9fe:a9fe]",
        "http://[64:ff9b:1:808:a9:fefa:9fe0:0]",
        "http://[64:ff9b:1:8a9:fe:a9fe:0:0]",
        "http://[64:ff9b:1:808::]",
    ] {
        let toml = format!(
            r#"manifest_version = 1
id = "org.example.r11-nat64"
name = "R11 NAT64"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Tester"]
description = "RFC 8215 NAT64 test"
config_schema = "schemas/config.schema.json"
state_schema = 1

[[http]]
origin = "{bad_origin}"
path_prefix = "/api"
methods = ["GET"]

[[components]]
id = "policy"
entry = "components/policy.wasm"
world = "crawler:plugin/hook-plugin@1.0.0"
permissions = ["hooks:search.after"]

[[components.hooks]]
id = "search.after"
priority = 100
"#
        );
        let res = validate_manifest_toml_with_policy(&toml, &PackagePolicy::default());
        assert!(
            res.is_err(),
            "manifest must reject subnetted RFC 8215 NAT64 forbidden origin `{bad_origin}`"
        );
    }
}

#[test]
fn resolve_config_write_enforces_required_ancestor_objects_and_produces_self_valid_public_values() {
    let raw_req = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "required": ["auth"],
        "properties": {
            "auth": {
                "type": "object",
                "additionalProperties": false,
                "required": ["token"],
                "properties": {
                    "token": { "type": "string", "writeOnly": true },
                    "label": { "type": "string" }
                }
            }
        }
    });
    let schema = validate_config_schema(&raw_req).expect("valid schema");
    let rep = BTreeMap::from([(
        "auth.token".to_string(),
        SecretWriteAction::Replace("s3cr3t".into()),
    )]);

    let errs = schema
        .resolve_config_write(&json!({}), &rep, &BTreeSet::new())
        .expect_err("omitted required ancestor `auth` must fail `/values/auth` required");
    assert!(
        errs.iter()
            .any(|e| e.path == "/values/auth" && e.code == "required"),
        "expected /values/auth required error, got: {errs:?}"
    );

    let ok = schema
        .resolve_config_write(&json!({ "auth": {} }), &rep, &BTreeSet::new())
        .expect("explicit `auth: {}` with active secret must succeed");
    assert_eq!(ok.public_values, json!({ "auth": {} }));
    schema
        .validate_values(&ok.public_values, false)
        .expect("persisted public_values must pass validate_values(false)");

    let raw_deep = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "auth": {
                "type": "object",
                "additionalProperties": false,
                "required": ["username", "inner"],
                "properties": {
                    "username": { "type": "string" },
                    "inner": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["token"],
                        "properties": {
                            "token": { "type": "string", "writeOnly": true }
                        }
                    }
                }
            }
        }
    });
    let deep_schema = validate_config_schema(&raw_deep).expect("valid deep schema");
    let deep_rep = BTreeMap::from([(
        "auth.inner.token".to_string(),
        SecretWriteAction::Replace("s3cr3t".into()),
    )]);
    let deep_errs = deep_schema
        .resolve_config_write(
            &json!({ "auth": { "username": "alice" } }),
            &deep_rep,
            &BTreeSet::new(),
        )
        .expect_err("omitted required nested `auth.inner` must fail `/values/auth/inner` required");
    assert!(
        deep_errs
            .iter()
            .any(|e| e.path == "/values/auth/inner" && e.code == "required"),
        "expected /values/auth/inner required error, got: {deep_errs:?}"
    );
}
