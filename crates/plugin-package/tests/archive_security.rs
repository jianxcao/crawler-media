mod common;

use common::{
    MINIMAL_WASM, ZipEntrySpec, build_minimal_hook_zip, build_zip, dev_policy,
    minimal_config_schema_json, minimal_hook_manifest_toml,
};
use plugin_package::{
    PackageError, SignatureStatus, validate_cmplugin_bytes, validate_cmplugin_reader,
};
use std::io::Cursor;

#[test]
fn arc_01_valid_bytes_and_reader_parity() {
    let zip = build_minimal_hook_zip();
    let res_bytes = validate_cmplugin_bytes(&zip, dev_policy()).expect("bytes ok");
    let res_reader = validate_cmplugin_reader(Cursor::new(&zip), dev_policy()).expect("reader ok");

    assert_eq!(res_bytes.package_sha256_hex.len(), 64);
    assert!(
        res_bytes
            .package_sha256_hex
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
    assert_eq!(res_bytes.package_sha256_hex, res_reader.package_sha256_hex);
    assert_eq!(res_bytes.signature.status, SignatureStatus::UnsignedDevMode);
    assert_eq!(res_bytes.signature, res_reader.signature);
    assert_eq!(
        res_bytes.component_bytes.get("components/policy.wasm"),
        Some(&MINIMAL_WASM.to_vec())
    );
    assert_eq!(res_bytes.component_bytes, res_reader.component_bytes);
}

#[test]
fn arc_02_empty_and_non_zip_rejected() {
    let err1 = validate_cmplugin_bytes(b"", dev_policy()).unwrap_err();
    assert!(matches!(err1, PackageError::InvalidArchive(_)));

    let err2 = validate_cmplugin_bytes(b"not a zip archive at all", dev_policy()).unwrap_err();
    assert!(matches!(err2, PackageError::InvalidArchive(_)));
}

#[test]
fn arc_03_spoofed_header_uncompressed_size_caught_by_stream() {
    // Low-compressibility payload (~128 KiB) so it tests entry size cap rather than ratio bomb
    let big_payload: Vec<u8> = (0..128 * 1024u32)
        .map(|i| (i.wrapping_mul(1_664_525).wrapping_add(1_013_904_223) >> 24) as u8)
        .collect();
    let mut entry = ZipEntrySpec::deflated("README.md", &big_payload);
    entry.claimed_uncompressed_size = 100; // spoofed small header size

    let zip = build_zip(&[
        ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
        entry,
    ]);

    let mut policy = dev_policy();
    policy.limits.max_entry_uncompressed_bytes = 64 * 1024;
    let err = validate_cmplugin_bytes(&zip, policy).unwrap_err();
    assert!(matches!(err, PackageError::LimitExceeded(_)));
}

#[test]
fn arc_04_compression_ratio_bomb_rejected() {
    // 256 KiB of zeros compresses to ~300 bytes (> 500x ratio)
    let zeros = vec![0u8; 256 * 1024];
    let zip = build_zip(&[
        ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
        ZipEntrySpec::deflated("README.md", &zeros),
    ]);

    let mut policy = dev_policy();
    policy.limits.max_compression_ratio = 100;
    let err = validate_cmplugin_bytes(&zip, policy).unwrap_err();
    assert!(matches!(err, PackageError::CompressionRatioExceeded(_)));
}

#[test]
fn arc_05_entry_count_cap_enforced() {
    let mut entries = vec![
        ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
    ];
    for i in 0..10 {
        entries.push(ZipEntrySpec::stored(&format!("docs/f{i}.txt"), b"hi"));
    }
    let zip = build_zip(&entries);
    let mut policy = dev_policy();
    policy.limits.max_entries = 8;
    let err = validate_cmplugin_bytes(&zip, policy).unwrap_err();
    assert!(matches!(err, PackageError::LimitExceeded(_)));
}

#[test]
fn arc_06_total_uncompressed_cap_enforced() {
    let chunk = vec![0x42u8; 20 * 1024];
    let zip = build_zip(&[
        ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
        ZipEntrySpec::stored("docs/a.txt", &chunk),
        ZipEntrySpec::stored("docs/b.txt", &chunk),
    ]);
    let mut policy = dev_policy();
    policy.limits.max_total_uncompressed_bytes = 32 * 1024;
    let err = validate_cmplugin_bytes(&zip, policy).unwrap_err();
    assert!(matches!(err, PackageError::LimitExceeded(_)));
}

#[test]
fn arc_07_compressed_package_byte_cap_enforced() {
    let zip = build_minimal_hook_zip();
    let mut policy = dev_policy();
    policy.limits.max_package_bytes = 128;
    let err = validate_cmplugin_reader(Cursor::new(&zip), &policy).unwrap_err();
    assert!(matches!(err, PackageError::LimitExceeded(_)));
    let err2 = validate_cmplugin_bytes(&zip, &policy).unwrap_err();
    assert!(matches!(err2, PackageError::LimitExceeded(_)));
}

#[test]
fn arc_08_unsafe_paths_rejected() {
    let bad_paths = [
        "../evil.txt",
        "a/../../evil.txt",
        "/etc/passwd",
        "C:\\evil.txt",
        "ui\\assets\\a.js",
        "a\0b.txt",
        "./README.md",
        "",
    ];
    for bad in bad_paths {
        let zip = build_zip(&[
            ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
            ZipEntrySpec::stored(
                "schemas/config.schema.json",
                minimal_config_schema_json().as_bytes(),
            ),
            ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
            ZipEntrySpec::stored(bad, b"x"),
        ]);
        let err = validate_cmplugin_bytes(&zip, dev_policy()).unwrap_err();
        assert!(
            matches!(err, PackageError::UnsafePath { .. }),
            "expected UnsafePath for `{bad:?}`, got {err:?}"
        );
    }
}

#[test]
fn arc_09_symlinks_and_special_entry_types_rejected() {
    let modes = [0o120777u32, 0o010644, 0o020600, 0o060600, 0o140755];
    for mode in modes {
        let mut entry = ZipEntrySpec::stored("README.md", b"target");
        entry.external_attr = mode << 16;
        let zip = build_zip(&[
            ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
            ZipEntrySpec::stored(
                "schemas/config.schema.json",
                minimal_config_schema_json().as_bytes(),
            ),
            ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
            entry,
        ]);
        let err = validate_cmplugin_bytes(&zip, dev_policy()).unwrap_err();
        assert!(
            matches!(err, PackageError::ForbiddenEntryType { .. }),
            "expected ForbiddenEntryType for mode {mode:#o}, got {err:?}"
        );
    }
}

#[test]
fn arc_10_duplicate_paths_rejected() {
    let zip = build_zip(&[
        ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
        ZipEntrySpec::stored("README.md", b"one"),
        ZipEntrySpec::stored("README.md", b"two"),
    ]);
    let err = validate_cmplugin_bytes(&zip, dev_policy()).unwrap_err();
    assert!(matches!(err, PackageError::DuplicatePath(_)));
}

#[test]
fn arc_11_case_colliding_paths_rejected() {
    let zip = build_zip(&[
        ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
        ZipEntrySpec::stored("docs/Readme.md", b"one"),
        ZipEntrySpec::stored("docs/readme.md", b"two"),
    ]);
    let err = validate_cmplugin_bytes(&zip, dev_policy()).unwrap_err();
    assert!(matches!(err, PackageError::PathCollision { .. }));
}

#[test]
fn arc_12_unicode_nfc_nfd_collision_rejected() {
    let zip = build_zip(&[
        ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
        ZipEntrySpec::stored("docs/cafe\u{0301}.txt", b"nfd"),
        ZipEntrySpec::stored("docs/caf\u{00e9}.txt", b"nfc"),
    ]);
    let err = validate_cmplugin_bytes(&zip, dev_policy()).unwrap_err();
    assert!(matches!(err, PackageError::PathCollision { .. }));
}

#[test]
fn arc_13_encrypted_entries_rejected() {
    for (flags, extra) in [
        (0x0001u16, vec![]),
        (0x0040u16, vec![]),
        (
            0x0000u16,
            vec![0x01, 0x99, 0x07, 0x00, 1, 0, b'A', b'E', 3, 0, 0],
        ),
    ] {
        let mut entry = ZipEntrySpec::stored("README.md", b"secret");
        entry.flags = flags;
        entry.extra = extra;
        let zip = build_zip(&[
            ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
            ZipEntrySpec::stored(
                "schemas/config.schema.json",
                minimal_config_schema_json().as_bytes(),
            ),
            ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
            entry,
        ]);
        let err = validate_cmplugin_bytes(&zip, dev_policy()).unwrap_err();
        assert!(matches!(err, PackageError::EncryptedArchiveEntry(_)));
    }
}

#[test]
fn arc_14_unsupported_compression_method_rejected() {
    let mut entry = ZipEntrySpec::stored("README.md", b"zstd");
    entry.method = 93;
    let zip = build_zip(&[
        ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
        entry,
    ]);
    let err = validate_cmplugin_bytes(&zip, dev_policy()).unwrap_err();
    assert!(matches!(
        err,
        PackageError::UnsupportedCompression { method: 93, .. }
    ));
}

#[test]
fn arc_15_forbidden_executable_extensions_and_exec_bits_rejected() {
    for ext in [
        "run.sh",
        "run.bat",
        "run.cmd",
        "bin.exe",
        "lib.dll",
        "lib.so",
        "lib.dylib",
        "run.ps1",
        "run.py",
        "run.rb",
        "run.pl",
    ] {
        let zip = build_zip(&[
            ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
            ZipEntrySpec::stored(
                "schemas/config.schema.json",
                minimal_config_schema_json().as_bytes(),
            ),
            ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
            ZipEntrySpec::stored(ext, b"echo hi"),
        ]);
        let err = validate_cmplugin_bytes(&zip, dev_policy()).unwrap_err();
        assert!(
            matches!(err, PackageError::ForbiddenExecutable(_)),
            "expected ForbiddenExecutable for {ext}, got {err:?}"
        );
    }

    let mut exec_readme = ZipEntrySpec::stored("README.md", b"hello");
    exec_readme.external_attr = 0o100755 << 16;
    let zip = build_zip(&[
        ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
        exec_readme,
    ]);
    let err = validate_cmplugin_bytes(&zip, dev_policy()).unwrap_err();
    assert!(matches!(err, PackageError::ForbiddenExecutable(_)));
}

#[test]
fn arc_16_directory_file_conflict_rejected() {
    let zip = build_zip(&[
        ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
        ZipEntrySpec::stored("docs", b"file"),
        ZipEntrySpec::stored("docs/readme.txt", b"nested"),
    ]);
    let err = validate_cmplugin_bytes(&zip, dev_policy()).unwrap_err();
    assert!(matches!(err, PackageError::InvalidArchive(_)));
}

#[test]
fn arc_17_undeclared_wasm_or_js_rejected() {
    let zip = build_zip(&[
        ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
        ZipEntrySpec::stored("components/extra.wasm", MINIMAL_WASM),
    ]);
    let err = validate_cmplugin_bytes(&zip, dev_policy()).unwrap_err();
    assert!(matches!(err, PackageError::ForbiddenExecutable(_)));

    let zip_js = build_zip(&[
        ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
        ZipEntrySpec::stored("scripts/extra.mjs", b"export default 1;"),
    ]);
    let err_js = validate_cmplugin_bytes(&zip_js, dev_policy()).unwrap_err();
    assert!(matches!(err_js, PackageError::ForbiddenExecutable(_)));
}

#[test]
fn arc_18_missing_plugin_toml_rejected() {
    let zip = build_zip(&[ZipEntrySpec::stored("README.md", b"no manifest")]);
    let err = validate_cmplugin_bytes(&zip, dev_policy()).unwrap_err();
    assert!(matches!(err, PackageError::MissingManifest));
}

#[test]
fn arc_19_case_and_unicode_file_vs_implied_dir_collision_rejected() {
    let zip_case = build_zip(&[
        ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
        ZipEntrySpec::stored("Docs", b"file"),
        ZipEntrySpec::stored("docs/readme.txt", b"nested"),
    ]);
    assert!(matches!(
        validate_cmplugin_bytes(&zip_case, dev_policy()),
        Err(PackageError::InvalidArchive(_) | PackageError::PathCollision { .. })
    ));

    let zip_unicode = build_zip(&[
        ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
        ZipEntrySpec::stored("caf\u{00e9}", b"file"),
        ZipEntrySpec::stored("cafe\u{0301}/readme.txt", b"nested"),
    ]);
    assert!(matches!(
        validate_cmplugin_bytes(&zip_unicode, dev_policy()),
        Err(PackageError::InvalidArchive(_) | PackageError::PathCollision { .. })
    ));
}

#[test]
fn arc_20_deflate_trailing_padding_and_aggregate_ratio_bomb_rejected() {
    let zeros = vec![0u8; 32 * 1024];
    let mut padded = ZipEntrySpec::deflated("README.md", &zeros);
    padded.compressed_data.extend(vec![0u8; 4096]);
    let zip_padded = build_zip(&[
        ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
        padded,
    ]);
    assert!(matches!(
        validate_cmplugin_bytes(&zip_padded, dev_policy()),
        Err(PackageError::InvalidArchive(_) | PackageError::CompressionRatioExceeded(_))
    ));

    // Many small entries each < ratio_check_min_bytes (4096), aggregate > max_compression_ratio
    let small_zeros = vec![0u8; 3000];
    let mut entries = vec![
        ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
    ];
    for i in 0..25 {
        entries.push(ZipEntrySpec::deflated(
            &format!("docs/z{i}.txt"),
            &small_zeros,
        ));
    }
    let zip_agg = build_zip(&entries);
    let mut policy = dev_policy();
    policy.limits.max_compression_ratio = 50;
    assert!(matches!(
        validate_cmplugin_bytes(&zip_agg, policy),
        Err(PackageError::CompressionRatioExceeded(_))
    ));
}

#[test]
fn arc_21_polyglot_lfh_mismatch_special_mode_dir_attr_and_wasm_v1() {
    // 1. Polyglot ZIP with prepended bytes and adjusted offsets
    let raw_zip = build_minimal_hook_zip();
    let eocd_pos = raw_zip.len() - 22;
    let cd_size = u32::from_le_bytes(raw_zip[eocd_pos + 12..eocd_pos + 16].try_into().unwrap());
    let cd_offset =
        u32::from_le_bytes(raw_zip[eocd_pos + 16..eocd_pos + 20].try_into().unwrap()) as usize;
    let prefix = b"#!/bin/sh\necho pwned\n";
    let shift = prefix.len() as u32;
    let mut poly = Vec::new();
    poly.extend_from_slice(prefix);
    poly.extend_from_slice(&raw_zip[..cd_offset]);
    let mut cd = raw_zip[cd_offset..eocd_pos].to_vec();
    let mut p = 0;
    while p < cd.len() {
        let nl = u16::from_le_bytes([cd[p + 28], cd[p + 29]]) as usize;
        let el = u16::from_le_bytes([cd[p + 30], cd[p + 31]]) as usize;
        let cl = u16::from_le_bytes([cd[p + 32], cd[p + 33]]) as usize;
        let old_lfh = u32::from_le_bytes(cd[p + 42..p + 46].try_into().unwrap());
        cd[p + 42..p + 46].copy_from_slice(&(old_lfh + shift).to_le_bytes());
        p += 46 + nl + el + cl;
    }
    poly.extend_from_slice(&cd);
    let mut eocd = raw_zip[eocd_pos..].to_vec();
    eocd[12..16].copy_from_slice(&cd_size.to_le_bytes());
    eocd[16..20].copy_from_slice(&(cd_offset as u32 + shift).to_le_bytes());
    poly.extend_from_slice(&eocd);
    assert!(matches!(
        validate_cmplugin_bytes(&poly, dev_policy()),
        Err(PackageError::InvalidArchive(_))
    ));

    // 2. LFH vs CD crc32 mismatch
    let mut lfh_mismatch = build_minimal_hook_zip();
    lfh_mismatch[14] ^= 0xFF;
    assert!(matches!(
        validate_cmplugin_bytes(&lfh_mismatch, dev_policy()),
        Err(PackageError::InvalidArchive(_))
    ));

    // 3. Setuid/setgid/sticky mode bits on non-dir entry
    for mode in [0o104644u32, 0o102644, 0o101644] {
        let mut entry = ZipEntrySpec::stored("README.md", b"hi");
        entry.external_attr = mode << 16;
        let zip = build_zip(&[
            ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
            ZipEntrySpec::stored(
                "schemas/config.schema.json",
                minimal_config_schema_json().as_bytes(),
            ),
            ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
            entry,
        ]);
        assert!(matches!(
            validate_cmplugin_bytes(&zip, dev_policy()),
            Err(PackageError::ForbiddenExecutable(_))
        ));
    }

    // 4. Directory entry ending in `/` with regular file mode / DOS file attr
    let mut fake_dir = ZipEntrySpec::dir("docs/");
    fake_dir.external_attr = (0o100644 << 16) | 0x20;
    let zip_fake_dir = build_zip(&[
        ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
        fake_dir,
    ]);
    assert!(matches!(
        validate_cmplugin_bytes(&zip_fake_dir, dev_policy()),
        Err(PackageError::ForbiddenEntryType { .. })
    ));

    // 5. WASM must be a valid WebAssembly Component Model binary (\0asm\x0d\x00\x01\x00);
    // Core WASM v1 (\0asm\x01\0\0\0), truncated headers, wrong layer, WAT text, and corrupt sections must fail.
    for bad_wasm in [
        b"\0asm".as_slice(),
        b"\0asm\x01\0\0\0".as_slice(),
        b"\0asm\x02\0\0\0".as_slice(),
        b"\0asm\x0d\x00\x00\x00".as_slice(),
        b"(component)".as_slice(),
        b"\0asm\x0d\x00\x01\x00\xff\xff".as_slice(),
    ] {
        let zip_bad_wasm = build_zip(&[
            ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
            ZipEntrySpec::stored(
                "schemas/config.schema.json",
                minimal_config_schema_json().as_bytes(),
            ),
            ZipEntrySpec::stored("components/policy.wasm", bad_wasm),
        ]);
        assert!(matches!(
            validate_cmplugin_bytes(&zip_bad_wasm, dev_policy()),
            Err(PackageError::InvalidManifest(_))
        ));
    }
}

#[test]
fn arc_22_undeclared_cjs_and_case_variant_ui_dir_rejected() {
    let zip_cjs = build_zip(&[
        ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
        ZipEntrySpec::stored(
            "schemas/config.schema.json",
            minimal_config_schema_json().as_bytes(),
        ),
        ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
        ZipEntrySpec::stored("evil.cjs", b"module.exports = 1;"),
    ]);
    assert!(matches!(
        validate_cmplugin_bytes(&zip_cjs, dev_policy()),
        Err(PackageError::ForbiddenExecutable(_))
    ));

    for ui_path in ["UI/evil.html", "UI/extra.txt"] {
        let zip_ui = build_zip(&[
            ZipEntrySpec::stored("plugin.toml", minimal_hook_manifest_toml().as_bytes()),
            ZipEntrySpec::stored(
                "schemas/config.schema.json",
                minimal_config_schema_json().as_bytes(),
            ),
            ZipEntrySpec::stored("components/policy.wasm", MINIMAL_WASM),
            ZipEntrySpec::stored(ui_path, b"evil"),
        ]);
        assert!(matches!(
            validate_cmplugin_bytes(&zip_ui, dev_policy()),
            Err(PackageError::ForbiddenUiAsset(_) | PackageError::InvalidUiManifest(_))
        ));
    }
}

#[test]
fn arc_23_reader_interrupted_chunked_and_exact_max_package_bytes() {
    struct ChunkedInterruptedReader<'a> {
        data: &'a [u8],
        pos: usize,
        step: usize,
    }
    impl<'a> std::io::Read for ChunkedInterruptedReader<'a> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.step += 1;
            if self.step % 3 == 1 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "transient",
                ));
            }
            if self.pos >= self.data.len() || buf.is_empty() {
                return Ok(0);
            }
            buf[0] = self.data[self.pos];
            self.pos += 1;
            Ok(1)
        }
    }

    let zip = build_minimal_hook_zip();
    let mut exact_policy = dev_policy();
    exact_policy.limits.max_package_bytes = zip.len() as u64;
    let ok = validate_cmplugin_reader(
        ChunkedInterruptedReader {
            data: &zip,
            pos: 0,
            step: 0,
        },
        &exact_policy,
    )
    .expect("exact max_package_bytes with Interrupted + 1-byte reads must pass");
    assert_eq!(ok.manifest.id, "org.example.hook-demo");

    let mut under_policy = dev_policy();
    under_policy.limits.max_package_bytes = (zip.len() - 1) as u64;
    let err = validate_cmplugin_reader(
        ChunkedInterruptedReader {
            data: &zip,
            pos: 0,
            step: 0,
        },
        &under_policy,
    )
    .unwrap_err();
    assert!(matches!(err, PackageError::LimitExceeded(_)));
}

#[test]
fn arc_24_cd_to_eocd_gap_rejected() {
    let raw_zip = build_minimal_hook_zip();
    let eocd_pos = raw_zip.len() - 22;
    for smuggled in [
        b"\0".as_slice(),
        b"SMUGGLED_PAYLOAD_BETWEEN_CD_AND_EOCD".as_slice(),
    ] {
        let mut gapped = Vec::with_capacity(raw_zip.len() + smuggled.len());
        gapped.extend_from_slice(&raw_zip[..eocd_pos]);
        gapped.extend_from_slice(smuggled);
        gapped.extend_from_slice(&raw_zip[eocd_pos..]);
        let err = validate_cmplugin_bytes(&gapped, dev_policy()).unwrap_err();
        assert!(
            matches!(err, PackageError::InvalidArchive(_)),
            "expected InvalidArchive for CD-to-EOCD gap of {} bytes, got {err:?}",
            smuggled.len()
        );
    }
}
