mod common;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use common::{
    TestSigningKey, ZipEntrySpec, build_minimal_hook_zip, build_zip, sha256_bytes, sha256_hex,
};
use plugin_package::{
    PackageError, PackagePolicy, SignatureStatus, parse_minisign_public_key,
    validate_cmplugin_bytes, verify_minisign,
};

#[test]
fn sig_01_valid_signed_package_ed_and_hashed() {
    let key = TestSigningKey::generate(0x1122_3344_5566_7788);
    let parsed = parse_minisign_public_key(&key.public_key_str).expect("parse pubkey");
    assert_eq!(parsed.key_id, key.key_id_hex);
    assert_eq!(parsed.fingerprint, key.fingerprint);

    let zip = build_minimal_hook_zip();
    let digest = sha256_bytes(&zip);

    for sig in [
        key.sign_sha256_hashed(&digest, "trusted comment test"),
        key.sign_sha256_legacy(&digest, "legacy Ed test"),
    ] {
        let policy = PackagePolicy {
            detached_signature: Some(sig.clone()),
            trust_keys: vec![key.trusted_key(false)],
            ..Default::default()
        };

        let validated = validate_cmplugin_bytes(&zip, &policy).expect("valid signed package");
        assert_eq!(validated.package_sha256, digest);
        assert_eq!(validated.signature.package_sha256_hex, sha256_hex(&zip));
        assert_eq!(
            validated.signature.status,
            SignatureStatus::Trusted {
                key_id: key.key_id_hex.clone(),
                fingerprint: key.fingerprint.clone(),
            }
        );
        assert_eq!(validated.signature.status.as_store_str(), "trusted");
        assert_eq!(
            validated.signature.status.signing_key_id(),
            Some(key.key_id_hex.as_str())
        );

        let direct = verify_minisign(&digest, &sig, &[key.trusted_key(false)]).expect("direct ok");
        assert_eq!(direct.status, validated.signature.status);
    }
}

#[test]
fn sig_02_multiple_trust_keys_selects_active_matching_key() {
    let other = TestSigningKey::generate(0xAAAA_BBBB_CCCC_DDDD);
    let revoked_other = TestSigningKey::generate(0x1234_5678_9ABC_DEF0);
    let active = TestSigningKey::generate(0xFEED_FACE_CAFE_BEEF);

    let zip = build_minimal_hook_zip();
    let digest = sha256_bytes(&zip);
    let sig = active.sign_sha256_hashed(&digest, "timestamp:1700000000");

    let policy = PackagePolicy {
        detached_signature: Some(sig),
        trust_keys: vec![
            other.trusted_key(false),
            revoked_other.trusted_key(true),
            active.trusted_key(false),
        ],
        ..Default::default()
    };

    let validated = validate_cmplugin_bytes(&zip, policy).expect("should match active key");
    assert_eq!(
        validated.signature.status.signing_key_id(),
        Some(active.key_id_hex.as_str())
    );
}

#[test]
fn sig_03_tampered_package_rejected_even_in_dev_mode() {
    let key = TestSigningKey::generate(0x0102_0304_0506_0708);
    let mut zip = build_minimal_hook_zip();
    let digest = sha256_bytes(&zip);
    let sig = key.sign_sha256_hashed(&digest, "signed");

    // Flip 1 bit in the package
    let last = zip.len() - 1;
    zip[last] ^= 0x01;

    let policy = PackagePolicy {
        dev_mode: true,
        detached_signature: Some(sig),
        trust_keys: vec![key.trusted_key(false)],
        ..Default::default()
    };

    let err = validate_cmplugin_bytes(&zip, policy).unwrap_err();
    assert!(matches!(err, PackageError::InvalidSignature(_)));
}

#[test]
fn sig_04_tampered_trusted_comment_rejected() {
    let key = TestSigningKey::generate(0x0908_0706_0504_0302);
    let zip = build_minimal_hook_zip();
    let digest = sha256_bytes(&zip);
    let sig = key.sign_sha256_hashed(&digest, "original comment");
    let tampered_sig = sig.replace("original comment", "tampered comment");

    let err = verify_minisign(&digest, &tampered_sig, &[key.trusted_key(false)]).unwrap_err();
    assert!(matches!(err, PackageError::InvalidSignature(_)));
}

#[test]
fn sig_05_untrusted_key_and_bundled_pubkey_ignored() {
    let key_a = TestSigningKey::generate(0x1111_1111_1111_1111);
    let key_b = TestSigningKey::generate(0x2222_2222_2222_2222);

    let zip = build_zip(&[
        ZipEntrySpec::stored(
            "plugin.toml",
            common::minimal_hook_manifest_toml().as_bytes(),
        ),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            common::minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("components/policy.wasm", common::MINIMAL_WASM),
        ZipEntrySpec::stored("minisign.pub", key_b.public_key_str.as_bytes()),
    ]);
    let digest = sha256_bytes(&zip);
    let sig = key_b.sign_sha256_hashed(&digest, "signed by b");

    let policy = PackagePolicy {
        detached_signature: Some(sig),
        trust_keys: vec![key_a.trusted_key(false)],
        ..Default::default()
    };

    let err = validate_cmplugin_bytes(&zip, policy).unwrap_err();
    assert!(matches!(err, PackageError::UntrustedKey(_)));
}

#[test]
fn sig_06_revoked_key_rejected_even_in_dev_mode() {
    let key = TestSigningKey::generate(0x3333_3333_3333_3333);
    let zip = build_minimal_hook_zip();
    let digest = sha256_bytes(&zip);
    let sig = key.sign_sha256_hashed(&digest, "revoked");

    let policy = PackagePolicy {
        dev_mode: true,
        detached_signature: Some(sig),
        trust_keys: vec![key.trusted_key(true)],
        ..Default::default()
    };

    let err = validate_cmplugin_bytes(&zip, policy).unwrap_err();
    assert!(matches!(err, PackageError::KeyRevoked(_)));
}

#[test]
fn sig_07_malformed_minisig_rejected() {
    let key = TestSigningKey::generate(0x4444_4444_4444_4444);
    let digest = [7u8; 32];

    let bad_sigs = [
        "not a minisig",
        "untrusted comment: ok\nnot-base64!\ntrusted comment: ok\nAAAA\n",
        &format!(
            "wrong header: ok\n{}\ntrusted comment: ok\n{}\n",
            B64.encode([0u8; 74]),
            B64.encode([0u8; 64])
        ),
        &format!(
            "untrusted comment: ok\n{}\ntrusted comment: ok\n{}\n",
            B64.encode([0u8; 20]),
            B64.encode([0u8; 64])
        ),
    ];

    for bad in bad_sigs {
        let err = verify_minisign(&digest, bad, &[key.trusted_key(false)]).unwrap_err();
        assert!(
            matches!(err, PackageError::InvalidSignature(_)),
            "expected InvalidSignature, got {err:?}"
        );
    }
}

#[test]
fn sig_08_unsigned_normal_mode_rejected() {
    let zip = build_minimal_hook_zip();
    let policy = PackagePolicy::default();
    let err = validate_cmplugin_bytes(&zip, policy).unwrap_err();
    assert!(matches!(err, PackageError::SignatureRequired));
}

#[test]
fn sig_09_unsigned_dev_mode_accepted() {
    let zip = build_minimal_hook_zip();
    let policy = PackagePolicy {
        dev_mode: true,
        ..Default::default()
    };
    let validated = validate_cmplugin_bytes(&zip, policy).expect("dev mode unsigned ok");
    assert_eq!(validated.signature.status, SignatureStatus::UnsignedDevMode);
    assert_eq!(validated.signature.status.as_store_str(), "development");
    assert_eq!(validated.signature.status.signing_key_id(), None);
}

#[test]
fn sig_10_whitespace_or_empty_signature_treated_as_malformed() {
    let zip = build_minimal_hook_zip();
    for blank in ["", "   \n\t  "] {
        let policy = PackagePolicy {
            dev_mode: true,
            detached_signature: Some(blank.to_string()),
            ..Default::default()
        };
        let err = validate_cmplugin_bytes(&zip, policy).unwrap_err();
        assert!(matches!(err, PackageError::InvalidSignature(_)));
    }
}

#[test]
fn sig_11_revoked_key_rejected_regardless_of_order_and_pk_alias() {
    let key = TestSigningKey::generate(0x5555_6666_7777_8888);
    let zip = build_minimal_hook_zip();
    let digest = sha256_bytes(&zip);
    let sig = key.sign_sha256_hashed(&digest, "dual-entry test");

    // [active, revoked] and [revoked, active] must both return KeyRevoked, even in dev_mode
    for trust_keys in [
        vec![key.trusted_key(false), key.trusted_key(true)],
        vec![key.trusted_key(true), key.trusted_key(false)],
    ] {
        let policy = PackagePolicy {
            dev_mode: true,
            detached_signature: Some(sig.clone()),
            trust_keys,
            ..Default::default()
        };
        let err = validate_cmplugin_bytes(&zip, policy).unwrap_err();
        assert!(
            matches!(err, PackageError::KeyRevoked(_)),
            "expected KeyRevoked, got {err:?}"
        );
    }

    // Same ed25519_pk under a different key_id marked revoked must also reject
    let raw_pk = B64
        .decode(&key.public_key_str)
        .expect("decode generated key");
    let mut alias_raw = raw_pk;
    alias_raw[2..10].copy_from_slice(&0x9999_8888_7777_6666u64.to_le_bytes());
    let alias_revoked = plugin_package::TrustedKey {
        key_id: "9999888877776666".into(),
        public_key: B64.encode(&alias_raw),
        revoked: true,
    };
    for trust_keys in [
        vec![key.trusted_key(false), alias_revoked.clone()],
        vec![alias_revoked, key.trusted_key(false)],
    ] {
        let policy = PackagePolicy {
            dev_mode: true,
            detached_signature: Some(sig.clone()),
            trust_keys,
            ..Default::default()
        };
        let err = validate_cmplugin_bytes(&zip, policy).unwrap_err();
        assert!(
            matches!(err, PackageError::KeyRevoked(_)),
            "expected KeyRevoked for pk alias, got {err:?}"
        );
    }
}

#[test]
fn sig_12_multi_candidate_same_key_id_tries_all_non_revoked_keys() {
    let shared_seed = 0xDEAD_BEEF_CAFE_BABEu64;
    let key1 = TestSigningKey::generate(shared_seed);
    let key2 = TestSigningKey::generate(shared_seed);
    assert_eq!(key1.key_id_hex, key2.key_id_hex);
    assert_ne!(key1.fingerprint, key2.fingerprint);

    let zip = build_minimal_hook_zip();
    let digest = sha256_bytes(&zip);
    let sig_by_key2 = key2.sign_sha256_hashed(&digest, "signed by second key");

    let verified = verify_minisign(
        &digest,
        &sig_by_key2,
        &[key1.trusted_key(false), key2.trusted_key(false)],
    )
    .expect("should verify using second matching key_id candidate");
    assert_eq!(
        verified.status,
        SignatureStatus::Trusted {
            key_id: key2.key_id_hex.clone(),
            fingerprint: key2.fingerprint.clone(),
        }
    );

    let revoked_err = verify_minisign(
        &digest,
        &sig_by_key2,
        &[key1.trusted_key(true), key2.trusted_key(false)],
    )
    .unwrap_err();
    assert!(matches!(revoked_err, PackageError::KeyRevoked(_)));
}
