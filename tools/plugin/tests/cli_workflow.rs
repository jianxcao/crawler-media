use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use blake2::Blake2bVar;
use blake2::digest::{Update, VariableOutput};
use plugin_cli::run;
use plugin_package::{
    PackagePolicy, SignatureStatus, TrustedKey, parse_signing_key, validate_cmplugin_bytes,
    verify_minisign,
};
use ring::signature::{Ed25519KeyPair, KeyPair};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use tempfile::TempDir;

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

fn build_unencrypted_minisign_secret(seed: [u8; 32], key_id: [u8; 8]) -> String {
    let kp = Ed25519KeyPair::from_seed_unchecked(&seed).expect("kp");
    let mut sk_pk = [0u8; 64];
    sk_pk[..32].copy_from_slice(&seed);
    sk_pk[32..].copy_from_slice(kp.public_key().as_ref());

    let mut hasher = Blake2bVar::new(32).expect("blake2b");
    hasher.update(b"Ed");
    hasher.update(&key_id);
    hasher.update(&sk_pk);
    let mut cksum = [0u8; 32];
    hasher.finalize_variable(&mut cksum).expect("cksum");

    let mut raw = Vec::with_capacity(158);
    raw.extend_from_slice(b"Ed");
    raw.extend_from_slice(&[0u8, 0u8]);
    raw.extend_from_slice(b"B2");
    raw.extend_from_slice(&[0u8; 32]);
    raw.extend_from_slice(&[0u8; 8]);
    raw.extend_from_slice(&[0u8; 8]);
    raw.extend_from_slice(&key_id);
    raw.extend_from_slice(&sk_pk);
    raw.extend_from_slice(&cksum);
    format!("untrusted comment: minisign key\n{}\n", B64.encode(raw))
}

#[test]
fn help_commands_succeed() {
    assert!(run(&args(&["--help"])).is_ok());
    for sub in [
        "scaffold", "validate", "test", "build", "pack", "sign", "dev",
    ] {
        assert!(run(&args(&[sub, "--help"])).is_ok(), "{sub} --help failed");
    }
}

#[test]
fn scaffold_rejects_reserved_id_unsupported_api_and_unknown_world_without_creating_dir() {
    let tmp = TempDir::new().expect("tmp");
    let s1 = tmp.path().join("s1");
    assert!(
        run(&args(&[
            "scaffold",
            "--id",
            "installed",
            "--world",
            "hook-plugin",
            &s1.to_string_lossy(),
        ]))
        .is_err()
    );
    assert!(!s1.exists());

    let s2 = tmp.path().join("s2");
    assert!(
        run(&args(&[
            "scaffold",
            "--api",
            "2.0.0",
            "--world",
            "hook-plugin",
            &s2.to_string_lossy(),
        ]))
        .is_err()
    );
    assert!(!s2.exists());

    let s3 = tmp.path().join("s3");
    assert!(
        run(&args(&[
            "scaffold",
            "--world",
            "foo-plugin",
            "--api",
            "1.0.0",
            &s3.to_string_lossy(),
        ]))
        .is_err()
    );
    assert!(!s3.exists());
}

#[test]
fn validate_accepts_all_five_worlds_and_rejects_bad_manifest_or_recursive_schema() {
    let tmp = TempDir::new().expect("tmp");
    for world in [
        "hook-plugin",
        "task-plugin",
        "endpoint-plugin",
        "indexer-plugin",
        "maintenance-plugin",
    ] {
        let proj = tmp.path().join(world);
        run(&args(&[
            "scaffold",
            "--world",
            world,
            "--api",
            "1.0.0",
            &proj.to_string_lossy(),
        ]))
        .unwrap_or_else(|e| panic!("scaffold {world}: {e}"));
        assert!(proj.join("plugin.toml").is_file());
        assert!(proj.join("schemas/config.schema.json").is_file());
        run(&args(&["validate", &proj.to_string_lossy()]))
            .unwrap_or_else(|e| panic!("validate {world}: {e}"));
    }

    let bad = tmp.path().join("bad");
    fs::create_dir_all(&bad).expect("mkdir");
    fs::write(
        bad.join("plugin.toml"),
        "manifest_version = 1\nid = \"installed\"\n",
    )
    .expect("write");
    assert!(run(&args(&["validate", &bad.to_string_lossy()])).is_err());

    let rec = tmp.path().join("hook-plugin");
    fs::write(
        rec.join("schemas/config.schema.json"),
        r##"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","$defs":{"n":{"type":"object","properties":{"next":{"$ref":"#/$defs/n"}}}},"properties":{"root":{"$ref":"#/$defs/n"}}}"##,
    )
    .expect("write");
    assert!(run(&args(&["validate", &rec.to_string_lossy()])).is_err());
}

#[test]
fn pack_rejects_forbidden_ui_assets_without_writing_output() {
    let tmp = TempDir::new().expect("tmp");
    let proj = tmp.path().join("ui_bad");
    run(&args(&[
        "scaffold",
        "--world",
        "hook-plugin",
        "--api",
        "1.0.0",
        "--ui",
        "react",
        &proj.to_string_lossy(),
    ]))
    .expect("scaffold");
    fs::create_dir_all(proj.join("ui/assets")).expect("mkdir");
    fs::write(proj.join("ui/assets/index.html"), "<html></html>").expect("write");
    let out_pkg = tmp.path().join("should_not_exist.cmplugin");
    assert!(
        run(&args(&[
            "pack",
            &proj.to_string_lossy(),
            "--output",
            &out_pkg.to_string_lossy(),
        ]))
        .is_err()
    );
    assert!(!out_pkg.exists());
}

#[test]
fn sign_missing_inputs_does_not_leave_partial_minisig() {
    let tmp = TempDir::new().expect("tmp");
    let missing_pkg = tmp.path().join("missing.cmplugin");
    let missing_key = tmp.path().join("missing.key");
    assert!(
        run(&args(&[
            "sign",
            &missing_pkg.to_string_lossy(),
            "--key",
            &missing_key.to_string_lossy(),
        ]))
        .is_err()
    );
    assert!(!tmp.path().join("missing.cmplugin.minisig").exists());
}

fn scaffold_build_and_pack_hook_ui(proj: &Path, out_pkg: &Path) -> Vec<u8> {
    run(&args(&[
        "scaffold",
        "--world",
        "hook-plugin",
        "--api",
        "1.0.0",
        "--ui",
        "react",
        "--id",
        "org.example.full-hook-ui",
        &proj.to_string_lossy(),
    ]))
    .expect("scaffold");

    run(&args(&["validate", &proj.to_string_lossy()])).expect("validate project");
    run(&args(&["test", &proj.to_string_lossy(), "--offline"])).expect("test --offline");
    run(&args(&["build", &proj.to_string_lossy()])).expect("build");

    assert!(proj.join("components/main.wasm").is_file());
    assert!(proj.join("ui/assets/remoteEntry.js").is_file());
    assert!(proj.join("ui/asset-manifest.json").is_file());

    run(&args(&[
        "pack",
        &proj.to_string_lossy(),
        "--output",
        &out_pkg.to_string_lossy(),
    ]))
    .expect("pack");
    assert!(out_pkg.is_file());
    run(&args(&["validate", &out_pkg.to_string_lossy()])).expect("validate unsigned archive");

    let pkg_bytes = fs::read(out_pkg).expect("read pkg");
    let dev_validated = validate_cmplugin_bytes(
        &pkg_bytes,
        &PackagePolicy {
            dev_mode: true,
            ..PackagePolicy::default()
        },
    )
    .expect("server policy dev validate");
    assert_eq!(dev_validated.manifest.id, "org.example.full-hook-ui");
    assert!(dev_validated.ui.is_some());
    pkg_bytes
}

fn verify_signing_and_post_sign_validate(work_dir: &Path, out_pkg: &Path, pkg_bytes: &[u8]) {
    let seed = [0x73u8; 32];
    let hex_key_path = work_dir.join("hex.key");
    let hex_seed_str: String = seed.iter().map(|b| format!("{b:02x}")).collect();
    fs::write(&hex_key_path, &hex_seed_str).expect("write hex key");
    run(&args(&[
        "sign",
        &out_pkg.to_string_lossy(),
        "--key",
        &hex_key_path.to_string_lossy(),
    ]))
    .expect("sign hex");

    let sig_path = Path::new(&format!("{}.minisig", out_pkg.display())).to_path_buf();
    assert!(sig_path.is_file());
    let parsed = parse_signing_key(&hex_seed_str).expect("parse");

    // Offline validate AFTER sign without --trust-key validates minisig syntax + reports key_id
    let offline_out =
        run(&args(&["validate", &out_pkg.to_string_lossy()])).expect("validate signed offline");
    assert!(offline_out.contains(&parsed.key_id_hex()));

    // Validate AFTER sign WITH --trust-key (inline pubkey & pubkey file) verifies Ed25519 signature
    let pubkey_file = work_dir.join("trusted.pub");
    fs::write(&pubkey_file, parsed.public_key_text()).expect("write pubkey");
    let signed_out = run(&args(&[
        "validate",
        &out_pkg.to_string_lossy(),
        "--trust-key",
        &pubkey_file.to_string_lossy(),
    ]))
    .expect("validate signed with --trust-key");
    assert!(signed_out.contains(&parsed.key_id_hex()));

    // Wrong trust key must reject
    let wrong_parsed = parse_signing_key(
        &[0x21u8; 32]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
    )
    .expect("wrong key");
    assert!(
        run(&args(&[
            "validate",
            &out_pkg.to_string_lossy(),
            "--trust-key",
            &wrong_parsed.public_key_text(),
        ]))
        .is_err()
    );

    // Sign with unencrypted minisign secret key file & verify
    let ms_key_path = work_dir.join("minisign.key");
    let ms_text = build_unencrypted_minisign_secret(seed, [1, 2, 3, 4, 5, 6, 7, 8]);
    fs::write(&ms_key_path, &ms_text).expect("write ms key");
    run(&args(&[
        "sign",
        &out_pkg.to_string_lossy(),
        "--key",
        &ms_key_path.to_string_lossy(),
    ]))
    .expect("sign minisign");
    let ms_sig_text = fs::read_to_string(&sig_path).expect("read ms sig");
    let parsed_ms = parse_signing_key(&ms_text).expect("parse ms");
    let digest: [u8; 32] = Sha256::digest(pkg_bytes).into();
    let ver = verify_minisign(
        &digest,
        &ms_sig_text,
        &[TrustedKey {
            key_id: parsed_ms.key_id_hex(),
            public_key: parsed_ms.public_key_text(),
            revoked: false,
        }],
    )
    .expect("verify minisign");
    assert!(matches!(ver.status, SignatureStatus::Trusted { .. }));

    // Corrupt minisig syntax must fail even without --trust-key
    fs::write(&sig_path, "corrupt minisig\n").expect("corrupt sig");
    assert!(run(&args(&["validate", &out_pkg.to_string_lossy()])).is_err());
}

fn verify_dev_sideload_against_mock_server(proj: &Path) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let srv = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let mut content_len: Option<usize> = None;
        let mut header_end: Option<usize> = None;
        loop {
            let n = stream.read(&mut chunk).expect("read");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if header_end.is_none()
                && let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n")
            {
                header_end = Some(pos + 4);
                let hdrs = String::from_utf8_lossy(&buf[..pos]);
                for line in hdrs.lines() {
                    if let Some((k, v)) = line.split_once(':')
                        && k.trim().eq_ignore_ascii_case("content-length")
                    {
                        content_len = v.trim().parse().ok();
                    }
                }
            }
            if let (Some(he), Some(cl)) = (header_end, content_len)
                && buf.len() >= he + cl
            {
                break;
            }
        }
        let he = header_end.expect("headers");
        let hdr_str = String::from_utf8_lossy(&buf[..he]);
        assert!(hdr_str.starts_with("POST /api/v1/plugins/dev "));
        assert!(hdr_str.contains("test-dev-tok"));
        let v = validate_cmplugin_bytes(
            &buf[he..],
            &PackagePolicy {
                dev_mode: true,
                ..PackagePolicy::default()
            },
        )
        .expect("dev body valid cmplugin");
        assert_eq!(v.manifest.id, "org.example.full-hook-ui");
        let resp_body = format!(
            "{{\"ok\":true,\"data\":{{\"plugin_id\":\"{}\",\"hash\":\"{}\"}}}}",
            v.manifest.id, v.package_sha256_hex
        );
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            resp_body.len(),
            resp_body
        );
        stream.write_all(resp.as_bytes()).expect("write");
    });

    run(&args(&[
        "dev",
        &proj.to_string_lossy(),
        "--instance",
        &format!("http://{addr}"),
        "--token",
        "test-dev-tok",
    ]))
    .expect("dev upload");
    srv.join().expect("server thread");
}

#[test]
fn full_offline_workflow_scaffold_test_build_pack_sign_and_dev_upload() {
    let tmp = TempDir::new().expect("tmp");
    let proj = tmp.path().join("full_hook_ui");
    let out_pkg = tmp.path().join("full_hook_ui.cmplugin");

    let pkg_bytes = scaffold_build_and_pack_hook_ui(&proj, &out_pkg);
    verify_signing_and_post_sign_validate(tmp.path(), &out_pkg, &pkg_bytes);
    verify_dev_sideload_against_mock_server(&proj);
}

#[test]
fn remaining_worlds_and_ui_only_build_and_pack_offline() {
    let tmp = TempDir::new().expect("tmp");
    for world in [
        "task-plugin",
        "endpoint-plugin",
        "indexer-plugin",
        "maintenance-plugin",
    ] {
        let proj = tmp.path().join(format!("bld_{world}"));
        let pkg = tmp.path().join(format!("bld_{world}.cmplugin"));
        run(&args(&[
            "scaffold",
            "--world",
            world,
            "--api",
            "1.0.0",
            &proj.to_string_lossy(),
        ]))
        .expect("scaffold");
        run(&args(&["test", &proj.to_string_lossy(), "--offline"])).expect("test");
        run(&args(&["build", &proj.to_string_lossy()])).expect("build");
        run(&args(&[
            "pack",
            &proj.to_string_lossy(),
            "--output",
            &pkg.to_string_lossy(),
        ]))
        .expect("pack");
        assert!(pkg.is_file());
    }

    let ui_only = tmp.path().join("ui_only_proj");
    let ui_pkg = tmp.path().join("ui_only.cmplugin");
    run(&args(&[
        "scaffold",
        "--api",
        "1.0.0",
        "--ui",
        "react",
        &ui_only.to_string_lossy(),
    ]))
    .expect("scaffold ui only");
    run(&args(&["validate", &ui_only.to_string_lossy()])).expect("validate ui only");
    run(&args(&["build", &ui_only.to_string_lossy()])).expect("build ui only");
    run(&args(&[
        "pack",
        &ui_only.to_string_lossy(),
        "--output",
        &ui_pkg.to_string_lossy(),
    ]))
    .expect("pack ui only");
    assert!(ui_pkg.is_file());
}
