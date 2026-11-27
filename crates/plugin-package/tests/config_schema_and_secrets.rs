use plugin_package::{
    PackageError, SecretWriteAction, project_config_with_secrets, split_config_secrets,
    validate_config_schema, validate_config_values,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn sch_01_valid_bounded_schema_with_local_defs_and_ref() {
    let raw = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$defs": {
            "SiteId": { "type": "string", "minLength": 1, "maxLength": 64 }
        },
        "type": "object",
        "additionalProperties": false,
        "required": ["sites", "interval_secs"],
        "properties": {
            "sites": {
                "type": "array",
                "minItems": 1,
                "maxItems": 8,
                "uniqueItems": true,
                "items": { "$ref": "#/$defs/SiteId" },
                "x-ui": { "widget": "site" }
            },
            "interval_secs": {
                "type": "integer",
                "minimum": 60,
                "maximum": 86400,
                "default": 3600,
                "x-ui": { "widget": "interval" }
            },
            "ratio": { "type": "number", "minimum": 0.5, "maximum": 10.0 },
            "mode": { "type": "string", "enum": ["fast", "safe"], "pattern": "^[a-z]+$" },
            "dry_run": { "type": "boolean", "default": true }
        }
    });
    let schema = validate_config_schema(&raw).expect("valid bounded schema");
    assert_eq!(schema.schema_revision().len(), 64);
    assert!(!schema.canonical_json().is_empty());
}

#[test]
fn sch_02_remote_and_non_local_refs_rejected() {
    for bad_ref in [
        "https://example.com/a.json",
        "http://example.com/a.json",
        "file:///etc/passwd",
        "../other.json",
        "#/properties/other",
    ] {
        let raw = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "x": { "$ref": bad_ref }
            }
        });
        assert!(
            matches!(
                validate_config_schema(&raw),
                Err(PackageError::InvalidSchema(_))
            ),
            "expected InvalidSchema for ref `{bad_ref}`"
        );
    }
}

#[test]
fn sch_03_recursive_and_dangling_defs_rejected() {
    let direct_cycle = json!({
        "$defs": {
            "Node": {
                "type": "object",
                "additionalProperties": false,
                "properties": { "next": { "$ref": "#/$defs/Node" } }
            }
        },
        "type": "object",
        "additionalProperties": false,
        "properties": { "root": { "$ref": "#/$defs/Node" } }
    });
    assert!(matches!(
        validate_config_schema(&direct_cycle),
        Err(PackageError::InvalidSchema(_))
    ));

    let mutual_cycle = json!({
        "$defs": {
            "A": { "type": "object", "additionalProperties": false, "properties": { "b": { "$ref": "#/$defs/B" } } },
            "B": { "type": "object", "additionalProperties": false, "properties": { "a": { "$ref": "#/$defs/A" } } }
        },
        "type": "object",
        "additionalProperties": false,
        "properties": { "start": { "$ref": "#/$defs/A" } }
    });
    assert!(matches!(
        validate_config_schema(&mutual_cycle),
        Err(PackageError::InvalidSchema(_))
    ));

    let dangling = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": { "x": { "$ref": "#/$defs/Missing" } }
    });
    assert!(matches!(
        validate_config_schema(&dangling),
        Err(PackageError::InvalidSchema(_))
    ));
}

#[test]
fn sch_04_excessive_nesting_depth_rejected() {
    let mut node = json!({ "type": "string" });
    for _ in 0..18 {
        node = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": { "nested": node }
        });
    }
    assert!(matches!(
        validate_config_schema(&node),
        Err(PackageError::InvalidSchema(_))
    ));
}

#[test]
fn sch_05_unsupported_keywords_and_cron_format_rejected() {
    for kw in [
        "oneOf",
        "anyOf",
        "allOf",
        "not",
        "if",
        "patternProperties",
        "dependencies",
    ] {
        let mut obj = serde_json::Map::new();
        obj.insert("type".into(), json!("object"));
        obj.insert(kw.into(), json!([]));
        assert!(
            matches!(
                validate_config_schema(&serde_json::Value::Object(obj)),
                Err(PackageError::InvalidSchema(_))
            ),
            "expected InvalidSchema for keyword {kw}"
        );
    }

    let cron_schema = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "sched": { "type": "string", "format": "cron" }
        }
    });
    assert!(matches!(
        validate_config_schema(&cron_schema),
        Err(PackageError::InvalidSchema(_))
    ));
}

#[test]
fn sch_06_x_ui_widget_whitelist_enforced() {
    let valid_widgets = [
        "text",
        "textarea",
        "password",
        "select",
        "multi-select",
        "site",
        "downloader",
        "library",
        "subscribe",
        "interval",
        "bytes",
    ];
    for w in valid_widgets {
        let raw = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "f": { "type": "string", "x-ui": { "widget": w } }
            }
        });
        assert!(
            validate_config_schema(&raw).is_ok(),
            "widget {w} should pass"
        );
    }

    for bad in ["html", "script", "cron", "custom"] {
        let raw = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "f": { "type": "string", "x-ui": { "widget": bad } }
            }
        });
        assert!(matches!(
            validate_config_schema(&raw),
            Err(PackageError::InvalidSchema(_))
        ));
    }
}

#[test]
fn sch_07_config_value_validation_defaults_and_field_errors() {
    let raw = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["interval_secs", "tags"],
        "properties": {
            "interval_secs": { "type": "integer", "minimum": 60, "maximum": 86400, "default": 3600 },
            "tags": {
                "type": "array",
                "minItems": 1,
                "maxItems": 3,
                "uniqueItems": true,
                "items": { "type": "string", "minLength": 2, "pattern": "^[a-z]+$" }
            }
        }
    });
    let schema = validate_config_schema(&raw).expect("schema");

    // Initial creation materializes default
    let created = schema
        .validate_values(&json!({ "tags": ["pt"] }), true)
        .expect("defaults materialized");
    assert_eq!(created["interval_secs"], json!(3600));

    // Update does NOT backfill defaults
    let missing_on_update = schema
        .validate_values(&json!({ "tags": ["pt"] }), false)
        .unwrap_err();
    assert!(
        missing_on_update
            .iter()
            .any(|e| e.path == "/values/interval_secs" && e.code == "required")
    );

    // Unknown field, minimum, uniqueItems, pattern
    let errs = validate_config_values(
        &schema,
        &json!({
            "interval_secs": 30,
            "tags": ["AB", "AB"],
            "extra": true
        }),
    )
    .unwrap_err();
    let PackageError::InvalidConfig { fields } = errs else {
        panic!("expected InvalidConfig");
    };
    assert!(
        fields
            .iter()
            .any(|f| f.path == "/values/interval_secs" && f.code == "minimum")
    );
    assert!(
        fields
            .iter()
            .any(|f| f.path == "/values/extra" && f.code == "additionalProperties")
    );
    assert!(
        fields
            .iter()
            .any(|f| f.path == "/values/tags" && f.code == "uniqueItems")
    );
    assert!(
        fields
            .iter()
            .any(|f| f.path == "/values/tags/0" && f.code == "pattern")
    );
}

#[test]
fn sch_08_secret_discovery_and_redaction() {
    let raw = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["host", "api_key"],
        "properties": {
            "host": { "type": "string" },
            "api_key": { "type": "string", "writeOnly": true, "x-ui": { "widget": "password" } },
            "webhook_token": { "type": "string", "format": "password" }
        }
    });
    let schema = validate_config_schema(&raw).expect("schema");
    let keys: Vec<&str> = schema
        .secret_fields()
        .iter()
        .map(|s| s.key.as_str())
        .collect();
    assert!(keys.contains(&"api_key"));
    assert!(keys.contains(&"webhook_token"));

    let mut configured = BTreeSet::new();
    configured.insert("api_key".to_string());
    let redacted = schema.redact_config(
        &json!({ "host": "https://pt.example", "api_key": "should-never-leak" }),
        &configured,
    );
    assert_eq!(redacted.values, json!({ "host": "https://pt.example" }));
    assert!(redacted.secrets["api_key"].configured);
    assert!(!redacted.secrets["webhook_token"].configured);

    let projected = project_config_with_secrets(&schema, &json!({ "host": "a" }), &configured);
    assert_eq!(projected["secrets"]["api_key"]["configured"], json!(true));
}

#[test]
fn sch_09_secret_keep_replace_clear_write_resolution() {
    let raw = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["host", "api_key"],
        "properties": {
            "host": { "type": "string" },
            "api_key": { "type": "string", "minLength": 4, "x-ui": { "widget": "password" } }
        }
    });
    let schema = validate_config_schema(&raw).expect("schema");

    // Passing plaintext secret inside values must be rejected
    let mut actions = BTreeMap::new();
    actions.insert("api_key".into(), SecretWriteAction::Keep);
    let mut existing = BTreeSet::new();
    existing.insert("api_key".into());
    let leak_err = schema
        .resolve_config_write(
            &json!({ "host": "https://a.test", "api_key": "plaintext" }),
            &actions,
            &existing,
        )
        .unwrap_err();
    assert!(leak_err.iter().any(|e| e.path == "/values/api_key"));

    // Keep with existing configured passes
    let ok_keep = schema
        .resolve_config_write(&json!({ "host": "https://a.test" }), &actions, &existing)
        .expect("keep ok");
    assert_eq!(ok_keep.public_values, json!({ "host": "https://a.test" }));

    // Replace too short fails
    let mut short_actions = BTreeMap::new();
    short_actions.insert("api_key".into(), SecretWriteAction::Replace("ab".into()));
    assert!(
        schema
            .resolve_config_write(
                &json!({ "host": "https://a.test" }),
                &short_actions,
                &existing
            )
            .is_err()
    );

    // Replace > 64 KiB fails
    let mut huge_actions = BTreeMap::new();
    huge_actions.insert(
        "api_key".into(),
        SecretWriteAction::Replace("x".repeat(65537)),
    );
    assert!(split_config_secrets(&schema, &json!({ "host": "a" }), &huge_actions).is_err());

    // Unknown secret key rejected
    let mut unknown_actions = BTreeMap::new();
    unknown_actions.insert("unknown".into(), SecretWriteAction::Clear);
    assert!(
        schema
            .resolve_config_write(&json!({ "host": "a" }), &unknown_actions, &existing)
            .is_err()
    );

    // Valid Replace
    let mut valid_replace = BTreeMap::new();
    valid_replace.insert(
        "api_key".into(),
        SecretWriteAction::Replace("secret123".into()),
    );
    let res = schema
        .resolve_config_write(
            &json!({ "host": "https://a.test" }),
            &valid_replace,
            &BTreeSet::new(),
        )
        .expect("replace ok");
    assert_eq!(res.secret_updates.len(), 1);
    assert_eq!(
        res.secret_updates[0].change,
        SecretWriteAction::Replace("secret123".into())
    );
}

#[test]
fn sch_10_secret_inside_array_or_non_string_rejected() {
    let non_string_secret = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "pin": { "type": "integer", "x-ui": { "widget": "password" } }
        }
    });
    assert!(matches!(
        validate_config_schema(&non_string_secret),
        Err(PackageError::InvalidSchema(_))
    ));

    let array_secret = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "keys": {
                "type": "array",
                "items": { "type": "string", "writeOnly": true }
            }
        }
    });
    assert!(matches!(
        validate_config_schema(&array_secret),
        Err(PackageError::InvalidSchema(_))
    ));
}

#[test]
fn sch_11_nested_object_secret_and_x_ui_secret_flag() {
    let raw = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "auth": {
                "type": "object",
                "additionalProperties": false,
                "required": ["token"],
                "properties": {
                    "label": { "type": "string" },
                    "token": { "type": "string", "x-ui": { "secret": true } }
                }
            }
        }
    });
    let schema = validate_config_schema(&raw).expect("nested secret schema");
    assert_eq!(schema.secret_fields().len(), 1);
    assert_eq!(schema.secret_fields()[0].key, "auth.token");
    assert!(schema.secret_fields()[0].required);

    let mut actions = BTreeMap::new();
    actions.insert(
        "auth.token".into(),
        SecretWriteAction::Replace("tok-123".into()),
    );
    let write = schema
        .resolve_config_write(
            &json!({ "auth": { "label": "main" } }),
            &actions,
            &BTreeSet::new(),
        )
        .expect("nested secret replace ok");
    assert_eq!(write.public_values, json!({ "auth": { "label": "main" } }));
}

#[test]
fn sch_12_ref_with_sibling_secret_and_validation_keywords() {
    let raw = json!({
        "$defs": {
            "PlainString": { "type": "string", "minLength": 4 }
        },
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "api_key": { "$ref": "#/$defs/PlainString", "writeOnly": true, "maxLength": 16 },
            "token": { "$ref": "#/$defs/PlainString", "x-ui": { "secret": true } }
        }
    });
    let schema = validate_config_schema(&raw).expect("valid $ref with sibling secret");
    let keys: Vec<&str> = schema
        .secret_fields()
        .iter()
        .map(|s| s.key.as_str())
        .collect();
    assert!(keys.contains(&"api_key"));
    assert!(keys.contains(&"token"));

    let leak = schema
        .resolve_config_write(
            &json!({ "api_key": "PLAINTEXT", "token": "LEAK" }),
            &BTreeMap::new(),
            &BTreeSet::new(),
        )
        .unwrap_err();
    assert!(leak.iter().any(|e| e.code == "secret_in_values"));

    let redacted = schema.redact_config(
        &json!({ "api_key": "PLAINTEXT", "token": "LEAK" }),
        &BTreeSet::new(),
    );
    assert_eq!(redacted.values, json!({}));

    let mut short = BTreeMap::new();
    short.insert("api_key".into(), SecretWriteAction::Replace("ab".into()));
    assert!(
        schema
            .resolve_config_write(&json!({}), &short, &BTreeSet::new())
            .is_err()
    );

    let mut too_long = BTreeMap::new();
    too_long.insert("api_key".into(), SecretWriteAction::Replace("a".repeat(17)));
    assert!(
        schema
            .resolve_config_write(&json!({}), &too_long, &BTreeSet::new())
            .is_err()
    );
}

#[test]
fn sch_13_dotted_property_name_secret_and_optional_nested_secret() {
    let raw_dotted = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "api.key": {
                "type": "string",
                "minLength": 10,
                "pattern": "^sk-[a-z]+$",
                "writeOnly": true
            },
            "a/b~c": { "type": "string", "writeOnly": true }
        }
    });
    let schema_dotted = validate_config_schema(&raw_dotted).expect("dotted prop schema");
    let slashed = schema_dotted
        .secret_fields()
        .iter()
        .find(|s| s.key == "a/b~c")
        .expect("escaped pointer spec");
    assert_eq!(slashed.pointer, "/values/a~1b~0c");

    let mut bad_replace = BTreeMap::new();
    bad_replace.insert("api.key".into(), SecretWriteAction::Replace("x".into()));
    assert!(
        schema_dotted
            .resolve_config_write(&json!({}), &bad_replace, &BTreeSet::new())
            .is_err()
    );

    let mut ok_replace = BTreeMap::new();
    ok_replace.insert(
        "api.key".into(),
        SecretWriteAction::Replace("sk-validsecret".into()),
    );
    assert!(
        schema_dotted
            .resolve_config_write(&json!({}), &ok_replace, &BTreeSet::new())
            .is_ok()
    );

    // Optional nested object secret: omitted `auth` should succeed without auth.token
    let raw_opt = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "auth": {
                "type": "object",
                "additionalProperties": false,
                "required": ["token"],
                "properties": {
                    "label": { "type": "string" },
                    "token": { "type": "string", "writeOnly": true }
                }
            }
        }
    });
    let schema_opt = validate_config_schema(&raw_opt).expect("opt nested schema");
    assert!(
        schema_opt
            .resolve_config_write(&json!({}), &BTreeMap::new(), &BTreeSet::new())
            .is_ok()
    );
    // Provided `auth` without `auth.token` must fail
    assert!(
        schema_opt
            .resolve_config_write(
                &json!({ "auth": { "label": "main" } }),
                &BTreeMap::new(),
                &BTreeSet::new()
            )
            .is_err()
    );
}

#[test]
fn sch_14_default_type_and_secret_default_validation() {
    let bad_defaults = [
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": { "count": { "type": "integer", "default": "not-an-int" } }
        }),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": { "count": { "type": "integer", "minimum": 10, "default": 5 } }
        }),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": { "secret": { "type": "string", "writeOnly": true, "default": "hardcoded" } }
        }),
        json!({
            "$defs": { "S": { "type": "string", "writeOnly": true } },
            "type": "object",
            "additionalProperties": false,
            "properties": { "secret": { "$ref": "#/$defs/S", "default": "hardcoded" } }
        }),
    ];
    for raw in bad_defaults {
        assert!(
            matches!(
                validate_config_schema(&raw),
                Err(PackageError::InvalidSchema(_))
            ),
            "expected InvalidSchema for {raw}"
        );
    }
}

#[test]
fn sch_15_object_defaults_with_secrets_and_ref_required_and_unreferenced_defs() {
    let secret_in_defaults = [
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": { "api_key": { "type": "string", "writeOnly": true } },
            "default": { "api_key": "leaked" }
        }),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "auth": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "token": { "type": "string", "format": "password" } },
                    "default": { "token": "leaked" }
                }
            }
        }),
        json!({
            "$defs": { "SecretStr": { "type": "string", "x-ui": { "secret": true } } },
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "auth": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "token": { "$ref": "#/$defs/SecretStr" } },
                    "default": { "token": "leaked" }
                }
            }
        }),
        json!({
            "$defs": {
                "Auth": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "token": { "type": "string", "writeOnly": true } },
                    "default": { "token": "leaked" }
                }
            },
            "type": "object",
            "additionalProperties": false,
            "properties": { "auth": { "$ref": "#/$defs/Auth" } }
        }),
    ];
    for raw in secret_in_defaults {
        assert!(
            matches!(
                validate_config_schema(&raw),
                Err(PackageError::InvalidSchema(_))
            ),
            "expected InvalidSchema for secret in object/root/$defs default: {raw}"
        );
    }

    let valid_partial_default = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "auth": {
                "type": "object",
                "additionalProperties": false,
                "required": ["label", "token"],
                "properties": {
                    "label": { "type": "string" },
                    "token": { "type": "string", "writeOnly": true }
                },
                "default": { "label": "default-label" }
            }
        }
    });
    let partial_schema =
        validate_config_schema(&valid_partial_default).expect("valid partial non-secret default");
    let materialized = partial_schema
        .validate_values(&json!({}), true)
        .expect("materializes non-secret default");
    assert_eq!(
        materialized,
        json!({ "auth": { "label": "default-label" } })
    );
}

#[test]
fn sch_16_ref_sibling_required_and_unreferenced_defs_keywords() {
    let ref_required_raw = json!({
        "$defs": {
            "Auth": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "label": { "type": "string" },
                    "token": { "type": "string", "writeOnly": true }
                }
            }
        },
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "auth": { "$ref": "#/$defs/Auth", "required": ["token"] },
            "backup_auth": { "$ref": "#/$defs/Auth" }
        }
    });
    let ref_schema = validate_config_schema(&ref_required_raw).expect("ref required schema");
    let auth_token = ref_schema
        .secret_fields()
        .iter()
        .find(|s| s.key == "auth.token")
        .expect("auth.token");
    assert!(auth_token.required, "auth.token must be required");
    let backup_token = ref_schema
        .secret_fields()
        .iter()
        .find(|s| s.key == "backup_auth.token")
        .expect("backup_auth.token");
    assert!(
        !backup_token.required,
        "sibling backup_auth.token must remain optional"
    );
    assert!(
        ref_schema
            .resolve_config_write(&json!({ "auth": {} }), &BTreeMap::new(), &BTreeSet::new())
            .is_err()
    );
    assert!(
        ref_schema
            .resolve_config_write(
                &json!({ "backup_auth": {} }),
                &BTreeMap::new(),
                &BTreeSet::new()
            )
            .is_ok()
    );

    for bad_kw in ["$schema", "$defs"] {
        let mut def_map = serde_json::Map::new();
        def_map.insert("type".into(), json!("string"));
        def_map.insert(bad_kw.into(), json!({}));
        let raw = json!({
            "$defs": { "Unused": serde_json::Value::Object(def_map) },
            "type": "object",
            "additionalProperties": false,
            "properties": { "host": { "type": "string" } }
        });
        assert!(
            matches!(
                validate_config_schema(&raw),
                Err(PackageError::InvalidSchema(_))
            ),
            "expected InvalidSchema for unreferenced $defs containing {bad_kw}"
        );
    }
}
