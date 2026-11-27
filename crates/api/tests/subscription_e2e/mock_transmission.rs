//! Stateful Transmission RPC mock (rpc-version configurable; < 16 = no labels).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct TrTask {
    pub hash: String,
    pub name: String,
    pub filename: String,
    pub labels: Vec<String>,
    pub percent: f64,
    /// (name relative to download dir, length, bytes completed, wanted)
    pub files: Vec<(String, u64, u64, bool)>,
}

pub struct MockTransmission {
    pub url: String,
    state: Arc<MockTransmissionState>,
    // The handler owns only state, so it can never drop and join its own server.
    _server: crate::lifecycle::FakeServer,
}

impl std::ops::Deref for MockTransmission {
    type Target = MockTransmissionState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

pub struct MockTransmissionState {
    pub download_dir: PathBuf,
    pub rpc_version: u64,
    pub tasks: Mutex<Vec<TrTask>>,
    pub sets: Mutex<Vec<Value>>,
    /// F-1: file names (metadata) given to the next added task.
    pub next_files: Mutex<Vec<String>>,
    next: AtomicU64,
}

impl MockTransmission {
    pub fn start(download_dir: &Path, rpc_version: u64) -> Arc<Self> {
        Self::start_with_handler_hook(download_dir, rpc_version, || {})
    }

    pub fn start_with_handler_hook(
        download_dir: &Path,
        rpc_version: u64,
        before_request: impl Fn() + Send + Sync + 'static,
    ) -> Arc<Self> {
        std::fs::create_dir_all(download_dir).unwrap();
        let state = Arc::new(MockTransmissionState {
            download_dir: download_dir.to_path_buf(),
            rpc_version,
            tasks: Mutex::new(Vec::new()),
            sets: Mutex::new(Vec::new()),
            next_files: Mutex::new(Vec::new()),
            next: AtomicU64::new(1),
        });
        let shared = state.clone();
        let server = crate::lifecycle::FakeServer::start(move |request, stream| {
            before_request();
            let body = std::str::from_utf8(&request.body).unwrap_or("{}");
            let call: Value = serde_json::from_str(body).unwrap_or(json!({}));
            let reply = shared.handle(&call).to_string();
            crate::lifecycle::respond(
                stream,
                "200 OK",
                "Content-Type: application/json\r\n",
                reply.as_bytes(),
            );
        });
        Arc::new(Self {
            url: format!("{}/transmission/rpc", server.url()),
            state,
            _server: server,
        })
    }

    pub fn config(&self) -> downloader::TransmissionConfig {
        downloader::TransmissionConfig {
            url: self.url.clone(),
            username: None,
            password: None,
            path_maps: Vec::new(),
        }
    }

    /// Finish task `index`: write `files` (relative to `<dir>/<name>/`) to disk.
    pub fn complete(&self, index: usize, name: &str, files: &[(&str, &[u8])]) {
        let mut tasks = self.tasks.lock();
        let task = &mut tasks[index];
        task.name = name.into();
        task.percent = 1.0;
        task.files.clear();
        for (rel, content) in files {
            let rel = format!("{name}/{rel}");
            let path = self.download_dir.join(&rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, content).unwrap();
            let len = content.len() as u64;
            task.files.push((rel, len, len, true));
        }
    }
}

impl MockTransmissionState {
    fn handle(&self, call: &Value) -> Value {
        let args = &call["arguments"];
        match call["method"].as_str().unwrap_or_default() {
            "session-get" => {
                json!({"result": "success", "arguments": {"rpc-version": self.rpc_version}})
            }
            "torrent-add" => {
                let n = self.next.fetch_add(1, Ordering::SeqCst);
                let filename = args["filename"].as_str().unwrap_or_default().to_string();
                let hash = args["metainfo"]
                    .as_str()
                    .and_then(decode_b64)
                    .and_then(|bytes| downloader::torrent_info_hash(&bytes))
                    .or_else(|| downloader::magnet_info_hash(&filename))
                    .unwrap_or_else(|| format!("{n:040x}"));
                let labels = args["labels"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                self.tasks.lock().push(TrTask {
                    hash: hash.clone(),
                    name: format!("task-{n}"),
                    filename,
                    labels,
                    percent: 0.0,
                    files: std::mem::take(&mut *self.next_files.lock())
                        .into_iter()
                        .map(|name| (name, 1, 0, true))
                        .collect(),
                });
                json!({"result": "success", "arguments": {"torrent-added": {"hashString": hash, "id": n}}})
            }
            "torrent-set" => {
                self.sets.lock().push(args.clone());
                if let Some(labels) = args["labels"].as_array() {
                    let parsed: Vec<String> = labels
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                    let ids: Vec<u64> = args["ids"]
                        .as_array()
                        .map(|arr| arr.iter().filter_map(|v| v.as_u64()).collect())
                        .unwrap_or_default();
                    let mut tasks = self.tasks.lock();
                    for (i, t) in tasks.iter_mut().enumerate() {
                        if ids.is_empty() || ids.contains(&((i + 1) as u64)) {
                            t.labels = parsed.clone();
                        }
                    }
                }
                json!({"result": "success", "arguments": {}})
            }
            "torrent-get" => {
                let tasks = self.tasks.lock();
                let rows: Vec<Value> = tasks
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let size: u64 = t.files.iter().map(|f| f.1).sum();
                        let mut row = json!({
                            "id": i + 1, "name": t.name, "hashString": t.hash,
                            "totalSize": size, "sizeWhenDone": size, "percentDone": t.percent,
                            "downloadDir": self.download_dir.display().to_string(),
                            "rateDownload": 0, "rateUpload": 0, "status": 4, "error": 0,
                            "downloadedEver": 0, "uploadedEver": 0,
                            "files": t.files.iter().map(|f| json!({"name": f.0, "length": f.1, "bytesCompleted": f.2})).collect::<Vec<_>>(),
                            "fileStats": t.files.iter().map(|f| json!({"wanted": f.3})).collect::<Vec<_>>(),
                        });
                        if self.rpc_version >= 16 {
                            row["labels"] = json!(t.labels);
                        }
                        row
                    })
                    .collect();
                json!({"result": "success", "arguments": {"torrents": rows}})
            }
            _ => json!({"result": "success", "arguments": {}}),
        }
    }
}

fn decode_b64(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut acc: u32 = 0;
    let mut bits: u8 = 0;
    for b in input.bytes() {
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\r' | b'\n' | b' ' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(val);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}
