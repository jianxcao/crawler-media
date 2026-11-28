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
