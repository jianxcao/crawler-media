#[path = "../../api/tests/support/lifecycle.rs"]
#[allow(dead_code)]
mod lifecycle;

use base64::Engine;
use lifecycle::FakeServer;
use std::io::Write;
use std::sync::Arc;

use parking_lot::Mutex;

use domain::{DownloaderId, SiteId, Torrent};
use downloader::{
    ClientSubmitResult, Downloader, DownloaderSet, MemoryDownloader, SubmitOptions, SubmitPayload,
    TransmissionConfig, TransmissionDownloader, parse_verified_torrent,
};

struct Capture {
    methods: Mutex<Vec<String>>,
    requests: Mutex<Vec<String>>,
    injected_torrents: Mutex<Option<String>>,
    injected_add: Mutex<Option<String>>,
    rpc_version: Mutex<u64>,
    drop_after_add: Mutex<bool>,
}

impl Capture {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            methods: Mutex::new(Vec::new()),
            requests: Mutex::new(Vec::new()),
            injected_torrents: Mutex::new(None),
            injected_add: Mutex::new(None),
            rpc_version: Mutex::new(16),
            drop_after_add: Mutex::new(false),
        })
    }
}

const SAMPLE_TORRENT: &[u8] =
    b"d8:announce10:https://pt4:infod6:lengthi1e4:name1:a12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaaee";

fn sample_payload() -> SubmitPayload {
    let parsed = parse_verified_torrent(SAMPLE_TORRENT).unwrap();
    SubmitPayload::TorrentBytes {
        bytes: parsed.bytes,
        bytes_digest: parsed.bytes_digest,
        info_hash: parsed.info_hash,
    }
}

fn default_options() -> SubmitOptions {
    SubmitOptions {
        ownership_tag: downloader::ownership_tag("https://pt.example/download.php?id=1"),
        ..Default::default()
    }
}

fn torrent() -> Torrent {
    Torrent {
        site_id: SiteId::new(),
        title: "The.Matrix.1999.2160p.BluRay.x265-GROUP".into(),
        enclosure: "https://pt.example/download.php?id=1".into(),
        size_bytes: Some(1),
        seeders: Some(1),
        free: true,
        hr: Some(false),
        imdb_id: None,
        id: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    }
}

fn serve(capture: Arc<Capture>, save_path: String) -> (String, FakeServer) {
    let server = FakeServer::start(move |request, stream| {
        let req = request.raw.clone();
        let method = if req.contains("session-get") {
            "session-get"
        } else if req.contains("torrent-add") {
            "torrent-add"
        } else if req.contains("torrent-get") {
            "torrent-get"
        } else if req.contains("torrent-remove") {
            "torrent-remove"
        } else if req.contains("torrent-set") {
            "torrent-set"
        } else {
            "unknown"
        };
        capture.methods.lock().push(method.into());
        if method == "torrent-add" || method == "torrent-remove" || method == "torrent-set" {
            capture.requests.lock().push(req.clone());
        }
        if method == "torrent-add" && *capture.drop_after_add.lock() {
            return;
        }
        let rpc_ver = *capture.rpc_version.lock();
        let body = match method {
            "session-get" => {
                format!(r#"{{"result":"success","arguments":{{"rpc-version":{rpc_ver}}}}}"#)
            }
            "torrent-add" => capture.injected_add.lock().clone().unwrap_or_else(|| {
                let parsed = parse_verified_torrent(SAMPLE_TORRENT).unwrap();
                format!(
                    r#"{{"result":"success","arguments":{{"torrent-added":{{"id":1,"hashString":"{}"}}}}}}"#,
                    parsed.info_hash
                )
            }),
            "torrent-set" => r#"{"result":"success","arguments":{}}"#.to_string(),
            "torrent-get" => {
                let injected = capture.injected_torrents.lock().clone();
                if let Some(custom) = injected {
                    custom
                } else {
                    let parsed = parse_verified_torrent(SAMPLE_TORRENT).unwrap();
                    format!(
                        r#"{{"result":"success","arguments":{{"torrents":[{{"id":1,"name":"The.Matrix.1999.2160p.BluRay.x265-GROUP","hashString":"{}","sizeWhenDone":1,"percentDone":1.0,"downloadDir":"{save_path}","error":0,"status":6,"labels":["crawler-media","{}"],"files":[{{"name":"matrix.mkv"}}]}}]}}}}"#,
                        parsed.info_hash,
                        downloader::ownership_tag("https://pt.example/download.php?id=1")
                    )
                }
            }
            "torrent-remove" => r#"{"result":"success","arguments":{}}"#.to_string(),
            _ => r#"{"result":"failure"}"#.to_string(),
        };
        let response = format!(
            "HTTP/1.1 200 OK\r\nX-Transmission-Session-Id: abc\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).ok();
    });
    (format!("{}/transmission/rpc", server.url()), server)
}

#[test]
fn transmission_submit_verified_sends_base64_metainfo_without_filename_url() {
    let capture = Capture::new();
    *capture.injected_torrents.lock() =
        Some(r#"{"result":"success","arguments":{"torrents":[]}}"#.into());
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url,
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();
    assert!(dl.supports_labels());

    let mut opts = default_options();
    opts.save_path = Some("/downloads/manual".into());
    opts.extra_tags = vec!["plugin:p1:tag".into()];
    let res = dl.submit_verified(&sample_payload(), &opts).unwrap();
    assert!(matches!(
        res,
        ClientSubmitResult::Added {
            already_owned: false,
            ..
        }
    ));

    let requests = capture.requests.lock();
    let add_req = requests.iter().find(|r| r.contains("torrent-add")).unwrap();
    let json_body: serde_json::Value =
        serde_json::from_str(add_req.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    let args = &json_body["arguments"];
    assert!(args.get("filename").is_none());
    let metainfo_b64 = args["metainfo"].as_str().unwrap();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(metainfo_b64)
        .unwrap();
    assert_eq!(decoded, SAMPLE_TORRENT);
    assert_eq!(args["download-dir"].as_str(), Some("/downloads/manual"));
}

#[test]
fn transmission_foreign_duplicate_and_unknown_outcomes_never_claim_tasks() {
    let capture = Capture::new();
    *capture.injected_torrents.lock() =
        Some(r#"{"result":"success","arguments":{"torrents":[]}}"#.into());
    *capture.injected_add.lock() = Some(
        r#"{"result":"success","arguments":{"torrent-duplicate":{"hashString":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","id":7}}}"#.into(),
    );
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url,
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();

    let dup = dl
        .submit_verified(&sample_payload(), &default_options())
        .unwrap();
    assert!(matches!(dup, ClientSubmitResult::ForeignDuplicate { .. }));
    assert!(
        !capture
            .requests
            .lock()
            .iter()
            .any(|r| r.contains("torrent-set")),
        "foreign duplicate must never trigger torrent-set label claiming"
    );

    // Empty arguments: {} -> UnknownOutcome
    *capture.injected_add.lock() = Some(r#"{"result":"success","arguments":{}}"#.into());
    let empty_unknown = dl
        .submit_verified(&sample_payload(), &default_options())
        .unwrap();
    assert!(matches!(
        empty_unknown,
        ClientSubmitResult::UnknownOutcome { .. }
    ));

    // Post-send EOF -> UnknownOutcome
    *capture.drop_after_add.lock() = true;
    let eof_unknown = dl
        .submit_verified(&sample_payload(), &default_options())
        .unwrap();
    assert!(matches!(
        eof_unknown,
        ClientSubmitResult::UnknownOutcome { .. }
    ));
}

#[test]
fn transmission_rpc_below_16_reports_no_labels_and_queries_status_accurately() {
    let capture = Capture::new();
    *capture.rpc_version.lock() = 15;
    let parsed = parse_verified_torrent(SAMPLE_TORRENT).unwrap();
    *capture.injected_torrents.lock() = Some(format!(
        r#"{{"result":"success","arguments":{{"torrents":[{{"id":1,"hashString":"{}","error":0,"status":0}}]}}}}"#,
        parsed.info_hash
    ));
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url,
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();
    assert!(!dl.supports_labels());

    let q = dl
        .query_task_status(&parsed.info_hash, &default_options().ownership_tag)
        .unwrap();
    assert!(q.exists);
    assert!(!q.owned_by_tag);
    assert!(!q.broken);
    assert!(q.paused);
}

#[test]
fn transmission_snapshots_preserve_metrics_and_only_mark_owned_tasks() {
    let capture = Capture::new();
    *capture.injected_torrents.lock() = Some(
        serde_json::json!({
            "result": "success", "arguments": {"torrents": [
                {"name": "owned", "hashString": "abc", "totalSize": 2048,
                 "percentDone": 0.5, "rateDownload": 100, "rateUpload": 20,
                 "downloadedEver": 1024, "uploadedEver": 256, "status": 4,
                 "labels": [downloader::TASK_TAG]},
                {"name": "foreign", "hashString": "def", "totalSize": 4096,
                 "percentDone": 1.0, "status": 0, "labels": ["other"]}
            ]}
        })
        .to_string(),
    );
    let (url, _server) = serve(capture, "/downloads".into());
    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url,
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();
    let rows = dl.task_snapshots().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].tag, downloader::TASK_TAG);
    assert_eq!(rows[1].tag, "");
}

#[test]
fn mixed_instances_coexist_and_optional_id_picks_default() {
    let dir = tempfile::tempdir().unwrap();
    let qbit_like = Arc::new(MemoryDownloader::new(dir.path().join("qbit")));
    let tr_like = Arc::new(MemoryDownloader::new(dir.path().join("tr")));
    let qbit_id = DownloaderId::new();
    let tr_id = DownloaderId::new();
    let set = DownloaderSet::new(
        qbit_id,
        [
            (qbit_id, qbit_like.clone() as Arc<dyn Downloader>),
            (tr_id, tr_like.clone() as Arc<dyn Downloader>),
        ]
        .into_iter()
        .collect(),
    )
    .unwrap();
    set.pick(None)
        .unwrap()
        .submit_verified(&sample_payload(), &default_options())
        .unwrap();
    set.pick(Some(tr_id))
        .unwrap()
        .submit_verified(&sample_payload(), &default_options())
        .unwrap();
    assert_eq!(qbit_like.submitted().len(), 1);
    assert_eq!(tr_like.submitted().len(), 1);
}

#[test]
fn transmission_completed_files_and_remove_work() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url,
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();
    let files = dl.completed_files(&torrent()).unwrap();
    assert_eq!(files[0], std::path::Path::new("/downloads/matrix.mkv"));
    dl.remove(&torrent(), true).unwrap();
    assert!(
        capture
            .methods
            .lock()
            .contains(&"torrent-remove".to_string())
    );
}

#[test]
fn transmission_handles_409_session_challenge() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let conflicts = Arc::new(AtomicUsize::new(0));
    let authorized = Arc::new(AtomicUsize::new(0));
    let server = {
        let (conflicts, authorized) = (conflicts.clone(), authorized.clone());
        FakeServer::start(move |request, stream| {
            let req = &request.raw;
            let has_token = req.lines().any(|l| {
                let lower = l.to_ascii_lowercase();
                lower.starts_with("x-transmission-session-id:") && lower.contains("test-token-409")
            });
            let (status, body) = if has_token {
                authorized.fetch_add(1, Ordering::SeqCst);
                (
                    "200 OK",
                    r#"{"result":"success","arguments":{"rpc-version":16,"torrents":[]}}"#,
                )
            } else {
                conflicts.fetch_add(1, Ordering::SeqCst);
                ("409 Conflict", "")
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nX-Transmission-Session-Id: test-token-409\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).ok();
        })
    };

    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url: format!("{}/transmission/rpc", server.url()),
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();
    assert_eq!(conflicts.load(Ordering::SeqCst), 1);
    let _ = dl.query_task_status("abcd", "tag").unwrap();
    assert_eq!(conflicts.load(Ordering::SeqCst), 1);
}

#[test]
fn debug_never_prints_the_password() {
    let config = TransmissionConfig {
        url: "http://127.0.0.1:9091".into(),
        username: Some("admin".into()),
        password: Some("transmission-password-secret".into()),
        path_maps: Vec::new(),
    };
    let rendered = format!("{config:?}");
    assert!(
        !rendered.contains("transmission-password-secret"),
        "Debug 不得输出 password: {rendered}"
    );
    assert!(rendered.contains("<redacted>"), "{rendered}");
    assert!(rendered.contains("127.0.0.1:9091"), "{rendered}");
    assert!(rendered.contains("admin"), "{rendered}");

    let anonymous = TransmissionConfig {
        url: "http://127.0.0.1:9091".into(),
        username: None,
        password: None,
        path_maps: Vec::new(),
    };
    assert!(
        format!("{anonymous:?}").contains("None"),
        "未配置口令应可诊断"
    );
}

#[test]
fn transmission_broken_foreign_task_returns_foreign_duplicate_when_labels_supported() {
    let capture = Capture::new();
    let parsed = parse_verified_torrent(SAMPLE_TORRENT).unwrap();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url,
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();
    assert!(dl.supports_labels());

    // Foreign broken task (labels = ["other"], error = 3) -> ForeignDuplicate
    *capture.injected_torrents.lock() = Some(format!(
        r#"{{"result":"success","arguments":{{"torrents":[{{"id":1,"hashString":"{}","error":3,"status":0,"labels":["other"]}}]}}}}"#,
        parsed.info_hash
    ));
    assert_eq!(
        dl.submit_verified(&sample_payload(), &default_options())
            .unwrap(),
        ClientSubmitResult::ForeignDuplicate {
            info_hash: parsed.info_hash.clone()
        }
    );

    // Instance-owned broken task (labels = ["cm-op:earlier"], error = 3) -> ExistingBroken
    *capture.injected_torrents.lock() = Some(format!(
        r#"{{"result":"success","arguments":{{"torrents":[{{"id":1,"hashString":"{}","error":3,"status":0,"labels":["cm-op:earlier"]}}]}}}}"#,
        parsed.info_hash
    ));
    assert_eq!(
        dl.submit_verified(&sample_payload(), &default_options())
            .unwrap(),
        ClientSubmitResult::ExistingBroken {
            info_hash: parsed.info_hash.clone()
        }
    );

    // RPC < 16 (no labels) broken task -> ExistingBroken
    let capture_v15 = Capture::new();
    *capture_v15.rpc_version.lock() = 15;
    *capture_v15.injected_torrents.lock() = Some(format!(
        r#"{{"result":"success","arguments":{{"torrents":[{{"id":1,"hashString":"{}","error":3,"status":0}}]}}}}"#,
        parsed.info_hash
    ));
    let (url_v15, _srv_v15) = serve(capture_v15, "/downloads".into());
    let dl_v15 = TransmissionDownloader::connect(TransmissionConfig {
        url: url_v15,
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();
    assert!(!dl_v15.supports_labels());
    assert_eq!(
        dl_v15
            .submit_verified(&sample_payload(), &default_options())
            .unwrap(),
        ClientSubmitResult::ExistingBroken {
            info_hash: parsed.info_hash
        }
    );
}

#[test]
fn transmission_submit_verified_zero_limits_disable_speed_caps() {
    let capture = Capture::new();
    *capture.injected_torrents.lock() =
        Some(r#"{"result":"success","arguments":{"torrents":[]}}"#.into());
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url,
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();

    let mut opts = default_options();
    opts.upload_limit = Some(0);
    opts.download_limit = Some(0);
    dl.submit_verified(&sample_payload(), &opts).unwrap();

    let set_body: serde_json::Value = {
        let requests = capture.requests.lock();
        let set_req = requests.iter().find(|r| r.contains("torrent-set")).unwrap();
        serde_json::from_str(set_req.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    };
    let args = &set_body["arguments"];
    assert_eq!(args["uploadLimited"], serde_json::json!(false));
    assert!(args.get("uploadLimit").is_none(), "{args}");
    assert_eq!(args["downloadLimited"], serde_json::json!(false));
    assert!(args.get("downloadLimit").is_none(), "{args}");
}

#[test]
fn transmission_submit_verified_rejects_comma_and_control_chars_in_tags() {
    let capture = Capture::new();
    *capture.injected_torrents.lock() =
        Some(r#"{"result":"success","arguments":{"torrents":[]}}"#.into());
    let (url, _server) = serve(capture, "/downloads".into());
    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url,
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();

    for (owner, extra) in [
        ("cm-op:2\n", vec![]),
        ("cm-op:2,injected", vec![]),
        ("cm-op:2", vec!["plugin:p:x,crawler-media-claimed".into()]),
        ("cm-op:2", vec!["plugin:p:bad\rtag".into()]),
    ] {
        let mut opts = default_options();
        opts.ownership_tag = owner.into();
        opts.extra_tags = extra;
        assert!(
            dl.submit_verified(&sample_payload(), &opts).is_err(),
            "expected submit_verified error for owner={owner:?}"
        );
    }
}
