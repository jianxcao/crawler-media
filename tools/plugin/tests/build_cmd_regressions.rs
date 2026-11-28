//! TDD regressions for `plugin build`:
//! - Item 9: pass `--locked` (not forced `--offline`) when `Cargo.lock` exists so fresh
//!   Cargo caches can fetch locked dependencies, while `CARGO_NET_OFFLINE=true` still enforces
//!   offline builds and `Cargo.lock` drift fails without mutating `Cargo.lock`.
//! - Item 10: relative plugin directories (and paths with spaces) invoke local Vite via its
//!   canonical absolute path and resolve absolute host `web_root`.

use plugin_cli::run;
use plugin_package::{PackagePolicy, validate_cmplugin_bytes};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tempfile::TempDir;

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

fn host_cargo_home() -> PathBuf {
    std::env::var_os("CARGO_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))
        .expect("CARGO_HOME or HOME must be set")
}

fn build_crate_tarball(work: &Path) -> (Vec<u8>, String) {
    let crate_dir = work.join("cmregprobe-0.1.0");
    fs::create_dir_all(crate_dir.join("src")).expect("mkdir crate");
    fs::write(
        crate_dir.join("Cargo.toml"),
        "[package]\nname = \"cmregprobe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("write Cargo.toml");
    fs::write(
        crate_dir.join("src/lib.rs"),
        "#![no_std]\npub fn marker_probe_value() -> u32 { 42 }\n",
    )
    .expect("write lib.rs");
    let archive = work.join("cmregprobe-0.1.0.crate");
    let status = Command::new("tar")
        .env("COPYFILE_DISABLE", "1")
        .args(["-czf"])
        .arg(&archive)
        .arg("-C")
        .arg(work)
        .arg("cmregprobe-0.1.0")
        .status()
        .expect("run tar");
    assert!(status.success(), "tar failed");
    let bytes = fs::read(&archive).expect("read crate tarball");
    let cksum = format!("{:x}", Sha256::digest(&bytes));
    (bytes, cksum)
}

struct LocalSparseRegistry {
    addr: SocketAddr,
    downloads: Arc<std::sync::atomic::AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl LocalSparseRegistry {
    fn start(tarball: Vec<u8>, cksum: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind sparse registry");
        let addr = listener.local_addr().expect("addr");
        listener.set_nonblocking(true).expect("nonblocking");
        let downloads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let dl_hits = downloads.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let config_json = format!(
            "{{\"dl\":\"http://{addr}/dl/{{crate}}/{{version}}/download\",\"api\":\"http://{addr}\"}}"
        );
        let index_entry = format!(
            "{{\"name\":\"cmregprobe\",\"vers\":\"0.1.0\",\"deps\":[],\"cksum\":\"{cksum}\",\"features\":{{}},\"yanked\":false}}\n"
        );
        let handle = std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("");
                let (status, ct, body): (&str, &str, &[u8]) = match path {
                    "/index/config.json" => ("200 OK", "application/json", config_json.as_bytes()),
                    "/index/cm/re/cmregprobe" => ("200 OK", "text/plain", index_entry.as_bytes()),
                    "/dl/cmregprobe/0.1.0/download" => {
                        dl_hits.fetch_add(1, Ordering::SeqCst);
                        ("200 OK", "application/octet-stream", tarball.as_slice())
                    }
                    _ => ("404 Not Found", "text/plain", b"not found"),
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body);
                let _ = stream.flush();
            }
        });
        Self {
            addr,
            downloads,
            stop,
            handle: Some(handle),
        }
    }

    fn download_hits(&self) -> usize {
        self.downloads.load(Ordering::SeqCst)
    }

    fn stop_now(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for LocalSparseRegistry {
    fn drop(&mut self) {
        self.stop_now();
    }
}

#[cfg(unix)]
fn prepare_isolated_cargo_home(dir: &Path, registry_addr: SocketAddr) {
    use std::os::unix::fs::symlink;
    let real = host_cargo_home();
    for sub in ["cache", "index", "src"] {
        fs::create_dir_all(dir.join("registry").join(sub)).expect("mkdir registry sub");
    }
    for sub in ["cache", "index"] {
        let src_dir = real.join("registry").join(sub);
        if let Ok(entries) = fs::read_dir(&src_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if name.to_string_lossy().starts_with("index.crates.io-") {
                    let _ = symlink(entry.path(), dir.join("registry").join(sub).join(&name));
                }
            }
        }
    }
    fs::write(
        dir.join("config.toml"),
        format!(
            "[registries.local-test]\nindex = \"sparse+http://{registry_addr}/index/\"\n\n[net]\nretry = 0\n"
        ),
    )
    .expect("write cargo config.toml");
}

fn run_child_plugin_build(
    cwd: &Path,
    project_arg: &str,
    cargo_home: Option<&Path>,
    target_dir: Option<&Path>,
    offline_env: Option<&str>,
) -> std::process::Output {
    let mut cmd = Command::new(std::env::current_exe().expect("current_exe"));
    cmd.args([
        "--exact",
        "child_exec_plugin_build_helper",
        "--nocapture",
        "--ignored",
    ])
    .current_dir(cwd)
    .env("CM_CHILD_PROJECT_ARG", project_arg)
    .env_remove("CARGO_NET_OFFLINE");
    if let Some(ch) = cargo_home {
        cmd.env("CARGO_HOME", ch);
    }
    if let Some(td) = target_dir {
        cmd.env("CARGO_TARGET_DIR", td);
    }
    if let Some(val) = offline_env {
        cmd.env("CARGO_NET_OFFLINE", val);
    }
    cmd.output().expect("spawn child plugin build")
}

#[test]
#[ignore]
fn child_exec_plugin_build_helper() {
    let proj_arg = std::env::var("CM_CHILD_PROJECT_ARG").expect("CM_CHILD_PROJECT_ARG");
    if let Err(err) = run(&args(&["build", &proj_arg])) {
        eprintln!("BUILD_ERR: {err}");
        std::process::exit(1);
    }
}

#[cfg(unix)]
#[test]
fn build_fetches_locked_dependency_into_fresh_cache_and_honors_offline_and_lock_drift() {
    let tmp = TempDir::new().expect("tmp");
    let (tarball, cksum) = build_crate_tarball(tmp.path());
    let mut reg = LocalSparseRegistry::start(tarball, cksum.clone());
    let proj = tmp.path().join("locked-plugin");
    run(&args(&[
        "scaffold",
        "--world",
        "hook-plugin",
        "--api",
        "1.0.0",
        "--id",
        "org.example.locked-fetch",
        &proj.to_string_lossy(),
    ]))
    .expect("scaffold");

    // Add locked dependency served exclusively by our local HTTP sparse registry.
    let toml_path = proj.join("Cargo.toml");
    let orig_toml = fs::read_to_string(&toml_path).expect("read Cargo.toml");
    fs::write(
        &toml_path,
        orig_toml.replace(
            "dlmalloc = { version = \"=0.2.13\", features = [\"global\"] }\n",
            "dlmalloc = { version = \"=0.2.13\", features = [\"global\"] }\ncmregprobe = { version = \"=0.1.0\", registry = \"local-test\" }\n",
        ),
    )
    .expect("update Cargo.toml");
    let lock_path = proj.join("Cargo.lock");
    let orig_lock = fs::read_to_string(&lock_path).expect("read Cargo.lock");
    let updated_lock = format!(
        "{}\n[[package]]\nname = \"cmregprobe\"\nversion = \"0.1.0\"\nsource = \"sparse+http://{}/index/\"\nchecksum = \"{cksum}\"\n",
        orig_lock.replace(
            "dependencies = [\n \"dlmalloc\",\n",
            "dependencies = [\n \"cmregprobe\",\n \"dlmalloc\",\n",
        ),
        reg.addr
    );
    fs::write(&lock_path, &updated_lock).expect("update Cargo.lock");
    let locked_bytes = fs::read(&lock_path).expect("read locked bytes");

    let cargo_home = tmp.path().join("cargo-home");
    prepare_isolated_cargo_home(&cargo_home, reg.addr);
    let target_dir = tmp.path().join("target");

    // 1. Fresh cache + explicit CARGO_NET_OFFLINE=true must fail when dep is uncached.
    let cold_offline = run_child_plugin_build(
        tmp.path(),
        &proj.to_string_lossy(),
        Some(&cargo_home),
        Some(&target_dir),
        Some("true"),
    );
    assert!(
        !cold_offline.status.success(),
        "explicit CARGO_NET_OFFLINE=true must fail on uncached locked dependency"
    );

    // 2. Fresh cache (without CARGO_NET_OFFLINE) must fetch locked dep, build wasm, keep Cargo.lock intact.
    let fresh_ok = run_child_plugin_build(
        tmp.path(),
        &proj.to_string_lossy(),
        Some(&cargo_home),
        Some(&target_dir),
        None,
    );
    assert!(
        fresh_ok.status.success(),
        "fresh cache build must succeed:\n{}",
        String::from_utf8_lossy(&fresh_ok.stderr)
    );
    assert!(proj.join("components/main.wasm").is_file());
    assert_eq!(
        reg.download_hits(),
        1,
        "must fetch locked dep from local registry"
    );
    assert_eq!(fs::read(&lock_path).expect("lock after"), locked_bytes);

    // 3. Stop registry; warmed cache + CARGO_NET_OFFLINE=true must still succeed.
    reg.stop_now();
    let warm_offline = run_child_plugin_build(
        tmp.path(),
        &proj.to_string_lossy(),
        Some(&cargo_home),
        Some(&target_dir),
        Some("true"),
    );
    assert!(
        warm_offline.status.success(),
        "warmed cache offline build must succeed:\n{}",
        String::from_utf8_lossy(&warm_offline.stderr)
    );

    // 4. Manifest/lockfile mismatch must fail under --locked and never rewrite Cargo.lock.
    let drifted_toml = fs::read_to_string(&toml_path)
        .expect("read")
        .replace("version = \"0.1.0\"", "version = \"0.2.0\"");
    fs::write(&toml_path, drifted_toml).expect("write drifted toml");
    let drift_fail = run_child_plugin_build(
        tmp.path(),
        &proj.to_string_lossy(),
        Some(&cargo_home),
        Some(&target_dir),
        Some("true"),
    );
    assert!(!drift_fail.status.success(), "lockfile drift must fail");
    assert_eq!(
        fs::read(&lock_path).expect("lock unchanged"),
        locked_bytes,
        "--locked must never rewrite Cargo.lock"
    );
}

fn seed_ui_plugin_with_local_vite(dir: &Path, id: &str) {
    run(&args(&[
        "scaffold",
        "--ui",
        "react",
        "--id",
        id,
        &dir.to_string_lossy(),
    ]))
    .expect("scaffold ui-only");
    let vite_bin = dir.join("ui/node_modules/vite/bin/vite.js");
    fs::create_dir_all(vite_bin.parent().unwrap()).expect("mkdir local vite");
    fs::write(
        &vite_bin,
        r#"const fs = require("fs");
const path = require("path");
if (!process.cwd().endsWith(`${path.sep}ui`)) {
  console.error("unexpected cwd:", process.cwd());
  process.exit(2);
}
fs.mkdirSync(path.join(process.cwd(), "assets"), { recursive: true });
fs.writeFileSync(path.join(process.cwd(), "assets", "remoteEntry.js"), "export const ok = true;\n");
"#,
    )
    .expect("write local vite stub");
}

#[test]
fn build_ui_invokes_local_vite_for_relative_absolute_and_spaced_directories() {
    let tmp = TempDir::new().expect("tmp");
    let cases = [
        ("demo", "./demo", "org.example.rel-vite"),
        ("demo abs", "./demo abs", "org.example.spaced-vite"),
    ];
    for (folder, rel_arg, plugin_id) in cases {
        let proj = tmp.path().join(folder);
        seed_ui_plugin_with_local_vite(&proj, plugin_id);
        let out = run_child_plugin_build(tmp.path(), rel_arg, None, None, None);
        assert!(
            out.status.success(),
            "relative build `{rel_arg}` failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let pkg = tmp.path().join(format!("{plugin_id}.cmplugin"));
        run(&args(&[
            "pack",
            &proj.to_string_lossy(),
            "--output",
            &pkg.to_string_lossy(),
        ]))
        .expect("pack");
        let validated = validate_cmplugin_bytes(
            &fs::read(&pkg).expect("read pkg"),
            &PackagePolicy {
                dev_mode: true,
                ..PackagePolicy::default()
            },
        )
        .expect("validate packed ui");
        assert!(validated.ui.is_some());
        assert!(proj.join("ui/assets/remoteEntry.js").is_file());
    }

    // Absolute path case too.
    let abs_proj = tmp.path().join("abs demo");
    seed_ui_plugin_with_local_vite(&abs_proj, "org.example.abs-vite");
    let abs_out = run_child_plugin_build(tmp.path(), &abs_proj.to_string_lossy(), None, None, None);
    assert!(
        abs_out.status.success(),
        "absolute build failed:\n{}",
        String::from_utf8_lossy(&abs_out.stderr)
    );
    assert!(abs_proj.join("ui/assets/remoteEntry.js").is_file());
}

#[cfg(unix)]
fn run_child_plugin_build_with_path(
    cwd: &Path,
    project_arg: &str,
    target_dir: &Path,
    fake_bin: &Path,
    extra_envs: &[(&str, &str)],
) -> std::process::Output {
    let orig_path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_val = std::ffi::OsString::from(fake_bin.as_os_str());
    path_val.push(":");
    path_val.push(orig_path);
    let mut cmd = Command::new(std::env::current_exe().expect("current_exe"));
    cmd.args([
        "--exact",
        "child_exec_plugin_build_helper",
        "--nocapture",
        "--ignored",
    ])
    .current_dir(cwd)
    .env("CM_CHILD_PROJECT_ARG", project_arg)
    .env("CARGO_TARGET_DIR", target_dir)
    .env("PATH", path_val);
    for (k, v) in extra_envs {
        cmd.env(k, v);
    }
    cmd.output().expect("spawn child plugin build")
}

#[cfg(unix)]
fn write_fake_cargo(fake_bin: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(fake_bin).expect("mkdir fake_bin");
    let script = fake_bin.join("cargo");
    fs::write(
        &script,
        r#"#!/bin/sh
set -eu
if [ "${1:-}" = "metadata" ]; then
  manifest_path=""
  prev=""
  for arg in "$@"; do
    if [ "$prev" = "--manifest-path" ]; then
      manifest_path="$arg"
      break
    fi
    prev="$arg"
  done
  pkg_dir="$(cd "$(dirname "$manifest_path")" && pwd)"
  pkg_name="${FAKE_CARGO_ROOT_PKG:-hook}"
  pkg_id="path+file://${pkg_dir}#${pkg_name}@0.1.0"
  printf '{"packages":[{"id":"%s","name":"%s","manifest_path":"%s"}],"workspace_members":["%s"],"workspace_default_members":["%s"],"resolve":{"root":"%s"}}\n' \
    "$pkg_id" "$pkg_name" "$manifest_path" "$pkg_id" "$pkg_id" "$pkg_id"
  exit 0
fi
out_dir="${CARGO_TARGET_DIR}/wasm32-wasip2/release"
mkdir -p "$out_dir"
manifest_path=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "--manifest-path" ]; then
    manifest_path="$arg"
    break
  fi
  prev="$arg"
done
pkg_dir="$(cd "$(dirname "$manifest_path")" && pwd)"
root_pkg="${FAKE_CARGO_ROOT_PKG:-hook}"
IFS=','
for spec in $FAKE_CARGO_ARTIFACTS; do
  # Format: [pkg_id_override@]target_name=src_path
  lhs="${spec%%=*}"
  src="${spec#*=}"
  case "$lhs" in
    *@*)
      art_pkg="${lhs%%@*}"
      name="${lhs#*@}"
      pkg_id="path+file:///deps/${art_pkg}#${art_pkg}@0.1.0"
      ;;
    *)
      name="$lhs"
      pkg_id="path+file://${pkg_dir}#${root_pkg}@0.1.0"
      ;;
  esac
  dest="${out_dir}/${name}.wasm"
  cp "$src" "$dest"
  printf '{"reason":"compiler-artifact","package_id":"%s","target":{"name":"%s","kind":["cdylib"]},"filenames":["%s"]}\n' "$pkg_id" "$name" "$dest"
done
"#,
    )
    .expect("write fake cargo");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod fake cargo");
}

#[cfg(unix)]
fn seed_multi_component_project(proj: &Path, task_bytes: &[u8]) {
    fs::create_dir_all(proj.join("components")).expect("mkdir components");
    fs::write(
        proj.join("plugin.toml"),
        r#"manifest_version = 1
id = "org.example.multi-comp"
name = "Multi Component"
version = "0.1.0"
api = "1.0.0"
license = "MIT"
authors = ["test"]
description = "Multi-component preservation test"
config_schema = "config.schema.json"
state_schema = 1

[[components]]
id = "hook"
entry = "components/hook.wasm"
world = "crawler:plugin/hook-plugin@1.0.0"
permissions = ["hooks:download.after"]

[[components.hooks]]
id = "download.after"
priority = 10

[[components]]
id = "task"
entry = "components/task.wasm"
world = "crawler:plugin/task-plugin@1.0.0"
permissions = []

[[components.tasks]]
id = "sync"
input_schema = "config.schema.json"

[components.tasks.schedule]
kind = "interval"
seconds = 3600
"#,
    )
    .expect("write plugin.toml");
    fs::write(
        proj.join("config.schema.json"),
        r#"{"type":"object","properties":{},"additionalProperties":false}"#,
    )
    .expect("write schema");
    fs::write(
        proj.join("Cargo.toml"),
        "[package]\nname = \"hook\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("write Cargo.toml");
    fs::write(proj.join("components/task.wasm"), task_bytes).expect("write prebuilt task.wasm");
}

#[cfg(unix)]
#[test]
fn build_multi_component_preserves_prebuilt_entries_and_rejects_ambiguous_artifacts_before_writing()
{
    let tmp = TempDir::new().expect("tmp");
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let policy_fixture = repo_root.join("crates/plugin-host/tests/fixtures/policy.wasm");
    let task_fixture = repo_root.join("crates/plugin-host/tests/fixtures/task_fixture.wasm");
    let policy_bytes = fs::read(&policy_fixture).expect("read policy.wasm");
    let task_bytes = fs::read(&task_fixture).expect("read task_fixture.wasm");
    assert_ne!(policy_bytes, task_bytes);

    let proj = tmp.path().join("multi-comp");
    seed_multi_component_project(&proj, &task_bytes);

    let fake_bin = tmp.path().join("fake-bin");
    write_fake_cargo(&fake_bin);
    let target_dir = tmp.path().join("target");

    // 1. Cargo builds only `hook` -> hook.wasm is written, prebuilt task.wasm stays byte-identical.
    let ok_spec = format!("hook={}", policy_fixture.display());
    let ok_out = run_child_plugin_build_with_path(
        tmp.path(),
        &proj.to_string_lossy(),
        &target_dir,
        &fake_bin,
        &[("FAKE_CARGO_ARTIFACTS", &ok_spec)],
    );
    assert!(
        ok_out.status.success(),
        "multi-component build with prebuilt task must succeed:\n{}",
        String::from_utf8_lossy(&ok_out.stderr)
    );
    assert_eq!(
        fs::read(proj.join("components/hook.wasm")).expect("read hook.wasm"),
        policy_bytes,
        "hook.wasm must be updated from compiled artifact"
    );
    assert_eq!(
        fs::read(proj.join("components/task.wasm")).expect("read task.wasm"),
        task_bytes,
        "prebuilt task.wasm must never be overwritten by hook artifact"
    );

    // Pack + validate must succeed cleanly.
    let pkg = tmp.path().join("org.example.multi-comp.cmplugin");
    run(&args(&[
        "pack",
        &proj.to_string_lossy(),
        "--output",
        &pkg.to_string_lossy(),
    ]))
    .expect("pack multi-component plugin");
    let validated = validate_cmplugin_bytes(
        &fs::read(&pkg).expect("read pkg"),
        &PackagePolicy {
            dev_mode: true,
            ..PackagePolicy::default()
        },
    )
    .expect("validate packed multi-component plugin");
    assert_eq!(validated.manifest.components.len(), 2);

    // 2. Unmapped / ambiguous Cargo artifact must fail BEFORE touching any existing entry on disk.
    let bad_spec = format!("unrelated={}", task_fixture.display());
    let bad_out = run_child_plugin_build_with_path(
        tmp.path(),
        &proj.to_string_lossy(),
        &target_dir,
        &fake_bin,
        &[("FAKE_CARGO_ARTIFACTS", &bad_spec)],
    );
    assert!(
        !bad_out.status.success(),
        "unmapped multi-component artifact must fail build"
    );
    assert_eq!(
        fs::read(proj.join("components/hook.wasm")).expect("hook untouched"),
        policy_bytes,
        "hook.wasm must remain untouched on ambiguous build failure"
    );
    assert_eq!(
        fs::read(proj.join("components/task.wasm")).expect("task untouched"),
        task_bytes,
        "task.wasm must remain untouched on ambiguous build failure"
    );
}

#[cfg(unix)]
fn seed_real_dep_plugin(tmp_root: &Path) -> PathBuf {
    let helper_dir = tmp_root.join("helper");
    fs::create_dir_all(helper_dir.join("src")).expect("mkdir helper");
    fs::write(
        helper_dir.join("Cargo.toml"),
        "[package]\nname = \"helper\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\ncrate-type = [\"rlib\", \"cdylib\"]\n",
    )
    .expect("write helper Cargo.toml");
    fs::write(
        helper_dir.join("src/lib.rs"),
        "pub fn helper_magic() -> u32 { 7 }\n",
    )
    .expect("write helper lib.rs");

    let real_proj = tmp_root.join("real-dep-plugin");
    run(&args(&[
        "scaffold",
        "--world",
        "hook-plugin",
        "--api",
        "1.0.0",
        "--id",
        "org.example.dep-cdylib",
        &real_proj.to_string_lossy(),
    ]))
    .expect("scaffold hook-plugin");
    let real_toml = fs::read_to_string(real_proj.join("Cargo.toml")).expect("read Cargo.toml");
    fs::write(
        real_proj.join("Cargo.toml"),
        real_toml.replace(
            "dlmalloc = { version = \"=0.2.13\", features = [\"global\"] }\n",
            "dlmalloc = { version = \"=0.2.13\", features = [\"global\"] }\nhelper = { path = \"../helper\" }\n",
        ),
    )
    .expect("add helper dep");
    let real_src = fs::read_to_string(real_proj.join("src/lib.rs")).expect("read lib.rs");
    let real_src = real_src.replace(
        "    #[panic_handler]\n    fn panic(_: &core::panic::PanicInfo<'_>) -> ! {\n        core::arch::wasm32::unreachable()\n    }\n",
        "",
    );
    fs::write(
        real_proj.join("src/lib.rs"),
        format!("{real_src}\n#[allow(dead_code)]\nfn _use_helper() -> u32 {{ helper::helper_magic() }}\n"),
    )
    .expect("use helper in lib.rs");
    let _ = fs::remove_file(real_proj.join("Cargo.lock"));
    real_proj
}

#[cfg(unix)]
#[test]
fn build_ignores_dependency_cdylib() {
    let tmp = TempDir::new().expect("tmp");
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let policy_fixture = repo_root.join("crates/plugin-host/tests/fixtures/policy.wasm");
    let task_fixture = repo_root.join("crates/plugin-host/tests/fixtures/task_fixture.wasm");
    let policy_bytes = fs::read(&policy_fixture).expect("read policy.wasm");
    let task_bytes = fs::read(&task_fixture).expect("read task_fixture.wasm");

    // 1. Dependency cdylib (`helper`) emitted alongside root `hook` is ignored;
    //    dependency cdylib named `task` never overwrites prebuilt `components/task.wasm`.
    let proj = tmp.path().join("dep-collision");
    seed_multi_component_project(&proj, &task_bytes);
    let fake_bin = tmp.path().join("fake-bin");
    write_fake_cargo(&fake_bin);
    let target_dir = tmp.path().join("target");

    let spec = format!(
        "helper_dep@helper={},task_dep@task={},hook={}",
        policy_fixture.display(),
        policy_fixture.display(),
        policy_fixture.display()
    );
    let out = run_child_plugin_build_with_path(
        tmp.path(),
        &proj.to_string_lossy(),
        &target_dir,
        &fake_bin,
        &[("FAKE_CARGO_ARTIFACTS", &spec)],
    );
    assert!(
        out.status.success(),
        "build must ignore dependency cdylib artifacts:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read(proj.join("components/hook.wasm")).unwrap(),
        policy_bytes
    );
    assert_eq!(
        fs::read(proj.join("components/task.wasm")).unwrap(),
        task_bytes
    );

    // 2. Real offline Cargo build with a local `rlib` + `cdylib` path dependency.
    let real_proj = seed_real_dep_plugin(tmp.path());
    let real_target = tmp.path().join("real-target");
    let real_out = run_child_plugin_build(
        tmp.path(),
        &real_proj.to_string_lossy(),
        None,
        Some(&real_target),
        Some("true"),
    );
    assert!(
        real_out.status.success(),
        "real offline Cargo build with cdylib path dependency must succeed:\n{}",
        String::from_utf8_lossy(&real_out.stderr)
    );
    let built_entry =
        fs::read(real_proj.join("components/main.wasm")).expect("read built components/main.wasm");
    let expected_root =
        fs::read(real_target.join("wasm32-wasip2/release/cm_plugin_real_dep_plugin.wasm"))
            .expect("root plugin artifact must exist");
    assert_eq!(built_entry, expected_root);
    let pkg = tmp.path().join("org.example.dep-cdylib.cmplugin");
    run(&args(&[
        "pack",
        &real_proj.to_string_lossy(),
        "--output",
        &pkg.to_string_lossy(),
    ]))
    .expect("pack built plugin");
    let validated = validate_cmplugin_bytes(
        &fs::read(&pkg).expect("read pkg"),
        &PackagePolicy {
            dev_mode: true,
            ..PackagePolicy::default()
        },
    )
    .expect("validate packed plugin");
    assert_eq!(validated.manifest.id, "org.example.dep-cdylib");
}
