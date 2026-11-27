#![cfg(unix)]

use plugin_cli::{CliError, run};
use plugin_package::PackageError;
use std::fs;
use std::os::unix::fs::symlink;
use tempfile::TempDir;

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

fn assert_unsafe_path(res: Result<String, CliError>, context: &str) {
    let err = res.expect_err(&format!("{context} must fail"));
    assert!(
        matches!(err, CliError::Package(PackageError::UnsafePath { .. })),
        "{context}: expected PackageError::UnsafePath, got: {err:?}"
    );
}

#[test]
fn scaffold_rejects_dotdot_symlink_escape_and_symlink_ancestors_without_overwriting() {
    let tmp = TempDir::new().expect("tmp");

    // Repro A: `<work>/nonexistent_sub/../sym_out` where `sym_out -> outside_victim`
    let outside_victim = tmp.path().join("outside_victim");
    fs::create_dir_all(&outside_victim).expect("mkdir outside_victim");
    fs::write(outside_victim.join("Cargo.toml"), "DO_NOT_OVERWRITE").expect("write victim");

    let work = tmp.path().join("work");
    fs::create_dir_all(&work).expect("mkdir work");
    let sym_out = work.join("sym_out");
    symlink(&outside_victim, &sym_out).expect("symlink sym_out");

    let escape_target = work.join("nonexistent_sub").join("..").join("sym_out");
    assert_unsafe_path(
        run(&args(&["scaffold", &escape_target.to_string_lossy()])),
        "scaffold work/nonexistent_sub/../sym_out",
    );
    assert_eq!(
        fs::read_to_string(outside_victim.join("Cargo.toml")).expect("read victim"),
        "DO_NOT_OVERWRITE",
        "outside_victim/Cargo.toml must not be overwritten"
    );
    assert!(
        !work.join("nonexistent_sub").exists(),
        "nonexistent_sub must not be created on rejected scaffold"
    );

    // Symlinked ancestor `sym_parent/new_proj`
    let real_parent = tmp.path().join("real_parent");
    fs::create_dir_all(&real_parent).expect("mkdir real_parent");
    let sym_parent = tmp.path().join("sym_parent");
    symlink(&real_parent, &sym_parent).expect("symlink sym_parent");
    assert_unsafe_path(
        run(&args(&[
            "scaffold",
            &sym_parent.join("new_proj").to_string_lossy(),
        ])),
        "scaffold sym_parent/new_proj",
    );
    assert!(!real_parent.join("new_proj").exists());
}

#[test]
fn scaffold_rejects_dotdot_nonempty_dir_and_file_and_normalizes_dir_slug() {
    let tmp = TempDir::new().expect("tmp");
    let work = tmp.path().join("work");
    fs::create_dir_all(&work).expect("mkdir work");

    // Repro B1: `<work>/nonexistent_sub/../existing_nonempty_dir`
    let nonempty = work.join("existing_nonempty_dir");
    fs::create_dir_all(&nonempty).expect("mkdir nonempty");
    fs::write(nonempty.join("sentinel.txt"), "KEEP").expect("write sentinel");

    let dotdot_nonempty = work
        .join("nonexistent_sub")
        .join("..")
        .join("existing_nonempty_dir");
    let err = run(&args(&["scaffold", &dotdot_nonempty.to_string_lossy()]))
        .expect_err("non-empty dir via .. must fail");
    assert!(
        err.to_string().contains("not empty"),
        "expected 'not empty' error, got: {err}"
    );
    assert!(!nonempty.join("plugin.toml").exists());
    assert!(!work.join("nonexistent_sub").exists());

    // Repro B2: `<work>/nonexistent_sub/../existing_file`
    let existing_file = work.join("existing_file");
    fs::write(&existing_file, "FILE").expect("write file");
    let dotdot_file = work
        .join("nonexistent_sub")
        .join("..")
        .join("existing_file");
    let file_err = run(&args(&["scaffold", &dotdot_file.to_string_lossy()]))
        .expect_err("file target via .. must fail");
    assert!(
        file_err.to_string().contains("not a directory"),
        "expected 'not a directory' error, got: {file_err}"
    );

    // Repro C: `dir_slug` derives from normalized path (`my_cool_plugin/child_dir/..` -> `org.example.my-cool-plugin`)
    let slug_parent = work.join("slug_parent");
    fs::create_dir_all(&slug_parent).expect("mkdir slug_parent");
    let unnormalized = slug_parent
        .join("my_cool_plugin")
        .join("child_dir")
        .join("..");
    let out = run(&args(&["scaffold", &unnormalized.to_string_lossy()]))
        .expect("scaffold normalized path");
    assert!(
        out.contains("org.example.my-cool-plugin"),
        "expected normalized slug id, got: {out}"
    );
    let manifest = fs::read_to_string(slug_parent.join("my_cool_plugin/plugin.toml"))
        .expect("read plugin.toml");
    assert!(manifest.contains("id = \"org.example.my-cool-plugin\""));
    assert!(!slug_parent.join("my_cool_plugin/child_dir").exists());
}
