use plugin_cli::{CliError, run};
use plugin_package::{
    PackagePolicy, SignatureStatus, TrustedKey, parse_signing_key, validate_cmplugin_bytes,
};
use std::fs;
use tempfile::TempDir;

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn keygen_creates_hex_seed_and_pubkey_compatible_with_sign_and_validate() {
    let tmp = TempDir::new().expect("tmp");
    let key_path = tmp.path().join("keys/official.key");
    let pubkey_path = tmp.path().join("keys/official.pub");

    let out = run(&args(&[
        "keygen",
        "--key",
        &key_path.to_string_lossy(),
        "--pubkey",
        &pubkey_path.to_string_lossy(),
    ]))
    .expect("keygen succeeds");

    let key_text = fs::read_to_string(&key_path).expect("read secret key");
    let trimmed_key = key_text.trim();
    assert_eq!(trimmed_key.len(), 64, "secret key must be 64-hex seed");
    assert!(
        trimmed_key.bytes().all(|b| b.is_ascii_hexdigit()),
        "secret key must be hex"
    );
    assert!(
        !out.contains(trimmed_key),
        "keygen output must never print or log secret key"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&key_path)
            .expect("key metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "secret key must have 0600 permissions on Unix");
    }

    let pubkey_text = fs::read_to_string(&pubkey_path).expect("read pubkey");
    let parsed_secret = parse_signing_key(trimmed_key).expect("parse generated secret key");
    assert_eq!(pubkey_text, parsed_secret.public_key_text());
    assert!(out.contains(&parsed_secret.key_id_hex()));

    // Verify generated keys work end-to-end with scaffold -> build -> pack -> sign -> validate
    let proj = tmp.path().join("sample_plugin");
    let pkg_path = tmp.path().join("sample.cmplugin");
    run(&args(&[
        "scaffold",
        "--world",
        "task-plugin",
        "--api",
        "1.0.0",
        "--id",
        "org.example.keygen-check",
        &proj.to_string_lossy(),
    ]))
    .expect("scaffold");
    run(&args(&["build", &proj.to_string_lossy()])).expect("build");
    run(&args(&[
        "pack",
        &proj.to_string_lossy(),
        "--output",
        &pkg_path.to_string_lossy(),
    ]))
    .expect("pack");
    run(&args(&[
        "sign",
        &pkg_path.to_string_lossy(),
        "--key",
        &key_path.to_string_lossy(),
    ]))
    .expect("sign");

    let val_out = run(&args(&[
        "validate",
        &pkg_path.to_string_lossy(),
        "--trust-key",
        &pubkey_path.to_string_lossy(),
    ]))
    .expect("validate with generated pubkey");
    assert!(val_out.contains(&parsed_secret.key_id_hex()));

    let pkg_bytes = fs::read(&pkg_path).expect("read pkg");
    let sig_text = fs::read_to_string(format!("{}.minisig", pkg_path.display())).expect("read sig");
    let verified = validate_cmplugin_bytes(
        &pkg_bytes,
        &PackagePolicy {
            dev_mode: false,
            detached_signature: Some(sig_text),
            trust_keys: vec![TrustedKey {
                key_id: parsed_secret.key_id_hex(),
                public_key: pubkey_text,
                revoked: false,
            }],
            ..PackagePolicy::default()
        },
    )
    .expect("server policy verifies package");
    assert!(matches!(
        verified.signature.status,
        SignatureStatus::Trusted { .. }
    ));
}

#[test]
fn keygen_help_and_error_cases() {
    assert!(run(&args(&["keygen", "--help"])).is_ok());

    let tmp = TempDir::new().expect("tmp");
    let same = tmp.path().join("same.key");
    assert!(matches!(
        run(&args(&[
            "keygen",
            "--key",
            &same.to_string_lossy(),
            "--pubkey",
            &same.to_string_lossy(),
        ])),
        Err(CliError::Validation(_))
    ));

    // Missing required flags -> Usage error
    assert!(matches!(
        run(&args(&["keygen", "--key", &same.to_string_lossy()])),
        Err(CliError::Usage(_))
    ));

    // Refuses to overwrite existing key file
    let key_path = tmp.path().join("existing.key");
    let pub_path = tmp.path().join("existing.pub");
    fs::write(&key_path, "already-here").expect("seed existing");
    assert!(
        run(&args(&[
            "keygen",
            "--key",
            &key_path.to_string_lossy(),
            "--pubkey",
            &pub_path.to_string_lossy(),
        ]))
        .is_err()
    );
    assert!(!pub_path.exists());
}
