mod common;

use common::{ZipEntrySpec, build_zip, dev_policy, minimal_config_schema_json};
use plugin_package::{PackageError, validate_cmplugin_bytes, validate_manifest_toml};

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
fn man_22_toml_utf8_multiline_literal_and_unicode_escape_no_panic() {
    let valid_utf8 = replace_base_manifest(
        r#"
[[components]]
id = "policy"
entry = "components/policy.wasm"
world = "crawler:plugin/hook-plugin@1.0.0"

[[components.hooks]]
id = "search.after"
priority = 10
"#,
    )
    .replace(
        "description = \"A valid test plugin\"",
        "description = '''\n中文描述：支持多字节 UTF-8\n'''",
    );
    let parsed = validate_manifest_toml(&valid_utf8).expect("utf8 multiline literal should parse");
    assert!(parsed.description.contains("中文描述"));

    for bad_ctrl in [
        replace_base_manifest("sites = [\"sites/a.yaml\"]\n").replace(
            "description = \"A valid test plugin\"",
            "description = '''bad\x00ctrl'''",
        ),
        replace_base_manifest("sites = [\"sites/a.yaml\"]\n").replace(
            "description = \"A valid test plugin\"",
            "description = \"\"\"bad\x00ctrl\"\"\"",
        ),
    ] {
        assert!(matches!(
            validate_manifest_toml(&bad_ctrl),
            Err(PackageError::InvalidManifest(_))
        ));
    }

    for bad_escape in ["\"\\u中中\"", "\"\\u12中\"", "\"\\U1234中中\""] {
        let toml = replace_base_manifest("sites = [\"sites/a.yaml\"]\n")
            .replace("\"A valid test plugin\"", bad_escape);
        let err = validate_manifest_toml(&toml).unwrap_err();
        assert!(
            matches!(err, PackageError::InvalidManifest(_)),
            "expected InvalidManifest for {bad_escape}, got {err:?}"
        );
    }
}

#[test]
fn man_23_toml_scoped_aot_subtables_inline_dotted_static_array_and_numbers() {
    let repeated_aot_subtables = replace_base_manifest(
        r#"
[[components]]
id = "runner"
entry = "components/runner.wasm"
world = "crawler:plugin/task-plugin@1.0.0"

[[components.tasks]]
id = "task-one"
input_schema = "schemas/t1.json"
[components.tasks.schedule]
kind = "interval"
seconds = 60

[[components.tasks]]
id = "task-two"
input_schema = "schemas/t2.json"
[components.tasks.schedule]
kind = "interval"
seconds = 120
"#,
    );
    let m = validate_manifest_toml(&repeated_aot_subtables)
        .expect("repeated subtable under distinct AOT elements must succeed");
    assert_eq!(m.components[0].tasks.len(), 2);
    assert_eq!(m.components[0].tasks[1].schedule.seconds, 120);

    let reopen_inline = replace_base_manifest(
        "sites = [\"sites/a.yaml\"]\nlimits = { memory_mib = 64 }\n[limits]\nfuel_limit = 100\n",
    );
    assert!(matches!(
        validate_manifest_toml(&reopen_inline),
        Err(PackageError::InvalidManifest(_))
    ));

    let mutate_inline_dotted = replace_base_manifest(
        "sites = [\"sites/a.yaml\"]\nlimits = { memory_mib = 64 }\nlimits.fuel_limit = 100\n",
    );
    assert!(matches!(
        validate_manifest_toml(&mutate_inline_dotted),
        Err(PackageError::InvalidManifest(_))
    ));

    let reopen_dotted = replace_base_manifest(
        "sites = [\"sites/a.yaml\"]\nlimits.memory_mib = 64\n[limits]\nfuel_limit = 100\n",
    );
    assert!(matches!(
        validate_manifest_toml(&reopen_dotted),
        Err(PackageError::InvalidManifest(_))
    ));

    let append_static_array = replace_base_manifest(
        "components = [{ id = \"a\", entry = \"components/a.wasm\", world = \"crawler:plugin/hook-plugin@1.0.0\" }]\n[[components]]\nid = \"b\"\nentry = \"components/b.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\n",
    );
    assert!(matches!(
        validate_manifest_toml(&append_static_array),
        Err(PackageError::InvalidManifest(_))
    ));

    for bad_num in [
        "0x+1", "0b+1", "0o+1", "+_1", "0x_1", "1__2", "1_", "1.", "01",
    ] {
        let toml = replace_base_manifest("sites = [\"sites/a.yaml\"]\n")
            .replace("state_schema = 1", &format!("state_schema = {bad_num}"));
        assert!(
            matches!(
                validate_manifest_toml(&toml),
                Err(PackageError::InvalidManifest(_))
            ),
            "expected InvalidManifest for number `{bad_num}`"
        );
    }
}

#[test]
fn man_24_phase1_events_allowlist_endpoint_fragment_http_origin_and_yaml_anchors() {
    for ok_ev in [
        "events:site.checkin.after",
        "events:download.after",
        "events:transfer.completed",
        "events:download.completed",
        "events:subscribe.completed",
        "events:library.item-added",
    ] {
        let toml = replace_base_manifest(&format!(
            "[[components]]\nid = \"h\"\nentry = \"components/h.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\npermissions = [\"{ok_ev}\"]\n[[components.hooks]]\nid = \"search.after\"\npriority = 10\n"
        ));
        assert!(validate_manifest_toml(&toml).is_ok(), "{ok_ev} should pass");
    }

    for bad_ev in ["events:playback.started", "events:completely.fake"] {
        let toml = replace_base_manifest(&format!(
            "[[components]]\nid = \"h\"\nentry = \"components/h.wasm\"\nworld = \"crawler:plugin/hook-plugin@1.0.0\"\npermissions = [\"{bad_ev}\"]\n[[components.hooks]]\nid = \"search.after\"\npriority = 10\n"
        ));
        assert!(matches!(
            validate_manifest_toml(&toml),
            Err(PackageError::UnknownCapability(_))
        ));
    }

    let bad_ep_frag = replace_base_manifest(
        "[[components]]\nid = \"e\"\nentry = \"components/e.wasm\"\nworld = \"crawler:plugin/endpoint-plugin@1.0.0\"\npermissions = [\"endpoints\"]\n[[components.endpoints]]\nid = \"ep\"\nmethod = \"GET\"\npath = \"/bad#frag\"\nauth = \"admin\"\ninput_schema = \"schemas/e.json\"\n",
    );
    assert!(matches!(
        validate_manifest_toml(&bad_ep_frag),
        Err(PackageError::InvalidManifest(_))
    ));

    for bad_origin in [
        "https://example.com/",
        "https://@example.com",
        "http://169.254.169.254",
        "http://169.254.10.20:8080",
        "http://[fe80::1]",
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

    let toml_site = replace_base_manifest("sites = [\"sites/bomb.yaml\"]\n");
    let bomb_yaml = "id: custom-bomb\nframework: nexusphp\nsearch:\n  path: /t.php\n  query_param: q\nx: &a [*a]\n";
    let zip_bomb = build_zip(&[
        ZipEntrySpec::stored("plugin.toml", toml_site.as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("sites/bomb.yaml", bomb_yaml.as_bytes()),
    ]);
    assert!(matches!(
        validate_cmplugin_bytes(&zip_bomb, dev_policy()),
        Err(PackageError::InvalidManifest(_))
    ));

    let safe_yaml = "id: custom-safe\nframework: nexusphp\nsearch:\n  path: /t.php\n  query_param: q\nfields:\n  title:\n    selector: 'a[href*=\"details.php\"]'\n    alt: \"H&R\"\n";
    let zip_safe = build_zip(&[
        ZipEntrySpec::stored(
            "plugin.toml",
            toml_site.replace("bomb.yaml", "safe.yaml").as_bytes(),
        ),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("sites/safe.yaml", safe_yaml.as_bytes()),
    ]);
    assert!(validate_cmplugin_bytes(&zip_safe, dev_policy()).is_ok());
}

fn build_site_zip(yaml: &str) -> Vec<u8> {
    let toml_site = replace_base_manifest("sites = [\"sites/test.yaml\"]\n");
    build_zip(&[
        ZipEntrySpec::stored("plugin.toml", toml_site.as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("sites/test.yaml", yaml.as_bytes()),
    ])
}

#[test]
fn man_25_yaml_anchor_bypasses_rejected_and_block_scalars_accepted() {
    let base_hdr =
        "id: custom-test\nframework: nexusphp\nsearch:\n  path: /t.php\n  query_param: q\n";
    let bypass_bodies = [
        "name: Bob's Tracker\nbase: &base\n  k: v\ncopy: *base\n",
        "name: Monitor 10\" HD\nbase: &base\n  k: v\ncopy: *base\n",
        "name: Bob 's Tracker\nbase: &base\n  k: v\ncopy: *base\n",
        "name: Bob\n  's Tracker\nbase: &base\n  k: v\ncopy: *base\n",
        "notes: \"hello \\\n\"\nbase: &base\n  k: v\ncopy: *base\n",
        "items:\n  - notes: |2\n      *ok\n    base: &alias\n      k: v\n    copy: *alias\n",
        "notes: |\nbase: &base\n  k: v\ncopy: *base\n",
        "x: !str &a hello\ny: *a\n",
        "a: &a 1\nitems: [1,*a]\n",
        "a: &a 1\nmap: {\"k\":*a}\n",
        "base: &base\n  k: v\nextra:\n  <<: *base\n",
    ];
    for body in bypass_bodies {
        let yaml = format!("{base_hdr}{body}");
        let err = validate_cmplugin_bytes(&build_site_zip(&yaml), dev_policy()).unwrap_err();
        assert!(
            matches!(&err, PackageError::InvalidManifest(msg) if msg.contains("anchor")),
            "expected anchor/alias InvalidManifest for `{body}`, got {err:?}"
        );
    }

    let bom_yaml = format!("\u{FEFF}{base_hdr}x: &a [*a]\n");
    let bom_err = validate_cmplugin_bytes(&build_site_zip(&bom_yaml), dev_policy()).unwrap_err();
    assert!(
        matches!(&bom_err, PackageError::InvalidManifest(msg) if msg.contains("anchor")),
        "expected anchor/alias InvalidManifest for BOM yaml, got {bom_err:?}"
    );

    let valid_bodies = [
        "notes: |\n  *bullet item\n  &literal entity\n",
        "notes: |-\n  # hash\n  &x\n",
        "notes: >+2\n    *folded\n",
        "items:\n  - |\n    *bullet\n    &entity\n",
        "name: Bob's \"10\\\" Tracker\n",
        "url: https://a.test/?a=1&b=2&q=*foo\n",
        "selector: a[href*=details]\n",
    ];
    for body in valid_bodies {
        let yaml = format!("{base_hdr}{body}");
        validate_cmplugin_bytes(&build_site_zip(&yaml), dev_policy())
            .unwrap_or_else(|e| panic!("valid YAML `{body}` failed: {e:?}"));
    }
}

#[test]
fn man_26_all_24_builtin_site_profiles_accepted_by_yaml_guard() {
    let profiles_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../indexer/src/profiles");
    let mut count = 0usize;
    for entry in std::fs::read_dir(&profiles_dir).expect("read profiles dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        count += 1;
        let raw_bytes = std::fs::read(&path).expect("read yaml bytes");
        let raw = String::from_utf8(raw_bytes).expect("profile yaml must be UTF-8");
        let patched = if let Some(rest) = raw.strip_prefix("id: ") {
            format!("id: custom-{rest}")
        } else if raw.contains("\nid: ") {
            raw.replacen("\nid: ", "\nid: custom-", 1)
        } else if raw.contains("\rid: ") {
            raw.replacen("\rid: ", "\rid: custom-", 1)
        } else {
            panic!("profile {} missing `id: ` line", path.display());
        };
        validate_cmplugin_bytes(&build_site_zip(&patched), dev_policy())
            .unwrap_or_else(|e| panic!("profile {} failed: {e:?}", path.display()));
    }
    assert_eq!(count, 24, "expected all 24 profiles to be checked");
}

#[test]
fn man_27_yaml_guard_rejects_all_linebreak_tag_flow_and_indent_bypasses() {
    let base_hdr =
        "id: custom-test\nframework: nexusphp\nsearch:\n  path: /t.php\n  query_param: q\n";

    let must_reject = [
        // 1. Bare CR, NEL (\u0085), LS (\u2028), PS (\u2029) after # comment and inside block scalars
        "a: # c\rb: &x 1\rc: *x\n",
        "notes: |\n  hello\rbase: &a\r  k: v\rcopy: *a\n",
        "a: # c\u{0085}b: &x 1\u{0085}c: *x\n",
        "notes: |\n  hello\u{2028}b: &x 1\u{2029}c: *x\n",
        // 2. Verbatim tags containing commas
        "x: !<tag:yaml.org,2002:seq>\n  - &a [1]\n  - *a\n",
        "x: !<tag:yaml.org,2002:str> &a hello\ny: *a\n",
        // 3. Tagged key before explicit-indent block scalar (!str foo: |2)
        "!str foo: |2\n  text\n&x bar: 1\nsub:\n  !str baz: |2\n    text\n  *x : 2\n",
        // 4. Flow-mode explicit key `?` and empty-key `:` without space
        "m: { ? &a 1: 2, sub: { ? *a : 3 } }\n",
        "m: { :&a 1, sub: { :*a } }\n",
        // 5. Multi-line flow inside block, comment inside flow, explicit block key, tabs & blank-indent
        "items: [\n  {\n    a: &x 1,\n    b: *x\n  }\n]\n",
        "a: [ foo\n  # comment\n  : &x 1 ]\n",
        "? a\n: |2\n  text\n&x: 1\n",
        "notes: |2\n  hi\n\t&x: 1\n",
        "a: |\n      \n  &x: 1\n",
        "|\n&a: 1\n",
    ];
    for body in must_reject {
        let yaml = format!("{base_hdr}{body}");
        let err = validate_cmplugin_bytes(&build_site_zip(&yaml), dev_policy())
            .expect_err(&format!("expected rejection for `{body:?}`"));
        assert!(
            matches!(err, PackageError::InvalidManifest(_)),
            "expected InvalidManifest for `{body:?}`, got {err:?}"
        );
    }

    let must_accept = [
        "x: !<tag:yaml.org,2002:str> hello\r\n",
        "x: !str hello\nitems: [\n  'a[href*=\"x\"]',\n  \"td > span\"\n]\n",
        "notes: |\n\n  # not a comment\n  *not_an_alias\n  &not_an_anchor\n",
        "a: |\n  ok\r\nb: \"line1 \\\r\n  line2\"\r\n",
    ];
    for body in must_accept {
        let yaml = format!("{base_hdr}{body}");
        validate_cmplugin_bytes(&build_site_zip(&yaml), dev_policy())
            .unwrap_or_else(|e| panic!("valid YAML `{body:?}` failed: {e:?}"));
    }
}

#[test]
fn man_28_provider_worlds_require_provide_cap_and_credential_binding_while_endpoint_allows_kv_only()
{
    let idx_missing_provide = replace_base_manifest(
        r#"credential_binding = "invocation-site"
[[components]]
id = "idx"
entry = "components/idx.wasm"
world = "crawler:plugin/indexer-plugin@1.0.0"
permissions = ["http:site-auth", "kv"]
[[components.indexers]]
id = "demo"
name = "Demo"
kind = "pt"
capabilities = ["search"]
site_schema = "schemas/site.schema.json"
"#,
    );
    assert!(matches!(
        validate_manifest_toml(&idx_missing_provide),
        Err(PackageError::InvalidManifest(_))
    ));

    let idx_missing_binding = replace_base_manifest(
        r#"[[components]]
id = "idx"
entry = "components/idx.wasm"
world = "crawler:plugin/indexer-plugin@1.0.0"
permissions = ["indexer:provide"]
[[components.indexers]]
id = "demo"
name = "Demo"
kind = "pt"
capabilities = ["search"]
site_schema = "schemas/site.schema.json"
"#,
    );
    assert!(matches!(
        validate_manifest_toml(&idx_missing_binding),
        Err(PackageError::InvalidManifest(_))
    ));

    let maint_missing_provide = replace_base_manifest(
        r#"credential_binding = "invocation-site"
[[components]]
id = "maint"
entry = "components/maint.wasm"
world = "crawler:plugin/maintenance-plugin@1.0.0"
permissions = ["http:site-auth"]
[[components.maintenance]]
id = "checkin"
actions = ["check-in"]
"#,
    );
    assert!(matches!(
        validate_manifest_toml(&maint_missing_provide),
        Err(PackageError::InvalidManifest(_))
    ));

    let maint_missing_binding = replace_base_manifest(
        r#"[[components]]
id = "maint"
entry = "components/maint.wasm"
world = "crawler:plugin/maintenance-plugin@1.0.0"
permissions = ["maintenance:provide"]
[[components.maintenance]]
id = "checkin"
actions = ["check-in"]
"#,
    );
    assert!(matches!(
        validate_manifest_toml(&maint_missing_binding),
        Err(PackageError::InvalidManifest(_))
    ));

    let ep_kv_only = replace_base_manifest(
        r#"[[components]]
id = "endpoint"
entry = "components/endpoint.wasm"
world = "crawler:plugin/endpoint-plugin@1.0.0"
permissions = ["kv"]
[[components.endpoints]]
id = "ping"
method = "POST"
path = "/ping"
auth = "admin"
input_schema = "schemas/config.schema.json"
capabilities = []
"#,
    );
    validate_manifest_toml(&ep_kv_only)
        .expect("endpoint-plugin with permissions = [\"kv\"] must remain valid");
}
