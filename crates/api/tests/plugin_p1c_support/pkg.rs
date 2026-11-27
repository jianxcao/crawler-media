use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use blake2::{Blake2b512, Digest as Blake2Digest};
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use sha2::Sha256;

pub fn build_stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cd = Vec::new();
    for (name, data) in entries {
        let lfh_offset = out.len() as u32;
        let crc = crc32fast::hash(data);
        let name_bytes = name.as_bytes();
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(data);

        cd.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        cd.extend_from_slice(&((3u16 << 8) | 20u16).to_le_bytes());
        cd.extend_from_slice(&20u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&crc.to_le_bytes());
        cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&(0o100644u32 << 16).to_le_bytes());
        cd.extend_from_slice(&lfh_offset.to_le_bytes());
        cd.extend_from_slice(name_bytes);
    }
    let cd_offset = out.len() as u32;
    let cd_size = cd.len() as u32;
    out.extend_from_slice(&cd);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

pub struct TestSigningKey {
    pub key_id_bytes: [u8; 8],
    pub public_key_str: String,
    pub keypair: Ed25519KeyPair,
}

impl TestSigningKey {
    pub fn generate(seed: u64) -> Self {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let keypair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let key_id_bytes = seed.to_le_bytes();
        let mut raw_pk = Vec::with_capacity(42);
        raw_pk.extend_from_slice(b"Ed");
        raw_pk.extend_from_slice(&key_id_bytes);
        raw_pk.extend_from_slice(keypair.public_key().as_ref());
        Self {
            key_id_bytes,
            public_key_str: B64.encode(&raw_pk),
            keypair,
        }
    }

    pub fn sign_package(&self, zip_bytes: &[u8]) -> String {
        let sha: [u8; 32] = Sha256::digest(zip_bytes).into();
        let digest = Blake2b512::digest(sha);
        let main_sig = self.keypair.sign(digest.as_slice());
        let mut sig_raw = Vec::with_capacity(74);
        sig_raw.extend_from_slice(b"ED");
        sig_raw.extend_from_slice(&self.key_id_bytes);
        sig_raw.extend_from_slice(main_sig.as_ref());
        let comment = "test signature";
        let mut global_msg = Vec::with_capacity(64 + comment.len());
        global_msg.extend_from_slice(main_sig.as_ref());
        global_msg.extend_from_slice(comment.as_bytes());
        let global_sig = self.keypair.sign(&global_msg);
        format!(
            "untrusted comment: test\n{}\ntrusted comment: {}\n{}\n",
            B64.encode(&sig_raw),
            comment,
            B64.encode(global_sig.as_ref()),
        )
    }
}

pub fn task_wasm() -> &'static [u8] {
    include_bytes!("../../../plugin-host/tests/fixtures/task_fixture.wasm")
}

pub fn sample_schema(require_secret: bool, desc: Option<&str>) -> String {
    let required = if require_secret {
        r#"["mode", "api_secret"]"#
    } else {
        r#"["mode"]"#
    };
    let desc_line = desc
        .map(|d| format!(r#""description": "{d}","#))
        .unwrap_or_default();
    format!(
        r#"{{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  {desc_line}
  "type": "object",
  "additionalProperties": false,
  "required": {required},
  "properties": {{
    "mode": {{ "type": "string", "default": "ok" }},
    "api_secret": {{ "type": "string", "minLength": 4, "x-ui": {{ "widget": "password" }} }}
  }}
}}"#
    )
}

pub fn schema_rev(require_secret: bool, desc: Option<&str>) -> String {
    let raw: serde_json::Value =
        serde_json::from_str(&sample_schema(require_secret, desc)).unwrap();
    plugin_package::validate_config_schema(&raw)
        .unwrap()
        .schema_revision()
        .to_string()
}

pub fn task_manifest_with_options(
    id: &str,
    version: &str,
    state_schema: u32,
    credential_bound: bool,
    permissions: &[&str],
) -> String {
    let perms = permissions
        .iter()
        .map(|p| format!(r#""{p}""#))
        .collect::<Vec<_>>()
        .join(", ");
    let cred_line = if credential_bound {
        "credential_binding = \"invocation-site\"\n"
    } else {
        ""
    };
    format!(
        r#"manifest_version = 1
id = "{id}"
name = "Task Plugin {version}"
version = "{version}"
api = "1.0.0"
license = "MIT"
authors = ["Test Author"]
description = "P1c test plugin"
config_schema = "schemas/config.schema.json"
state_schema = {state_schema}
{cred_line}
[[components]]
id = "worker"
entry = "components/worker.wasm"
world = "crawler:plugin/task-plugin@1.0.0"
permissions = [{perms}]

[[components.tasks]]
id = "sync"
input_schema = "schemas/task.schema.json"
[components.tasks.schedule]
kind = "interval"
seconds = 300
"#
    )
}

pub fn build_task_package_custom(
    id: &str,
    version: &str,
    state_schema: u32,
    require_secret: bool,
    desc: Option<&str>,
    credential_bound: bool,
    permissions: &[&str],
) -> Vec<u8> {
    let manifest =
        task_manifest_with_options(id, version, state_schema, credential_bound, permissions);
    let schema = sample_schema(require_secret, desc);
    let task_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{"action":{"type":"string"}}}"#;
    build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("schemas/config.schema.json", schema.as_bytes()),
        ("schemas/task.schema.json", task_schema.as_bytes()),
        ("components/worker.wasm", task_wasm()),
    ])
}

pub fn build_task_package(
    id: &str,
    version: &str,
    state_schema: u32,
    require_secret: bool,
) -> Vec<u8> {
    build_task_package_custom(
        id,
        version,
        state_schema,
        require_secret,
        None,
        false,
        &[
            "kv",
            "blob",
            "command:schedule.set",
            "command:schedule.disable",
        ],
    )
}

pub fn build_task_package_with_schema_path(id: &str, version: &str, schema_path: &str) -> Vec<u8> {
    let manifest = format!(
        r#"manifest_version = 1
id = "{id}"
name = "Task Plugin {version}"
version = "{version}"
api = "1.0.0"
license = "MIT"
authors = ["Test Author"]
description = "P1c test plugin"
config_schema = "schemas/config.schema.json"
state_schema = 1

[[components]]
id = "worker"
entry = "components/worker.wasm"
world = "crawler:plugin/task-plugin@1.0.0"
permissions = ["kv"]

[[components.tasks]]
id = "sync"
input_schema = "{schema_path}"
[components.tasks.schedule]
kind = "interval"
seconds = 300
"#
    );
    let schema = sample_schema(false, None);
    let task_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{"action":{"type":"string"}}}"#;
    build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("schemas/config.schema.json", schema.as_bytes()),
        (schema_path, task_schema.as_bytes()),
        ("components/worker.wasm", task_wasm()),
    ])
}
