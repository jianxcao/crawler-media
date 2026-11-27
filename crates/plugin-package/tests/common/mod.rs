#![allow(dead_code)]

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use blake2::{Blake2b512, Digest};
use flate2::Compression;
use flate2::write::DeflateEncoder;
use plugin_package::{PackagePolicy, TrustedKey};
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use sha2::Sha256;
use std::io::Write;

pub const MINIMAL_WASM: &[u8] = b"\0asm\x0d\x00\x01\x00";

#[derive(Clone)]
pub struct ZipEntrySpec {
    pub name: Vec<u8>,
    pub compressed_data: Vec<u8>,
    pub method: u16,
    pub flags: u16,
    pub crc32: u32,
    pub claimed_uncompressed_size: u32,
    pub external_attr: u32,
    pub version_made_by: u16,
    pub extra: Vec<u8>,
}

impl ZipEntrySpec {
    pub fn stored(name: &str, data: &[u8]) -> Self {
        let crc = crc32fast::hash(data);
        Self {
            name: name.as_bytes().to_vec(),
            compressed_data: data.to_vec(),
            method: 0,
            flags: 0,
            crc32: crc,
            claimed_uncompressed_size: data.len() as u32,
            external_attr: 0o100644 << 16,
            version_made_by: (3 << 8) | 20,
            extra: Vec::new(),
        }
    }

    pub fn deflated(name: &str, data: &[u8]) -> Self {
        let crc = crc32fast::hash(data);
        let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
        enc.write_all(data).expect("deflate write");
        let compressed = enc.finish().expect("deflate finish");
        Self {
            name: name.as_bytes().to_vec(),
            compressed_data: compressed,
            method: 8,
            flags: 0,
            crc32: crc,
            claimed_uncompressed_size: data.len() as u32,
            external_attr: 0o100644 << 16,
            version_made_by: (3 << 8) | 20,
            extra: Vec::new(),
        }
    }

    pub fn dir(name: &str) -> Self {
        Self {
            name: name.as_bytes().to_vec(),
            compressed_data: Vec::new(),
            method: 0,
            flags: 0,
            crc32: 0,
            claimed_uncompressed_size: 0,
            external_attr: (0o040755 << 16) | 0x10,
            version_made_by: (3 << 8) | 20,
            extra: Vec::new(),
        }
    }
}

pub fn build_zip(entries: &[ZipEntrySpec]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cd = Vec::new();

    for entry in entries {
        let lfh_offset = out.len() as u32;
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&entry.flags.to_le_bytes());
        out.extend_from_slice(&entry.method.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&entry.crc32.to_le_bytes());
        out.extend_from_slice(&(entry.compressed_data.len() as u32).to_le_bytes());
        out.extend_from_slice(&entry.claimed_uncompressed_size.to_le_bytes());
        out.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entry.extra.len() as u16).to_le_bytes());
        out.extend_from_slice(&entry.name);
        out.extend_from_slice(&entry.extra);
        out.extend_from_slice(&entry.compressed_data);

        cd.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        cd.extend_from_slice(&entry.version_made_by.to_le_bytes());
        cd.extend_from_slice(&20u16.to_le_bytes());
        cd.extend_from_slice(&entry.flags.to_le_bytes());
        cd.extend_from_slice(&entry.method.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&entry.crc32.to_le_bytes());
        cd.extend_from_slice(&(entry.compressed_data.len() as u32).to_le_bytes());
        cd.extend_from_slice(&entry.claimed_uncompressed_size.to_le_bytes());
        cd.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
        cd.extend_from_slice(&(entry.extra.len() as u16).to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&entry.external_attr.to_le_bytes());
        cd.extend_from_slice(&lfh_offset.to_le_bytes());
        cd.extend_from_slice(&entry.name);
        cd.extend_from_slice(&entry.extra);
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
    pub key_id_hex: String,
    pub public_key_str: String,
    pub fingerprint: String,
    keypair: Ed25519KeyPair,
}

impl TestSigningKey {
    pub fn generate(key_id_seed: u64) -> Self {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).expect("pkcs8");
        let keypair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("keypair");
        let key_id_bytes = key_id_seed.to_le_bytes();
        let key_id_hex = format!("{key_id_seed:016X}");
        let mut raw_pk = Vec::with_capacity(42);
        raw_pk.extend_from_slice(b"Ed");
        raw_pk.extend_from_slice(&key_id_bytes);
        raw_pk.extend_from_slice(keypair.public_key().as_ref());
        let public_key_str = B64.encode(&raw_pk);
        let fp_digest = Sha256::digest(&raw_pk);
        let fingerprint = hex_lower(&fp_digest);
        Self {
            key_id_bytes,
            key_id_hex,
            public_key_str,
            fingerprint,
            keypair,
        }
    }

    pub fn trusted_key(&self, revoked: bool) -> TrustedKey {
        TrustedKey {
            key_id: self.key_id_hex.clone(),
            public_key: self.public_key_str.clone(),
            revoked,
        }
    }

    pub fn sign_sha256_hashed(&self, package_sha256: &[u8; 32], comment: &str) -> String {
        let digest = Blake2b512::digest(package_sha256);
        self.format_sig(b"ED", digest.as_slice(), comment)
    }

    pub fn sign_sha256_legacy(&self, package_sha256: &[u8; 32], comment: &str) -> String {
        self.format_sig(b"Ed", package_sha256, comment)
    }

    fn format_sig(&self, alg: &[u8; 2], msg: &[u8], trusted_comment: &str) -> String {
        let main_sig = self.keypair.sign(msg);
        let mut sig_raw = Vec::with_capacity(74);
        sig_raw.extend_from_slice(alg);
        sig_raw.extend_from_slice(&self.key_id_bytes);
        sig_raw.extend_from_slice(main_sig.as_ref());

        let mut global_msg = Vec::with_capacity(64 + trusted_comment.len());
        global_msg.extend_from_slice(main_sig.as_ref());
        global_msg.extend_from_slice(trusted_comment.as_bytes());
        let global_sig = self.keypair.sign(&global_msg);

        format!(
            "untrusted comment: signature from crawler-media test\n{}\ntrusted comment: {}\n{}\n",
            B64.encode(&sig_raw),
            trusted_comment,
            B64.encode(global_sig.as_ref()),
        )
    }
}

pub fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    let d = Sha256::digest(data);
    let mut out = [0u8; 32];
    out.copy_from_slice(&d);
    out
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex_lower(&sha256_bytes(data))
}

pub fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

pub fn minimal_config_schema_json() -> &'static str {
    r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "additionalProperties": false,
  "required": ["enabled"],
  "properties": {
    "enabled": { "type": "boolean", "default": true }
  }
}"#
}

pub fn minimal_hook_manifest_toml() -> &'static str {
    r#"manifest_version = 1
id = "org.example.hook-demo"
name = "Hook Demo"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Example Author"]
description = "Minimal hook plugin"
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
"#
}

pub fn minimal_site_yaml(id: &str) -> String {
    format!(
        "id: {id}\nframework: nexusphp\nsearch:\n  path: /torrents.php\n  query_param: search\n"
    )
}

pub fn build_minimal_hook_zip() -> Vec<u8> {
    build_zip(&[
        ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
    ])
}

pub fn dev_policy() -> PackagePolicy {
    PackagePolicy {
        dev_mode: true,
        ..PackagePolicy::default()
    }
}
