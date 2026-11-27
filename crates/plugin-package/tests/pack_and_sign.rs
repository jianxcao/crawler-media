use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use blake2::digest::{Update, VariableOutput};
use blake2::{Blake2b512, Blake2bVar, Digest};
use plugin_package::{
    PackageError, PackagePolicy, SignatureStatus, TrustedKey, build_deterministic_cmplugin_zip,
    is_forbidden_ui_asset_path, parse_signing_key, reject_yaml_anchors, sign_cmplugin_bytes,
    validate_cmplugin_bytes, validate_safe_package_path, verify_minisign,
};
use ring::signature::{Ed25519KeyPair, KeyPair};
use sha2::Sha256;
use std::collections::BTreeMap;

const MINIMAL_COMPONENT_WASM: &[u8] =
    include_bytes!("../../plugin-host/tests/fixtures/policy.wasm");

fn sample_entries() -> BTreeMap<String, Vec<u8>> {
    let mut map = BTreeMap::new();
    map.insert(
        "plugin.toml".to_string(),
        br#"manifest_version = 1
id = "org.example.pack-test"
name = "Pack Test"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Tester"]
description = "Pack test plugin"
config_schema = "schemas/config.schema.json"
state_schema = 1

[[components]]
id = "main"
entry = "components/main.wasm"
world = "crawler:plugin/hook-plugin@1.0.0"
permissions = []

[[components.hooks]]
id = "search.after"
priority = 100
"#
        .to_vec(),
    );
    map.insert(
        "schemas/config.schema.json".to_string(),
        br#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#
            .to_vec(),
    );
    map.insert(
        "components/main.wasm".to_string(),
        MINIMAL_COMPONENT_WASM.to_vec(),
    );
    map
}

fn build_unencrypted_minisign_secret_key(seed: [u8; 32], key_id: [u8; 8]) -> String {
    let kp = Ed25519KeyPair::from_seed_unchecked(&seed).expect("ed25519 keypair");
    let pk = kp.public_key().as_ref();
    let mut sk_and_pk = [0u8; 64];
    sk_and_pk[..32].copy_from_slice(&seed);
    sk_and_pk[32..].copy_from_slice(pk);

    let mut cksum_hasher = Blake2bVar::new(32).expect("blake2b-256");
    cksum_hasher.update(b"Ed");
    cksum_hasher.update(&key_id);
    cksum_hasher.update(&sk_and_pk);
    let mut cksum = [0u8; 32];
    cksum_hasher.finalize_variable(&mut cksum).expect("cksum");

    let mut raw = Vec::with_capacity(158);
    raw.extend_from_slice(b"Ed");
    raw.extend_from_slice(&[0u8, 0u8]);
    raw.extend_from_slice(b"B2");
    raw.extend_from_slice(&[0u8; 32]);
    raw.extend_from_slice(&[0u8; 8]);
    raw.extend_from_slice(&[0u8; 8]);
    raw.extend_from_slice(&key_id);
    raw.extend_from_slice(&sk_and_pk);
    raw.extend_from_slice(&cksum);

    format!(
        "untrusted comment: minisign unencrypted secret key\n{}\n",
        B64.encode(raw)
    )
}

#[test]
fn deterministic_zip_round_trips_and_rejects_traversal_and_collisions() {
    let entries = sample_entries();
    let zip_a = build_deterministic_cmplugin_zip(&entries).expect("pack a");
    let zip_b = build_deterministic_cmplugin_zip(&entries).expect("pack b");
    assert_eq!(
        zip_a, zip_b,
        "deterministic pack output must match byte-for-byte"
    );

    let policy = PackagePolicy {
        dev_mode: true,
        ..PackagePolicy::default()
    };
    let validated = validate_cmplugin_bytes(&zip_a, &policy).expect("valid cmplugin zip");
    assert_eq!(validated.manifest.id, "org.example.pack-test");

    let mut bad_traversal = entries.clone();
    bad_traversal.insert("../evil.txt".to_string(), b"x".to_vec());
    assert!(matches!(
        build_deterministic_cmplugin_zip(&bad_traversal),
        Err(PackageError::UnsafePath { .. })
    ));

    let mut bad_collision = entries;
    bad_collision.insert("Plugin.toml".to_string(), b"dup".to_vec());
    assert!(matches!(
        build_deterministic_cmplugin_zip(&bad_collision),
        Err(PackageError::PathCollision { .. })
    ));
}

#[test]
fn signs_with_hex_seed_and_unencrypted_minisign_secret_key() {
    let zip_bytes = build_deterministic_cmplugin_zip(&sample_entries()).expect("pack");
    let sha_digest: [u8; 32] = Sha256::digest(&zip_bytes).into();

    let seed = [0x42u8; 32];
    let hex_key = seed.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let parsed_hex = parse_signing_key(&hex_key).expect("parse hex seed");
    let sig_hex = sign_cmplugin_bytes(&zip_bytes, &parsed_hex, "test.cmplugin").expect("sign hex");

    let trusted_hex = vec![TrustedKey {
        key_id: parsed_hex.key_id_hex(),
        public_key: parsed_hex.public_key_text(),
        revoked: false,
    }];
    let ver = verify_minisign(&sha_digest, &sig_hex, &trusted_hex).expect("verify hex signature");
    assert!(matches!(ver.status, SignatureStatus::Trusted { .. }));

    let mut custom_key_id = [0u8; 8];
    let pre = Blake2b512::digest(b"custom-id");
    custom_key_id.copy_from_slice(&pre[..8]);
    let minisign_sk = build_unencrypted_minisign_secret_key(seed, custom_key_id);
    let parsed_ms = parse_signing_key(&minisign_sk).expect("parse minisign secret key");
    assert_eq!(parsed_ms.key_id_bytes, custom_key_id);
    let sig_ms =
        sign_cmplugin_bytes(&zip_bytes, &parsed_ms, "test.cmplugin").expect("sign minisign");
    let trusted_ms = vec![TrustedKey {
        key_id: parsed_ms.key_id_hex(),
        public_key: parsed_ms.public_key_text(),
        revoked: false,
    }];
    let ver_ms =
        verify_minisign(&sha_digest, &sig_ms, &trusted_ms).expect("verify minisign signature");
    assert!(matches!(ver_ms.status, SignatureStatus::Trusted { .. }));
}

#[test]
fn exposes_shared_path_yaml_and_ui_asset_guards() {
    assert!(validate_safe_package_path("schemas/config.schema.json").is_ok());
    assert!(validate_safe_package_path("../escape").is_err());
    assert!(reject_yaml_anchors("id: demo\n", "sites/demo.yaml").is_ok());
    assert!(reject_yaml_anchors("a: &x\n  k: 1\n", "sites/demo.yaml").is_err());
    assert!(is_forbidden_ui_asset_path("ui/assets/index.html"));
    assert!(is_forbidden_ui_asset_path("ui/assets/app.js.map"));
    assert!(!is_forbidden_ui_asset_path("ui/assets/remoteEntry.js"));
}
