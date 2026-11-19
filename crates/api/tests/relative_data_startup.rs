//! Internal data paths resolve against each server process's working directory.

#![cfg(unix)]

use std::fs::File;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const TOKEN: &str = "relative-data-cli-token";

struct Server {
    child: Child,
    base: String,
    agent: ureq::Agent,
    log_path: PathBuf,
}

impl Server {
    fn start(cwd: &Path, data_env: Option<&str>) -> Self {
        Self::start_with_read_only(cwd, data_env, false)
    }

    fn start_with_read_only(cwd: &Path, data_env: Option<&str>, read_only: bool) -> Self {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let log_path = cwd.join(format!("server-{port}.log"));
        let log = File::create(&log_path).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_crawler-media"));
        command
            .arg("serve")
            .current_dir(cwd)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", cwd)
            .env("CRAWLER_MEDIA_TOKEN", TOKEN)
            .env(
                "CRAWLER_MEDIA_ADMIN_PASSWORD",
                "relative-data-admin-password",
            )
            .env("CRAWLER_MEDIA_LISTEN", format!("127.0.0.1:{port}"))
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log));
        if let Some(data) = data_env {
            command.env("CRAWLER_MEDIA_DATA", data);
        }
        let mut server = Self {
            child: command.spawn().unwrap(),
            base: format!("http://127.0.0.1:{port}"),
            agent: ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_global(Some(Duration::from_secs(2)))
                .build()
                .new_agent(),
            log_path,
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let exit = server.child.try_wait().unwrap();
            assert!(
                exit.is_none() && Instant::now() < deadline,
                "server failed to become ready ({exit:?}): {}",
                std::fs::read_to_string(&server.log_path).unwrap()
            );
            if let Ok(mut response) = server
                .agent
                .get(&format!("{}/api/v1/health", server.base))
                .call()
                && response.status().as_u16() == 200
            {
                let health: Value =
                    serde_json::from_str(&response.body_mut().read_to_string().unwrap()).unwrap();
                assert_eq!(health["data"]["read_only"], read_only, "{health}");
                return server;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn request(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let url = format!("{}{path}", self.base);
        let auth = format!("Bearer {TOKEN}");
        let mut response = match method {
            "GET" => self.agent.get(&url).header("Authorization", &auth).call(),
            "POST" => self
                .agent
                .post(&url)
                .header("Authorization", &auth)
                .send_json(body.unwrap()),
            "PATCH" => self
                .agent
                .patch(&url)
                .header("Authorization", &auth)
                .send_json(body.unwrap()),
            _ => panic!("unsupported method {method}"),
        }
        .unwrap();
        let status = response.status().as_u16();
        let body = serde_json::from_str(&response.body_mut().read_to_string().unwrap()).unwrap();
        (status, body)
    }

    fn stop(mut self) {
        assert!(
            Command::new("kill")
                .args(["-TERM", &self.child.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "shutdown failed: {status}");
                return;
            }
            assert!(Instant::now() < deadline, "server did not stop");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn library_ids(server: &Server, expected_data: &Path) -> Vec<(String, String)> {
    let (status, body) = server.request("GET", "/api/v1/libraries?scope=admin", None);
    assert_eq!(status, 200, "{body}");
    let libraries = body["data"].as_array().unwrap();
    assert_eq!(libraries.len(), 2, "{body}");
    let mut ids = Vec::new();
    for (kind, suffix) in [("movie", "movies"), ("tv", "tv")] {
        let library = libraries
            .iter()
            .find(|library| library["kind"] == kind)
            .unwrap();
        let roots = library["root_paths"].as_array().unwrap();
        assert_eq!(roots.len(), 1, "{library}");
        let root = Path::new(roots[0].as_str().unwrap());
        assert!(root.is_absolute(), "{library}");
        assert_eq!(root, expected_data.join("library").join(suffix));
        assert!(root.is_dir());
        let id = library["id"].as_str().unwrap().to_owned();
        assert!(!id.is_empty());
        ids.push((kind.to_owned(), id));
    }
    ids
}

fn check_startup(data_env: Option<&str>, relative_data: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().canonicalize().unwrap();
    let expected_data = cwd.join(relative_data);
    let server = Server::start(&cwd, data_env);
    let ids = library_ids(&server, &expected_data);
    for (method, path, body) in [
        (
            "POST",
            "/api/v1/libraries".to_owned(),
            json!({
                "kind": "movie", "name": "Relative root", "root_paths": ["external-relative"],
            }),
        ),
        (
            "PATCH",
            format!("/api/v1/libraries/{}", ids[0].1),
            json!({
                "root_paths": ["external-relative"],
            }),
        ),
    ] {
        let (status, response) = server.request(method, &path, Some(body));
        assert_eq!(status, 400, "{method} {path}: {response}");
    }
    assert!(!cwd.join("external-relative").exists());
    assert_eq!(library_ids(&server, &expected_data), ids);
    server.stop();

    let restarted = Server::start(&cwd, data_env);
    assert_eq!(library_ids(&restarted, &expected_data), ids);
    for file in [
        "app.db",
        "library.db",
        "subscribe.db",
        "cache.db",
        "jobs.db",
    ] {
        assert!(expected_data.join(file).is_file(), "missing {file}");
    }
    restarted.stop();
}

#[test]
fn default_data_directory_starts_and_preserves_library_ids_on_restart() {
    check_startup(None, "data");
}

#[test]
fn relative_data_env_starts_and_preserves_library_ids_on_restart() {
    check_startup(Some("nested/data"), "nested/data");
}

#[test]
fn same_data_directory_allows_only_one_writable_process() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().canonicalize().unwrap();
    let first = Server::start(&cwd, None);
    let second = Server::start_with_read_only(&cwd, None, true);
    assert_ne!(first.base, second.base);
    let (status, first_health) = first.request("GET", "/api/v1/health", None);
    assert_eq!(status, 200);
    assert_eq!(first_health["data"]["read_only"], false);
    let (status, second_health) = second.request("GET", "/api/v1/health", None);
    assert_eq!(status, 200);
    assert_eq!(second_health["data"]["read_only"], true);
    second.stop();
    first.stop();
}
