#![cfg(unix)]

use plugin_cli::{CliError, run};
use plugin_package::{PackageError, parse_signing_key};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use tempfile::TempDir;

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

const MIN_SCHEMA: &str = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#;
const VALID_SITE_YAML: &str = "id: custom-r7-tracker\nframework: nexusphp\nsearch:\n  path: /torrents.php\n  query_param: search\n";

fn write_valid_site_project(dir: &Path) {
    fs::create_dir_all(dir.join("schemas")).expect("mkdir schemas");
    fs::create_dir_all(dir.join("sites")).expect("mkdir sites");
    fs::write(
        dir.join("plugin.toml"),
        r#"manifest_version = 1
id = "org.example.r7-sym"
name = "R7 Symlink Check"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Tester"]
description = "Round 7 CLI symlink hardening test"
config_schema = "schemas/config.schema.json"
state_schema = 1
sites = ["sites/custom.yaml"]
"#,
    )
    .expect("write plugin.toml");
    fs::write(dir.join("schemas/config.schema.json"), MIN_SCHEMA).expect("write schema");
    fs::write(dir.join("sites/custom.yaml"), VALID_SITE_YAML).expect("write site yaml");
}

fn assert_unsafe_path(res: Result<String, CliError>, context: &str) {
    let err = res.expect_err(&format!("{context} must fail"));
    assert!(
        matches!(err, CliError::Package(PackageError::UnsafePath { .. })),
        "{context}: expected PackageError::UnsafePath, got: {err:?}"
    );
}

#[test]
fn validate_and_project_commands_reject_symlink_ancestors_dotdot_and_dangling_symlinks() {
    let tmp = TempDir::new().expect("tmp");

    // 1. Real project inside `outside_root`, with a real subdirectory `real_subdir`
    let outside_root = tmp.path().join("outside_root");
    write_valid_site_project(&outside_root);
    fs::create_dir_all(outside_root.join("real_subdir")).expect("mkdir real_subdir");

    let sym_root = tmp.path().join("sym_root");
    symlink(&outside_root, &sym_root).expect("symlink sym_root");
    let dotdot_escape = sym_root.join("real_subdir").join("..");

    // `sym_root/real_subdir/..` must fail with UnsafePath across validate, pack, build, test
    assert_unsafe_path(
        run(&args(&["validate", &dotdot_escape.to_string_lossy()])),
        "validate sym_root/real_subdir/..",
    );
    let unused_out = tmp.path().join("unused.cmplugin");
    assert_unsafe_path(
        run(&args(&[
            "pack",
            &dotdot_escape.to_string_lossy(),
            "--output",
            &unused_out.to_string_lossy(),
        ])),
        "pack sym_root/real_subdir/..",
    );
    assert_unsafe_path(
        run(&args(&["build", &dotdot_escape.to_string_lossy()])),
        "build sym_root/real_subdir/..",
    );
    assert_unsafe_path(
        run(&args(&[
            "test",
            &dotdot_escape.to_string_lossy(),
            "--offline",
        ])),
        "test sym_root/real_subdir/..",
    );

    // 2. `sym_parent/proj` where `sym_parent` is a symlink to an ancestor directory
    let outside_parent = tmp.path().join("outside_parent");
    let nested_proj = outside_parent.join("proj");
    write_valid_site_project(&nested_proj);
    let sym_parent = tmp.path().join("sym_parent");
    symlink(&outside_parent, &sym_parent).expect("symlink sym_parent");
    let via_sym_parent = sym_parent.join("proj");

    for subcmd in ["validate", "build"] {
        assert_unsafe_path(
            run(&args(&[subcmd, &via_sym_parent.to_string_lossy()])),
            &format!("{subcmd} sym_parent/proj"),
        );
    }
    assert_unsafe_path(
        run(&args(&[
            "pack",
            &via_sym_parent.to_string_lossy(),
            "--output",
            &unused_out.to_string_lossy(),
        ])),
        "pack sym_parent/proj",
    );
    assert_unsafe_path(
        run(&args(&[
            "test",
            &via_sym_parent.to_string_lossy(),
            "--offline",
        ])),
        "test sym_parent/proj",
    );

    // 3. Dangling directory symlink must fail with UnsafePath (not "does not exist")
    let dangling_dir = tmp.path().join("dangling_dir_sym");
    symlink(tmp.path().join("missing_target_dir"), &dangling_dir).expect("dangling symlink");
    assert_unsafe_path(
        run(&args(&["validate", &dangling_dir.to_string_lossy()])),
        "validate dangling directory symlink",
    );

    // 4. Genuine missing non-symlink path still reports "does not exist"
    let missing_real = tmp.path().join("truly_missing_path");
    let missing_err = run(&args(&["validate", &missing_real.to_string_lossy()]))
        .expect_err("missing path must fail");
    assert!(
        missing_err.to_string().contains("does not exist"),
        "expected 'does not exist', got: {missing_err}"
    );
}

#[test]
fn validate_rejects_symlinked_cmplugin_and_symlinked_minisig() {
    let tmp = TempDir::new().expect("tmp");
    let proj = tmp.path().join("real_proj");
    write_valid_site_project(&proj);

    let real_pkg = tmp.path().join("real.cmplugin");
    run(&args(&[
        "pack",
        &proj.to_string_lossy(),
        "--output",
        &real_pkg.to_string_lossy(),
    ]))
    .expect("pack real.cmplugin");

    // 1. Symlinked `.cmplugin` file & archive under symlinked parent directory
    let sym_pkg = tmp.path().join("sym.cmplugin");
    symlink(&real_pkg, &sym_pkg).expect("symlink .cmplugin");
    assert_unsafe_path(
        run(&args(&["validate", &sym_pkg.to_string_lossy()])),
        "validate symlinked .cmplugin",
    );

    let outside_dir = tmp.path().join("pkg_holder");
    fs::create_dir_all(&outside_dir).expect("mkdir pkg_holder");
    fs::copy(&real_pkg, outside_dir.join("copy.cmplugin")).expect("copy pkg");
    let sym_holder = tmp.path().join("sym_holder");
    symlink(&outside_dir, &sym_holder).expect("symlink holder");
    assert_unsafe_path(
        run(&args(&[
            "validate",
            &sym_holder.join("copy.cmplugin").to_string_lossy(),
        ])),
        "validate sym_holder/copy.cmplugin",
    );

    // 2. Sign real_pkg, move .minisig outside, and replace `.cmplugin.minisig` with a symlink
    let key_hex: String = [0x42u8; 32].iter().map(|b| format!("{b:02x}")).collect();
    let key_file = tmp.path().join("sign.key");
    fs::write(&key_file, &key_hex).expect("write key");
    run(&args(&[
        "sign",
        &real_pkg.to_string_lossy(),
        "--key",
        &key_file.to_string_lossy(),
    ]))
    .expect("sign real.cmplugin");

    let sig_path = tmp.path().join("real.cmplugin.minisig");
    let ext_sig = tmp.path().join("external.minisig");
    fs::rename(&sig_path, &ext_sig).expect("move minisig");
    symlink(&ext_sig, &sig_path).expect("symlink .cmplugin.minisig");

    let pubkey = parse_signing_key(&key_hex)
        .expect("parse key")
        .public_key_text();
    assert_unsafe_path(
        run(&args(&["validate", &real_pkg.to_string_lossy()])),
        "validate with symlinked .cmplugin.minisig (offline)",
    );
    assert_unsafe_path(
        run(&args(&[
            "validate",
            &real_pkg.to_string_lossy(),
            "--trust-key",
            &pubkey,
        ])),
        "validate with symlinked .cmplugin.minisig (--trust-key)",
    );

    // Dangling `.cmplugin.minisig` symlink must also be rejected with UnsafePath
    fs::remove_file(&sig_path).expect("rm sig symlink");
    symlink(tmp.path().join("missing.minisig"), &sig_path).expect("dangling minisig symlink");
    assert_unsafe_path(
        run(&args(&["validate", &real_pkg.to_string_lossy()])),
        "validate with dangling .cmplugin.minisig symlink",
    );
}

#[test]
fn pack_rejects_symlinked_output_file_and_symlinked_output_parent_components() {
    let tmp = TempDir::new().expect("tmp");
    let proj = tmp.path().join("pack_proj");
    write_valid_site_project(&proj);

    // 1. Output path is a symlink to an existing victim file
    let victim = tmp.path().join("victim.txt");
    fs::write(&victim, b"keep-safe").expect("write victim");
    let sym_out = tmp.path().join("sym_out.cmplugin");
    symlink(&victim, &sym_out).expect("symlink output");

    assert_unsafe_path(
        run(&args(&[
            "pack",
            &proj.to_string_lossy(),
            "--output",
            &sym_out.to_string_lossy(),
        ])),
        "pack into symlinked output file",
    );
    assert_eq!(fs::read(&victim).expect("read victim"), b"keep-safe");

    // 2. Output path is a dangling symlink to a not-yet-created file
    let uncreated_victim = tmp.path().join("uncreated_victim.txt");
    let dangling_out = tmp.path().join("dangling_out.cmplugin");
    symlink(&uncreated_victim, &dangling_out).expect("dangling output symlink");
    assert_unsafe_path(
        run(&args(&[
            "pack",
            &proj.to_string_lossy(),
            "--output",
            &dangling_out.to_string_lossy(),
        ])),
        "pack into dangling output symlink",
    );
    assert!(!uncreated_victim.exists());

    // 3. Output path traverses a symlinked parent directory (`sym_out_dir/nested/out.cmplugin`)
    let ext_out_dir = tmp.path().join("ext_out_dir");
    fs::create_dir_all(&ext_out_dir).expect("mkdir ext_out_dir");
    let sym_out_dir = tmp.path().join("sym_out_dir");
    symlink(&ext_out_dir, &sym_out_dir).expect("symlink sym_out_dir");
    let nested_via_sym = sym_out_dir.join("nested/out.cmplugin");

    assert_unsafe_path(
        run(&args(&[
            "pack",
            &proj.to_string_lossy(),
            "--output",
            &nested_via_sym.to_string_lossy(),
        ])),
        "pack via symlinked output parent dir",
    );
    assert!(!ext_out_dir.join("nested").exists());

    // 4. `nonexistent_sub/../sym_out.cmplugin` traversal must still catch `sym_out.cmplugin`
    let tricky_out = tmp.path().join("nonexistent_sub/../sym_out.cmplugin");
    assert_unsafe_path(
        run(&args(&[
            "pack",
            &proj.to_string_lossy(),
            "--output",
            &tricky_out.to_string_lossy(),
        ])),
        "pack via nonexistent_sub/../sym_out.cmplugin",
    );
    assert_eq!(fs::read(&victim).expect("read victim"), b"keep-safe");

    // 5. Positive control: non-existent clean nested output path still succeeds
    let clean_nested = tmp.path().join("clean_dist/sub/good.cmplugin");
    run(&args(&[
        "pack",
        &proj.to_string_lossy(),
        "--output",
        &clean_nested.to_string_lossy(),
    ]))
    .expect("pack into clean nested output path");
    assert!(clean_nested.is_file());
}

#[test]
fn sign_rejects_symlinks_on_package_key_and_minisig_output() {
    let tmp = TempDir::new().expect("tmp");
    let proj = tmp.path().join("sign_proj");
    write_valid_site_project(&proj);

    let real_pkg = tmp.path().join("pkg.cmplugin");
    run(&args(&[
        "pack",
        &proj.to_string_lossy(),
        "--output",
        &real_pkg.to_string_lossy(),
    ]))
    .expect("pack pkg.cmplugin");

    let key_hex: String = [0x55u8; 32].iter().map(|b| format!("{b:02x}")).collect();
    let real_key = tmp.path().join("real.key");
    fs::write(&real_key, &key_hex).expect("write key");

    // 1. Symlinked package file
    let sym_pkg = tmp.path().join("sym_pkg.cmplugin");
    symlink(&real_pkg, &sym_pkg).expect("symlink package");
    assert_unsafe_path(
        run(&args(&[
            "sign",
            &sym_pkg.to_string_lossy(),
            "--key",
            &real_key.to_string_lossy(),
        ])),
        "sign symlinked package",
    );
    assert!(!tmp.path().join("sym_pkg.cmplugin.minisig").exists());

    // 2. Symlinked key file (and dangling key symlink)
    let sym_key = tmp.path().join("sym.key");
    symlink(&real_key, &sym_key).expect("symlink key");
    assert_unsafe_path(
        run(&args(&[
            "sign",
            &real_pkg.to_string_lossy(),
            "--key",
            &sym_key.to_string_lossy(),
        ])),
        "sign with symlinked key",
    );
    let dangling_key = tmp.path().join("dangling.key");
    symlink(tmp.path().join("no_such.key"), &dangling_key).expect("dangling key");
    assert_unsafe_path(
        run(&args(&[
            "sign",
            &real_pkg.to_string_lossy(),
            "--key",
            &dangling_key.to_string_lossy(),
        ])),
        "sign with dangling key symlink",
    );
    assert!(!tmp.path().join("pkg.cmplugin.minisig").exists());

    // 3. Pre-existing symlink at `pkg.cmplugin.minisig` (both existing target & dangling target)
    let victim_sig = tmp.path().join("victim_sig.txt");
    fs::write(&victim_sig, b"original-victim").expect("write victim_sig");
    let sig_out = tmp.path().join("pkg.cmplugin.minisig");
    symlink(&victim_sig, &sig_out).expect("symlink minisig output");

    assert_unsafe_path(
        run(&args(&[
            "sign",
            &real_pkg.to_string_lossy(),
            "--key",
            &real_key.to_string_lossy(),
        ])),
        "sign when .minisig is a symlink to existing file",
    );
    assert_eq!(
        fs::read(&victim_sig).expect("read victim_sig"),
        b"original-victim"
    );

    fs::remove_file(&sig_out).expect("rm sig_out symlink");
    let uncreated_sig_victim = tmp.path().join("uncreated_sig_victim.txt");
    symlink(&uncreated_sig_victim, &sig_out).expect("dangling minisig output symlink");
    assert_unsafe_path(
        run(&args(&[
            "sign",
            &real_pkg.to_string_lossy(),
            "--key",
            &real_key.to_string_lossy(),
        ])),
        "sign when .minisig is a dangling symlink",
    );
    assert!(!uncreated_sig_victim.exists());

    // 4. Positive control: removing symlink allows clean signing
    fs::remove_file(&sig_out).expect("rm dangling sig_out");
    run(&args(&[
        "sign",
        &real_pkg.to_string_lossy(),
        "--key",
        &real_key.to_string_lossy(),
    ]))
    .expect("clean sign succeeds");
    assert!(sig_out.is_file());
}
