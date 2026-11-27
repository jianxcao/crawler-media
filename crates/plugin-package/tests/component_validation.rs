mod common;

use common::{
    ZipEntrySpec, build_zip, dev_policy, minimal_config_schema_json, minimal_hook_manifest_toml,
};
use plugin_package::{PackageError, validate_cmplugin_bytes};

const POLICY_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/policy.wasm");
const TASK_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/task_fixture.wasm");
const ENDPOINT_WASM: &[u8] =
    include_bytes!("../../plugin-host/tests/fixtures/endpoint_fixture.wasm");

// Valid Core WASM v1 module (magic \0asm + version 1 + empty function body), NOT a Component Model binary.
const CORE_WASM_V1: &[u8] =
    b"\0asm\x01\0\0\0\x01\x04\x01\x60\x00\x00\x03\x02\x01\x00\x0a\x04\x01\x02\x00\x0b";

#[test]
fn comp_01_real_compiled_components_accepted() {
    let manifest_toml = r#"manifest_version = 1
id = "org.example.real-components"
name = "Real Components"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Example Author"]
description = "All three compiled component fixtures"
config_schema = "schemas/config.schema.json"
state_schema = 1

[[components]]
id = "policy"
entry = "components/policy.wasm"
world = "crawler:plugin/hook-plugin@1.0.0"
permissions = ["hooks:search.after"]

[[components.hooks]]
id = "search.after"
priority = 100

[[components]]
id = "tasks"
entry = "components/task_fixture.wasm"
world = "crawler:plugin/task-plugin@1.0.0"

[[components.tasks]]
id = "sync"
input_schema = "schemas/task.schema.json"

[components.tasks.schedule]
kind = "interval"
seconds = 60

[[components]]
id = "api"
entry = "components/endpoint_fixture.wasm"
world = "crawler:plugin/endpoint-plugin@1.0.0"
permissions = ["endpoints"]

[[components.endpoints]]
id = "ping"
method = "GET"
path = "/ping"
auth = "admin"
input_schema = "schemas/endpoint.schema.json"
"#;

    let empty_obj_schema = r#"{"type":"object","additionalProperties":false}"#;
    let zip = build_zip(&[
        ZipEntrySpec::stored("plugin.toml", manifest_toml.as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("schemas/task.schema.json", empty_obj_schema.as_bytes()),
        ZipEntrySpec::stored("schemas/endpoint.schema.json", empty_obj_schema.as_bytes()),
        ZipEntrySpec::stored("components/policy.wasm", POLICY_WASM),
        ZipEntrySpec::stored("components/task_fixture.wasm", TASK_WASM),
        ZipEntrySpec::stored("components/endpoint_fixture.wasm", ENDPOINT_WASM),
    ]);

    let pkg = validate_cmplugin_bytes(&zip, dev_policy())
        .expect("real Component Model wasm fixtures must be accepted");
    assert_eq!(pkg.components.len(), 3);
    assert_eq!(pkg.components[0].wasm_bytes, POLICY_WASM);
    assert_eq!(pkg.components[1].wasm_bytes, TASK_WASM);
    assert_eq!(pkg.components[2].wasm_bytes, ENDPOINT_WASM);
}

#[test]
fn comp_02_core_wasm_v1_rejected() {
    let zip = build_zip(&[
        ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("components/policy.wasm", CORE_WASM_V1),
    ]);

    let err = validate_cmplugin_bytes(&zip, dev_policy())
        .expect_err("Core WASM v1 module must be rejected");
    assert!(
        matches!(err, PackageError::InvalidManifest(_)),
        "expected InvalidManifest for Core WASM v1, got {err:?}"
    );
}
