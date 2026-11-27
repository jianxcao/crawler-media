mod common;

use common::{ZipEntrySpec, build_zip, dev_policy, minimal_config_schema_json, minimal_site_yaml};
use plugin_package::{
    PackageError, PackagePolicy, validate_cmplugin_bytes, validate_manifest_toml,
    validate_manifest_toml_with_policy,
};

fn replace_base_manifest(extra: &str) -> String {
    format!(
        r#"manifest_version = 1
id = "org.example.valid-plugin"
name = "Valid Plugin"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Author One"]
description = "A valid test plugin"
config_schema = "schemas/config.schema.json"
state_schema = 1
{extra}"#
    )
}

#[test]
fn man_01_valid_hook_task_and_endpoint_manifests() {
    let hook_toml = replace_base_manifest(
        r#"
[[components]]
id = "policy"
entry = "components/policy.wasm"
world = "crawler:plugin/hook-plugin@1.0.0"
permissions = ["hooks:search.after", "kv"]

[[components.hooks]]
id = "search.after"
priority = 400
"#,
    );
    let m1 = validate_manifest_toml(&hook_toml).expect("hook manifest");
    assert_eq!(m1.components.len(), 1);

    let task_toml = replace_base_manifest(
        r#"
[[components]]
id = "runner"
entry = "components/runner.wasm"
world = "crawler:plugin/task-plugin@1.0.0"
permissions = ["kv", "read:sites"]

[[components.tasks]]
id = "daily"
input_schema = "schemas/task.schema.json"
schedule = { kind = "interval", seconds = 3600 }
concurrency_key = "daily"
"#,
    );
    let m2 = validate_manifest_toml(&task_toml).expect("task manifest");
    assert_eq!(m2.components[0].tasks.len(), 1);

    let endpoint_toml = replace_base_manifest(
        r#"
[[components]]
id = "api"
entry = "components/api.wasm"
world = "crawler:plugin/endpoint-plugin@1.0.0"
permissions = ["endpoints", "read:sites", "command:site.maintenance"]

[[components.endpoints]]
id = "run-check"
method = "POST"
path = "/run"
auth = "admin"
input_schema = "schemas/ep.schema.json"
capabilities = ["read:sites", "command:site.maintenance"]
"#,
    );
    let m3 = validate_manifest_toml(&endpoint_toml).expect("endpoint manifest");
    assert_eq!(m3.components[0].endpoints.len(), 1);
}

#[test]
fn man_02_unknown_toml_keys_rejected_at_every_depth() {
    let cases = [
        replace_base_manifest(
            "unknown_top = 1\n[[components]]\nid = \"p\"\nentry = \"components/p.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\n[[components.hooks]]\nid = \"search.after\"\npriority = 1\n",
        ),
        replace_base_manifest(
            "[limits]\nmemory_mib = 64\nextra_limit = 1\n[[components]]\nid = \"p\"\nentry = \"components/p.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\n[[components.hooks]]\nid = \"search.after\"\npriority = 1\n",
        ),
        replace_base_manifest(
            "[[components]]\nid = \"p\"\nentry = \"components/p.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\nextra_comp = 1\n[[components.hooks]]\nid = \"search.after\"\npriority = 1\n",
        ),
        replace_base_manifest(
            "[[components]]\nid = \"p\"\nentry = \"components/p.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\n[[components.hooks]]\nid = \"search.after\"\npriority = 1\nextra_hook = 1\n",
        ),
        replace_base_manifest(
            "[[components]]\nid = \"t\"\nentry = \"components/t.wasm\"\nworld = \"crawler:plugin/task-plugin@1.0.0\"\n[[components.tasks]]\nid = \"t1\"\ninput_schema = \"schemas/t.json\"\nschedule = { kind = \"interval\", seconds = 60 }\nextra_task = 1\n",
        ),
        replace_base_manifest(
            "[[components]]\nid = \"t\"\nentry = \"components/t.wasm\"\nworld = \"crawler:plugin/task-plugin@1.0.0\"\n[[components.tasks]]\nid = \"t1\"\ninput_schema = \"schemas/t.json\"\nschedule = { kind = \"interval\", seconds = 60, extra_sched = 1 }\n",
        ),
        replace_base_manifest(
            "[[components]]\nid = \"e\"\nentry = \"components/e.wasm\"\nworld = \"crawler:plugin/endpoint-plugin@1.0.0\"\npermissions = [\"endpoints\"]\n[[components.endpoints]]\nid = \"e1\"\nmethod = \"GET\"\npath = \"/x\"\nauth = \"admin\"\ninput_schema = \"schemas/e.json\"\nextra_ep = 1\n",
        ),
        replace_base_manifest(
            "[[http]]\norigin = \"https://example.com\"\npath_prefix = \"/api\"\nmethods = [\"GET\"]\nextra_http = 1\n[[components]]\nid = \"p\"\nentry = \"components/p.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\n[[components.hooks]]\nid = \"search.after\"\npriority = 1\n",
        ),
        replace_base_manifest(
            "[ui]\nkind = \"federation-react\"\ncontract = \"1.0.0\"\nremote_name = \"cm_test\"\nentry = \"ui/assets/remoteEntry.js\"\nasset_manifest = \"ui/asset-manifest.json\"\nreact = \"19.3.0\"\nreact_dom = \"19.3.0\"\nextra_ui = 1\n[[ui.surfaces]]\nid = \"s1\"\nkind = \"config\"\nexpose = \"./Config\"\ntitle = \"Cfg\"\n",
        ),
        replace_base_manifest(
            "[ui]\nkind = \"federation-react\"\ncontract = \"1.0.0\"\nremote_name = \"cm_test\"\nentry = \"ui/assets/remoteEntry.js\"\nasset_manifest = \"ui/asset-manifest.json\"\nreact = \"19.3.0\"\nreact_dom = \"19.3.0\"\n[[ui.surfaces]]\nid = \"s1\"\nkind = \"config\"\nexpose = \"./Config\"\ntitle = \"Cfg\"\nextra_surface = 1\n",
        ),
    ];
    for case in cases {
        let err = validate_manifest_toml(&case).unwrap_err();
        assert!(matches!(err, PackageError::InvalidManifest(_)));
    }
}

#[test]
fn man_03_manifest_version_must_be_one() {
    for bad_ver in ["0", "2", "\"1\""] {
        let toml = format!(
            "manifest_version = {bad_ver}\nid = \"org.example.a\"\nname = \"A\"\nversion = \"1.0.0\"\napi = \"1.0.0\"\nlicense = \"MIT\"\nauthors = [\"A\"]\ndescription = \"D\"\nconfig_schema = \"schemas/c.json\"\nstate_schema = 1\nsites = [\"sites/a.yaml\"]\n"
        );
        assert!(matches!(
            validate_manifest_toml(&toml),
            Err(PackageError::InvalidManifest(_))
        ));
    }
}

#[test]
fn man_04_plugin_id_rules_and_reserved_names() {
    let long_id = format!("org.example.{}", "a".repeat(120));
    let bad_ids = [
        "ab",
        &long_id,
        "Org.Example.Foo",
        "example",
        "org.example/foo",
        "org.example\\foo",
        ".org.example",
        "org.example.",
        "org..example",
        "org.example.-bad",
        "org.example.bad-",
        "org.example.with space",
        "org.example.under_score",
        "installed",
        "repos",
        "developer",
        "new",
    ];
    for bad in bad_ids {
        let toml = format!(
            "manifest_version = 1\nid = \"{bad}\"\nname = \"A\"\nversion = \"1.0.0\"\napi = \"1.0.0\"\nlicense = \"MIT\"\nauthors = [\"A\"]\ndescription = \"D\"\nconfig_schema = \"schemas/c.json\"\nstate_schema = 1\nsites = [\"sites/a.yaml\"]\n"
        );
        assert!(
            matches!(
                validate_manifest_toml(&toml),
                Err(PackageError::InvalidManifest(_))
            ),
            "expected InvalidManifest for id `{bad}`"
        );
    }
}

#[test]
fn man_05_display_metadata_no_empty_or_html() {
    let base_ok = "manifest_version = 1\nid = \"org.example.meta\"\nversion = \"1.0.0\"\napi = \"1.0.0\"\nconfig_schema = \"schemas/c.json\"\nstate_schema = 1\nsites = [\"sites/a.yaml\"]\n";
    let bad_cases = [
        format!(
            "{base_ok}name = \"\"\nlicense = \"MIT\"\nauthors = [\"A\"]\ndescription = \"D\"\n"
        ),
        format!(
            "{base_ok}name = \"<b>Bad</b>\"\nlicense = \"MIT\"\nauthors = [\"A\"]\ndescription = \"D\"\n"
        ),
        format!("{base_ok}name = \"Ok\"\nlicense = \"\"\nauthors = [\"A\"]\ndescription = \"D\"\n"),
        format!("{base_ok}name = \"Ok\"\nlicense = \"MIT\"\nauthors = []\ndescription = \"D\"\n"),
        format!(
            "{base_ok}name = \"Ok\"\nlicense = \"MIT\"\nauthors = [\"<script>x</script>\"]\ndescription = \"D\"\n"
        ),
        format!(
            "{base_ok}name = \"Ok\"\nlicense = \"MIT\"\nauthors = [\"A\"]\ndescription = \"<img src=x>\"\n"
        ),
    ];
    for case in bad_cases {
        assert!(matches!(
            validate_manifest_toml(&case),
            Err(PackageError::InvalidManifest(_))
        ));
    }
}

#[test]
fn man_06_exact_semver_and_exact_api_version() {
    for bad_ver in ["1.0", "^1.0.0", "v1.0.0"] {
        let toml = format!(
            "manifest_version = 1\nid = \"org.example.sv\"\nname = \"A\"\nversion = \"{bad_ver}\"\napi = \"1.0.0\"\nlicense = \"MIT\"\nauthors = [\"A\"]\ndescription = \"D\"\nconfig_schema = \"schemas/c.json\"\nstate_schema = 1\nsites = [\"sites/a.yaml\"]\n"
        );
        assert!(matches!(
            validate_manifest_toml(&toml),
            Err(PackageError::InvalidManifest(_))
        ));
    }
    for bad_api in ["^1.0.0", "1.0", "1.0.1", "2.0.0"] {
        let toml = format!(
            "manifest_version = 1\nid = \"org.example.sv\"\nname = \"A\"\nversion = \"1.0.0\"\napi = \"{bad_api}\"\nlicense = \"MIT\"\nauthors = [\"A\"]\ndescription = \"D\"\nconfig_schema = \"schemas/c.json\"\nstate_schema = 1\nsites = [\"sites/a.yaml\"]\n"
        );
        let err = validate_manifest_toml(&toml).unwrap_err();
        assert!(matches!(
            err,
            PackageError::InvalidManifest(_) | PackageError::UnsupportedWorld { .. }
        ));
    }
}

#[test]
fn man_07_state_schema_positive_integer() {
    for bad in ["0", "-1", "\"1\""] {
        let toml = format!(
            "manifest_version = 1\nid = \"org.example.ss\"\nname = \"A\"\nversion = \"1.0.0\"\napi = \"1.0.0\"\nlicense = \"MIT\"\nauthors = [\"A\"]\ndescription = \"D\"\nconfig_schema = \"schemas/c.json\"\nstate_schema = {bad}\nsites = [\"sites/a.yaml\"]\n"
        );
        assert!(matches!(
            validate_manifest_toml(&toml),
            Err(PackageError::InvalidManifest(_))
        ));
    }
}

#[test]
fn man_08_empty_package_rejected() {
    let toml = replace_base_manifest("");
    assert!(matches!(
        validate_manifest_toml(&toml),
        Err(PackageError::EmptyPackage)
    ));
}

#[test]
fn man_09_valid_yaml_only_package() {
    let toml = replace_base_manifest("sites = [\"sites/custom.yaml\"]\n");
    let zip = build_zip(&[
        ZipEntrySpec::stored("plugin.toml", toml.as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored(
            "sites/custom.yaml",
            minimal_site_yaml("custom-site").as_bytes(),
        ),
    ]);
    let pkg = validate_cmplugin_bytes(&zip, dev_policy()).expect("yaml-only valid");
    assert!(pkg.components.is_empty());
    assert_eq!(pkg.sites.len(), 1);
    assert_eq!(pkg.sites[0].id, "custom-site");
}

#[test]
fn man_11_component_count_and_id_and_entry_checks() {
    let mut comps = String::new();
    for i in 0..17 {
        comps.push_str(&format!(
            "[[components]]\nid = \"c{i}\"\nentry = \"components/c{i}.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\n[[components.hooks]]\nid = \"search.after\"\npriority = 10\n"
        ));
    }
    assert!(matches!(
        validate_manifest_toml(&replace_base_manifest(&comps)),
        Err(PackageError::InvalidManifest(_))
    ));

    let dup_id = replace_base_manifest(
        "[[components]]\nid = \"dup\"\nentry = \"components/a.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\n[[components.hooks]]\nid = \"search.after\"\npriority = 10\n[[components]]\nid = \"dup\"\nentry = \"components/b.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\n[[components.hooks]]\nid = \"search.after\"\npriority = 20\n",
    );
    assert!(matches!(
        validate_manifest_toml(&dup_id),
        Err(PackageError::InvalidManifest(_))
    ));

    let bad_wasm_zip = build_zip(&[
        ZipEntrySpec::stored(
            "plugin.toml",
            replace_base_manifest(
                "[[components]]\nid = \"c1\"\nentry = \"components/c1.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\n[[components.hooks]]\nid = \"search.after\"\npriority = 10\n",
            )
            .as_bytes(),
        ),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("components/c1.wasm", b"not-wasm-magic"),
    ]);
    assert!(matches!(
        validate_cmplugin_bytes(&bad_wasm_zip, dev_policy()),
        Err(PackageError::InvalidManifest(_))
    ));
}

#[test]
fn man_12_world_gating_and_phase2_rejection() {
    for world in [
        "crawler:plugin/unknown@1.0.0",
        "crawler:plugin/hook-plugin@2.0.0",
        "crawler:plugin/list-plugin@1.0.0",
        "crawler:plugin/channel-plugin@1.0.0",
    ] {
        let toml = replace_base_manifest(&format!(
            "[[components]]\nid = \"c1\"\nentry = \"components/c1.wasm\"\nworld = \"{world}\"\n"
        ));
        let err = validate_manifest_toml_with_policy(&toml, &PackagePolicy::default()).unwrap_err();
        assert!(
            matches!(err, PackageError::UnsupportedWorld { .. }),
            "expected UnsupportedWorld for {world}, got {err:?}"
        );
    }

    let indexer_toml = replace_base_manifest(
        "credential_binding = \"invocation-site\"\n[[components]]\nid = \"idx\"\nentry = \"components/idx.wasm\"\nworld = \"crawler:plugin/indexer-plugin@1.0.0\"\npermissions = [\"indexer:provide\"]\n[[components.indexers]]\nid = \"demo\"\nname = \"Demo\"\nkind = \"pt\"\ncapabilities = [\"search\"]\nsite_schema = \"schemas/site.schema.json\"\n",
    );
    validate_manifest_toml_with_policy(&indexer_toml, &PackagePolicy::default())
        .expect("indexer-plugin allowed by default policy");

    let maint_toml = replace_base_manifest(
        "credential_binding = \"invocation-site\"\n[[components]]\nid = \"maint\"\nentry = \"components/maint.wasm\"\nworld = \"crawler:plugin/maintenance-plugin@1.0.0\"\npermissions = [\"maintenance:provide\"]\n[[components.maintenance]]\nid = \"demo-maint\"\nactions = [\"check-in\"]\n",
    );
    validate_manifest_toml_with_policy(&maint_toml, &PackagePolicy::default())
        .expect("maintenance-plugin allowed by default policy");
}

#[test]
fn man_13_cross_world_table_exclusivity_enforced() {
    let cases = [
        "[[components]]\nid = \"h\"\nentry = \"components/h.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\n[[components.hooks]]\nid = \"search.after\"\npriority = 10\n[[components.tasks]]\nid = \"t\"\ninput_schema = \"schemas/t.json\"\nschedule = { kind = \"interval\", seconds = 60 }\n",
        "[[components]]\nid = \"t\"\nentry = \"components/t.wasm\"\nworld = \"crawler:plugin/task-plugin@1.0.0\"\n[[components.tasks]]\nid = \"t\"\ninput_schema = \"schemas/t.json\"\nschedule = { kind = \"interval\", seconds = 60 }\n[[components.hooks]]\nid = \"search.after\"\npriority = 10\n",
        "[[components]]\nid = \"e\"\nentry = \"components/e.wasm\"\nworld = \"crawler:plugin/endpoint-plugin@1.0.0\"\npermissions = [\"endpoints\"]\n[[components.endpoints]]\nid = \"ep\"\nmethod = \"GET\"\npath = \"/a\"\nauth = \"admin\"\ninput_schema = \"schemas/e.json\"\n[[components.hooks]]\nid = \"search.after\"\npriority = 10\n",
    ];
    for c in cases {
        let err = validate_manifest_toml(&replace_base_manifest(c)).unwrap_err();
        assert!(matches!(err, PackageError::InvalidManifest(_)));
    }
}

#[test]
fn man_14_hook_bounds_enforced() {
    let bad_prio = replace_base_manifest(
        "[[components]]\nid = \"h\"\nentry = \"components/h.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\n[[components.hooks]]\nid = \"search.after\"\npriority = 1001\n",
    );
    assert!(matches!(
        validate_manifest_toml(&bad_prio),
        Err(PackageError::InvalidManifest(_))
    ));

    let neg_prio = replace_base_manifest(
        "[[components]]\nid = \"h\"\nentry = \"components/h.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\n[[components.hooks]]\nid = \"search.after\"\npriority = -1\n",
    );
    assert!(matches!(
        validate_manifest_toml(&neg_prio),
        Err(PackageError::InvalidManifest(_))
    ));

    let dup_hook = replace_base_manifest(
        "[[components]]\nid = \"h\"\nentry = \"components/h.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\n[[components.hooks]]\nid = \"search.after\"\npriority = 10\n[[components.hooks]]\nid = \"search.after\"\npriority = 20\n",
    );
    assert!(matches!(
        validate_manifest_toml(&dup_hook),
        Err(PackageError::InvalidManifest(_))
    ));

    let bad_hook = replace_base_manifest(
        "[[components]]\nid = \"h\"\nentry = \"components/h.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\n[[components.hooks]]\nid = \"playback.progress\"\npriority = 10\n",
    );
    assert!(matches!(
        validate_manifest_toml(&bad_hook),
        Err(PackageError::UnsupportedHook(_))
    ));

    let empty_hooks = replace_base_manifest(
        "[[components]]\nid = \"h\"\nentry = \"components/h.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\n",
    );
    assert!(matches!(
        validate_manifest_toml(&empty_hooks),
        Err(PackageError::InvalidManifest(_))
    ));
}

#[test]
fn man_15_task_bounds_enforced() {
    let cron = replace_base_manifest(
        "[[components]]\nid = \"t\"\nentry = \"components/t.wasm\"\nworld = \"crawler:plugin/task-plugin@1.0.0\"\n[[components.tasks]]\nid = \"t1\"\ninput_schema = \"schemas/t.json\"\nschedule = { kind = \"cron\", seconds = 60 }\n",
    );
    assert!(matches!(
        validate_manifest_toml(&cron),
        Err(PackageError::InvalidManifest(_))
    ));

    for bad_secs in [59, 86401] {
        let toml = replace_base_manifest(&format!(
            "[[components]]\nid = \"t\"\nentry = \"components/t.wasm\"\nworld = \"crawler:plugin/task-plugin@1.0.0\"\n[[components.tasks]]\nid = \"t1\"\ninput_schema = \"schemas/t.json\"\nschedule = {{ kind = \"interval\", seconds = {bad_secs} }}\n"
        ));
        assert!(matches!(
            validate_manifest_toml(&toml),
            Err(PackageError::InvalidManifest(_))
        ));
    }

    let empty_tasks = replace_base_manifest(
        "[[components]]\nid = \"t\"\nentry = \"components/t.wasm\"\nworld = \"crawler:plugin/task-plugin@1.0.0\"\n",
    );
    assert!(matches!(
        validate_manifest_toml(&empty_tasks),
        Err(PackageError::InvalidManifest(_))
    ));
}

#[test]
fn man_16_endpoint_bounds_enforced() {
    let public_auth = replace_base_manifest(
        "[[components]]\nid = \"e\"\nentry = \"components/e.wasm\"\nworld = \"crawler:plugin/endpoint-plugin@1.0.0\"\npermissions = [\"endpoints\"]\n[[components.endpoints]]\nid = \"ep\"\nmethod = \"GET\"\npath = \"/a\"\nauth = \"public\"\ninput_schema = \"schemas/e.json\"\n",
    );
    assert!(matches!(
        validate_manifest_toml(&public_auth),
        Err(PackageError::InvalidManifest(_))
    ));

    let ungranted_cap = replace_base_manifest(
        "[[components]]\nid = \"e\"\nentry = \"components/e.wasm\"\nworld = \"crawler:plugin/endpoint-plugin@1.0.0\"\npermissions = [\"endpoints\", \"read:sites\"]\n[[components.endpoints]]\nid = \"ep\"\nmethod = \"POST\"\npath = \"/a\"\nauth = \"admin\"\ninput_schema = \"schemas/e.json\"\ncapabilities = [\"command:downloader.add\"]\n",
    );
    assert!(matches!(
        validate_manifest_toml(&ungranted_cap),
        Err(PackageError::InvalidManifest(_))
    ));
}

#[test]
fn man_17_closed_permissions_reject_unknown_and_wildcards() {
    for bad_perm in [
        "config:secret",
        "sql:raw",
        "hooks:*",
        "task:run:*",
        "task:run:daily",
        "list:provide",
        "notify:channel",
    ] {
        let toml = replace_base_manifest(&format!(
            "[[components]]\nid = \"h\"\nentry = \"components/h.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\npermissions = [\"{bad_perm}\"]\n[[components.hooks]]\nid = \"search.after\"\npriority = 10\n"
        ));
        let err = validate_manifest_toml(&toml).unwrap_err();
        assert!(
            matches!(err, PackageError::UnknownCapability(_)),
            "expected UnknownCapability for {bad_perm}, got {err:?}"
        );
    }
}

#[test]
fn man_18_credential_binding_and_http_destinations() {
    let missing_binding = replace_base_manifest(
        "[[components]]\nid = \"h\"\nentry = \"components/h.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\npermissions = [\"http:site-auth\"]\n[[components.hooks]]\nid = \"search.after\"\npriority = 10\n",
    );
    assert!(matches!(
        validate_manifest_toml(&missing_binding),
        Err(PackageError::InvalidManifest(_))
    ));

    let bad_binding = replace_base_manifest(
        "credential_binding = \"global\"\n[[components]]\nid = \"h\"\nentry = \"components/h.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\npermissions = [\"http:site-auth\"]\n[[components.hooks]]\nid = \"search.after\"\npriority = 10\n",
    );
    assert!(matches!(
        validate_manifest_toml(&bad_binding),
        Err(PackageError::InvalidManifest(_))
    ));

    for bad_origin in [
        "https://user:pass@example.com",
        "https://*.example.com",
        "ftp://example.com",
        "https://example.com/path?x=1",
    ] {
        let toml = replace_base_manifest(&format!(
            "[[http]]\norigin = \"{bad_origin}\"\npath_prefix = \"/api\"\nmethods = [\"GET\"]\n[[components]]\nid = \"h\"\nentry = \"components/h.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\n[[components.hooks]]\nid = \"search.after\"\npriority = 10\n"
        ));
        assert!(
            matches!(
                validate_manifest_toml(&toml),
                Err(PackageError::InvalidManifest(_))
            ),
            "expected InvalidManifest for origin `{bad_origin}`"
        );
    }
}

#[test]
fn man_19_limits_must_be_positive_and_within_cap() {
    for bad_limit in [
        "memory_mib = 0",
        "memory_mib = 257",
        "fuel_limit = 0",
        "wall_ms = 0",
    ] {
        let toml = replace_base_manifest(&format!(
            "[limits]\n{bad_limit}\n[[components]]\nid = \"h\"\nentry = \"components/h.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\n[[components.hooks]]\nid = \"search.after\"\npriority = 10\n"
        ));
        assert!(matches!(
            validate_manifest_toml(&toml),
            Err(PackageError::InvalidManifest(_))
        ));
    }
}

#[test]
fn man_20_site_yaml_validation_and_collision_rejection() {
    let toml_missing = replace_base_manifest("sites = [\"sites/missing.yaml\"]\n");
    let zip_missing = build_zip(&[
        ZipEntrySpec::stored("plugin.toml", toml_missing.as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
    ]);
    assert!(matches!(
        validate_cmplugin_bytes(&zip_missing, dev_policy()),
        Err(PackageError::InvalidManifest(_))
    ));

    let toml_builtin = replace_base_manifest("sites = [\"sites/demo.yaml\"]\n");
    let zip_builtin = build_zip(&[
        ZipEntrySpec::stored("plugin.toml", toml_builtin.as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("sites/demo.yaml", minimal_site_yaml("demo").as_bytes()),
    ]);
    assert!(matches!(
        validate_cmplugin_bytes(&zip_builtin, dev_policy()),
        Err(PackageError::InvalidManifest(_))
    ));

    let toml_dup = replace_base_manifest("sites = [\"sites/a.yaml\", \"sites/b.yaml\"]\n");
    let zip_dup = build_zip(&[
        ZipEntrySpec::stored("plugin.toml", toml_dup.as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("sites/a.yaml", minimal_site_yaml("same-id").as_bytes()),
        ZipEntrySpec::stored("sites/b.yaml", minimal_site_yaml("same-id").as_bytes()),
    ]);
    assert!(matches!(
        validate_cmplugin_bytes(&zip_dup, dev_policy()),
        Err(PackageError::InvalidManifest(_))
    ));

    let toml_bad_ext = replace_base_manifest("sites = [\"sites/a.txt\"]\n");
    assert!(matches!(
        validate_manifest_toml(&toml_bad_ext),
        Err(PackageError::InvalidManifest(_))
    ));
}

#[test]
fn man_21_widened_policy_indexer_maintenance_and_multiline_toml() {
    let mut policy = PackagePolicy::default();
    policy
        .allowed_worlds
        .push("crawler:plugin/indexer-plugin@1.0.0".into());
    policy
        .allowed_worlds
        .push("crawler:plugin/maintenance-plugin@1.0.0".into());

    let toml = r#"manifest_version = 1
id = "org.example.site-suite"
name = "Site Suite"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ['Author A', "Author B"]
description = """
Multi-line description
for site suite"""
config_schema = "schemas/config.schema.json"
state_schema = 2
credential_binding = "invocation-site"

[[http]]
origin = "https://tracker.example.com"
path_prefix = "/api"
methods = ["GET", "POST"]

[[components]]
id = "idx"
entry = "components/idx.wasm"
world = "crawler:plugin/indexer-plugin@1.0.0"
permissions = ["indexer:provide", "http:site-auth"]

[[components.indexers]]
id = "main"
name = "Main Indexer"
kind = "nexusphp"
capabilities = ["search", "rss"]
site_schema = "schemas/site.schema.json"

[[components]]
id = "maint"
entry = "components/maint.wasm"
world = "crawler:plugin/maintenance-plugin@1.0.0"
permissions = ["maintenance:provide", "browser:maintenance"]

[[components.maintenance]]
id = "checkin"
actions = ["verify-session", "check-in"]
"#;
    let m = validate_manifest_toml_with_policy(toml, &policy).expect("widened policy passes");
    assert_eq!(m.components.len(), 2);
    assert_eq!(m.credential_binding.as_deref(), Some("invocation-site"));
}

#[test]
fn man_25_phase2_commands_and_endpoint_auth_rejected_by_default() {
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
        let toml = replace_base_manifest(&format!(
            "[[components]]\nid = \"h\"\nentry = \"components/h.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\npermissions = [\"command:{cmd}\"]\n[[components.hooks]]\nid = \"search.after\"\npriority = 10\n"
        ));
        let err = validate_manifest_toml_with_policy(&toml, &PackagePolicy::default()).unwrap_err();
        assert!(
            matches!(err, PackageError::UnknownCapability(_)),
            "expected UnknownCapability for deferred command `{cmd}`, got {err:?}"
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
        "notify.send",
        "schedule.set",
        "schedule.disable",
    ];
    for cmd in phase1_cmds {
        let toml = replace_base_manifest(&format!(
            "[[components]]\nid = \"h\"\nentry = \"components/h.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\npermissions = [\"command:{cmd}\"]\n[[components.hooks]]\nid = \"search.after\"\npriority = 10\n"
        ));
        validate_manifest_toml(&toml).unwrap_or_else(|e| panic!("Phase 1 command {cmd}: {e:?}"));
    }

    let apikey_toml = replace_base_manifest(
        "[[components]]\nid = \"e\"\nentry = \"components/e.wasm\"\nworld = \"crawler:plugin/endpoint-plugin@1.0.0\"\npermissions = [\"endpoints\"]\n[[components.endpoints]]\nid = \"ep\"\nmethod = \"GET\"\npath = \"/a\"\nauth = \"apikey\"\ninput_schema = \"schemas/e.json\"\n",
    );
    assert!(matches!(
        validate_manifest_toml(&apikey_toml),
        Err(PackageError::InvalidManifest(_))
    ));

    for ok_auth in ["admin", "user"] {
        let ok_toml = replace_base_manifest(&format!(
            "[[components]]\nid = \"e\"\nentry = \"components/e.wasm\"\nworld = \"crawler:plugin/endpoint-plugin@1.0.0\"\npermissions = [\"endpoints\"]\n[[components.endpoints]]\nid = \"ep\"\nmethod = \"GET\"\npath = \"/a\"\nauth = \"{ok_auth}\"\ninput_schema = \"schemas/e.json\"\n"
        ));
        validate_manifest_toml(&ok_toml)
            .unwrap_or_else(|e| panic!("auth `{ok_auth}` should pass: {e:?}"));
    }

    let mut widened = PackagePolicy::default();
    widened.allowed_commands.push("library.refresh".into());
    widened.allowed_endpoint_auths.push("apikey".into());
    let widened_cmd_toml = replace_base_manifest(
        "[[components]]\nid = \"h\"\nentry = \"components/h.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\npermissions = [\"command:library.refresh\"]\n[[components.hooks]]\nid = \"search.after\"\npriority = 10\n",
    );
    validate_manifest_toml_with_policy(&widened_cmd_toml, &widened)
        .expect("widened policy.allowed_commands allows extra command");
    validate_manifest_toml_with_policy(&apikey_toml, &widened)
        .expect("widened policy.allowed_endpoint_auths allows apikey");
}

#[test]
fn man_26_http_origin_control_whitespace_backslash_dot_rejected() {
    let bad_toml_origins = [
        r#""https://example.com/\n""#,
        r#""https://example.com\t""#,
        r#""https://ex\tample.com""#,
        r#""https://example.com\\""#,
        r#""https://example.com/.""#,
        r#""https://example.com:""#,
    ];
    for origin_lit in bad_toml_origins {
        let toml = replace_base_manifest(&format!(
            "[[http]]\norigin = {origin_lit}\npath_prefix = \"/api\"\nmethods = [\"GET\"]\n[[components]]\nid = \"h\"\nentry = \"components/h.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\n[[components.hooks]]\nid = \"search.after\"\npriority = 10\n"
        ));
        assert!(
            matches!(
                validate_manifest_toml(&toml),
                Err(PackageError::InvalidManifest(_))
            ),
            "expected InvalidManifest for origin literal {origin_lit}"
        );
    }

    for ok_origin in ["https://example.com", "https://example.com:443"] {
        let toml = replace_base_manifest(&format!(
            "[[http]]\norigin = \"{ok_origin}\"\npath_prefix = \"/api\"\nmethods = [\"GET\"]\n[[components]]\nid = \"h\"\nentry = \"components/h.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\n[[components.hooks]]\nid = \"search.after\"\npriority = 10\n"
        ));
        validate_manifest_toml(&toml)
            .unwrap_or_else(|e| panic!("valid origin `{ok_origin}` failed: {e:?}"));
    }
}

#[test]
fn man_27_phase1_hooks_allowed_and_phase2_hooks_rejected() {
    for hook_id in [
        "search.before",
        "search.site.after",
        "search.after",
        "filter.after",
        "subscribe.score",
        "subscribe.select",
        "download.before",
        "download.after",
        "transfer.rename",
        "transfer.completed",
        "site.checkin.after",
    ] {
        let toml = replace_base_manifest(&format!(
            "[[components]]\nid = \"h\"\nentry = \"components/h.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\n[[components.hooks]]\nid = \"{hook_id}\"\npriority = 10\n"
        ));
        validate_manifest_toml(&toml)
            .unwrap_or_else(|e| panic!("Phase 1 hook `{hook_id}` should validate: {e:?}"));
    }

    for phase2 in [
        "site.create.before",
        "site.request.before",
        "subscribe.create.before",
        "transfer.plan",
        "transfer.intercept",
        "upgrade.decide",
        "scrape.before",
        "playback.started",
    ] {
        let toml = replace_base_manifest(&format!(
            "[[components]]\nid = \"h\"\nentry = \"components/h.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\n[[components.hooks]]\nid = \"{phase2}\"\npriority = 10\n"
        ));
        assert!(
            matches!(
                validate_manifest_toml(&toml),
                Err(PackageError::UnsupportedHook(_))
            ),
            "Phase 2 hook `{phase2}` must be rejected"
        );
    }
}
