#[path = "../../api/tests/support/lifecycle.rs"]
#[allow(dead_code)]
mod lifecycle;

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use downloader::{
    DiskSpaceStatus, Downloader, MemoryDownloader, PathMap, QbitConfig, QbitDownloader,
    SubmitOptions, SubmitPayload, TransmissionConfig, TransmissionDownloader,
    parse_verified_torrent,
};
use lifecycle::FakeServer;
use parking_lot::Mutex;

const SAMPLE_TORRENT: &[u8] =
    b"d8:announce10:https://pt4:infod6:lengthi1024e4:name9:movie.mkv12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaaee";

fn sample_payload() -> SubmitPayload {
    let parsed = parse_verified_torrent(SAMPLE_TORRENT).unwrap();
    SubmitPayload::TorrentBytes {
        bytes: parsed.bytes,
        bytes_digest: parsed.bytes_digest,
        info_hash: parsed.info_hash,
    }
}

#[test]
fn memory_downloader_supports_task_files_controls_and_disk_space() {
    let dir = tempfile::tempdir().unwrap();
    let dl = MemoryDownloader::new(dir.path().join("stage"));
    let payload = sample_payload();
    let hash = payload.info_hash().to_string();
    dl.submit_verified(
        &payload,
        &SubmitOptions {
            ownership_tag: "cm-op:1".into(),
            save_path: Some("/dl/boost".into()),
            category: Some("boost".into()),
            extra_tags: vec!["site:1".into()],
            paused: false,
            ..Default::default()
        },
    )
    .unwrap();
    let f1 = dir.path().join("stage/movie.mkv");
    std::fs::write(&f1, vec![0u8; 1024]).unwrap();
    dl.set_task_files(&hash, vec![f1.clone()], 1024);

    let files = dl.task_files_by_hash(&hash).unwrap().unwrap();
    assert_eq!(files.files, vec![f1]);
    assert_eq!(files.total_bytes, 1024);
    assert!(!files.claimed_by_app);

    dl.pause_task(&hash).unwrap();
    assert!(dl.is_task_paused(&hash));
    dl.resume_task(&hash).unwrap();
    assert!(!dl.is_task_paused(&hash));

    dl.set_task_tags(&hash, &["tag-a".into(), "tag-b".into()])
        .unwrap();
    assert_eq!(dl.task_tags(&hash), vec!["tag-a", "tag-b"]);

    dl.set_task_category(&hash, "boost-2").unwrap();
    assert_eq!(dl.task_category(&hash).as_deref(), Some("boost-2"));

    dl.set_task_limits(&hash, Some(1024), Some(2048)).unwrap();
    assert_eq!(dl.task_limits(&hash), (Some(1024), Some(2048)));

    let free = dl.disk_free_space().unwrap();
    assert_eq!(free.status, DiskSpaceStatus::Available);
    assert!(free.available_bytes.unwrap() > 0);

    dl.set_disk_space(DiskSpaceStatus::Unknown, None);
    assert_eq!(
        dl.disk_free_space().unwrap().status,
        DiskSpaceStatus::Unknown
    );

    dl.delete_task(&hash, false).unwrap();
    assert_eq!(dl.deleted_tasks(), vec![(hash.clone(), false)]);
    assert!(dl.task_files_by_hash(&hash).unwrap().is_none());
}

#[test]
fn memory_downloader_remember_identity_remove_owned_allows_resubmit_and_preserves_system_tags() {
    let dir = tempfile::tempdir().unwrap();
    let dl = MemoryDownloader::new(dir.path().join("stage"));
    let payload = sample_payload();
    let hash = payload.info_hash().to_string();
    let torrent = domain::Torrent {
        site_id: domain::SiteId::new(),
        id: Some("1".into()),
        title: "Sample.Movie.2025.1080p".into(),
        enclosure: "https://pt.example/download.php?id=42".into(),
        size_bytes: Some(1024),
        seeders: Some(5),
        leechers: None,
        snatched: None,
        upload_time: None,
        poster_url: None,
        free: true,
        hr: Some(false),
        imdb_id: None,
        detail_url: None,
        category: None,
        free_2x: false,
        free_until: None,
    };

    assert_eq!(
        dl.owned_identity(&torrent).unwrap(),
        Some(format!("memory:{}", torrent.enclosure))
    );

    let enc_tag = downloader::ownership_tag(&torrent.enclosure);
    let r1 = dl
        .submit_verified(
            &payload,
            &SubmitOptions {
                ownership_tag: "cm-op:op-1".into(),
                extra_tags: vec![
                    downloader::TASK_TAG.into(),
                    downloader::CLAIMED_TAG.into(),
                    enc_tag.clone(),
                    "cm-op:op-1".into(),
                    "cm-sub:sub-1".into(),
                    "old-user-tag".into(),
                ],
                ..Default::default()
            },
        )
        .unwrap();
    assert!(matches!(
        r1,
        downloader::ClientSubmitResult::Added {
            already_owned: false,
            ..
        }
    ));
    dl.remember_identity(&torrent, &hash.to_ascii_uppercase())
        .unwrap();
    assert_eq!(dl.owned_identity(&torrent).unwrap(), Some(hash.clone()));

    dl.set_task_tags(&hash, &["caller-tag".into(), "cm-op:op-1".into()])
        .unwrap();
    assert_eq!(
        dl.task_tags(&hash),
        vec![
            downloader::TASK_TAG.to_string(),
            downloader::CLAIMED_TAG.to_string(),
            enc_tag.clone(),
            "cm-op:op-1".to_string(),
            "cm-sub:sub-1".to_string(),
            "caller-tag".to_string(),
        ]
    );

    dl.remove_owned(&torrent, false).unwrap();
    assert_eq!(
        dl.owned_identity(&torrent).unwrap(),
        Some(format!("memory:{}", torrent.enclosure))
    );

    let r2 = dl
        .submit_verified(
            &payload,
            &SubmitOptions {
                ownership_tag: "cm-op:op-2".into(),
                extra_tags: vec![enc_tag],
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        matches!(
            r2,
            downloader::ClientSubmitResult::Added {
                already_owned: false,
                ..
            }
        ),
        "re-submitting after remove_owned must succeed instead of ForeignDuplicate: {r2:?}"
    );
    assert_eq!(dl.owned_identity(&torrent).unwrap(), Some(hash.clone()));

    dl.delete_task(&hash, false).unwrap();
    assert_eq!(
        dl.owned_identity(&torrent).unwrap(),
        Some(format!("memory:{}", torrent.enclosure))
    );
}

#[derive(Default)]
struct QbitCapture {
    calls: Mutex<Vec<String>>,
    maindata_error: Mutex<bool>,
    free_space: Mutex<Option<u64>>,
    category_exists: Mutex<bool>,
}

fn qbit_route(
    req: &str,
    cap: &Arc<QbitCapture>,
    hash: &str,
    save_path: &str,
) -> (&'static str, String, &'static str) {
    if req.starts_with("POST /api/v2/auth/login") {
        return ("200 OK", "Ok.".into(), "Set-Cookie: SID=s1; HttpOnly\r\n");
    }
    cap.calls.lock().push(req.to_string());
    if req.contains("/api/v2/torrents/info") {
        return (
            "200 OK",
            format!(
                r#"[{{"hash":"{hash}","name":"movie.mkv","progress":0.5,"save_path":"{save_path}","tags":"crawler-media,cm-op:1,old-tag","category":"old-cat","size":1024}}]"#
            ),
            "",
        );
    }
    if req.contains("/api/v2/torrents/files") {
        return (
            "200 OK",
            r#"[{"name":"movie.mkv","progress":0.5,"priority":1,"size":1024}]"#.into(),
            "",
        );
    }
    if req.contains("/api/v2/sync/maindata") {
        if *cap.maindata_error.lock() {
            return ("500 Internal Server Error", "err".into(), "");
        }
        let body = match *cap.free_space.lock() {
            Some(bytes) => {
                format!(r#"{{"rid":1,"server_state":{{"free_space_on_disk":{bytes}}}}}"#)
            }
            None => r#"{"rid":1,"server_state":{}}"#.to_string(),
        };
        return ("200 OK", body, "");
    }
    if req.contains("/api/v2/torrents/setCategory") && !*cap.category_exists.lock() {
        return ("409 Conflict", "missing category".into(), "");
    }
    if req.contains("/api/v2/torrents/createCategory") {
        *cap.category_exists.lock() = true;
        return ("200 OK", "Ok.".into(), "");
    }
    ("200 OK", "Ok.".into(), "")
}

#[test]
fn qbit_downloader_boost_ops_wire_protocol() {
    let cap = Arc::new(QbitCapture::default());
    *cap.free_space.lock() = Some(9_000_000);
    let hash = sample_payload().info_hash().to_string();
    let hash_srv = hash.clone();
    let cap_srv = cap.clone();
    let server = FakeServer::start(move |req, stream| {
        let (status, body, extra) = qbit_route(&req.raw, &cap_srv, &hash_srv, "/container/dl");
        let resp = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let qb = QbitDownloader::connect(QbitConfig {
        url: format!("http://{}", server.addr()),
        username: "admin".into(),
        password: "pw".into(),
        api_key: None,
        category: None,
        path_maps: vec![PathMap {
            from: "/container/dl".into(),
            to: PathBuf::from("/host/dl"),
        }],
    })
    .unwrap();

    let files = qb.task_files_by_hash(&hash).unwrap().unwrap();
    assert_eq!(files.files, vec![PathBuf::from("/host/dl/movie.mkv")]);
    assert_eq!(files.total_bytes, 1024);

    qb.set_task_tags(&hash, &["new-tag".into()]).unwrap();
    qb.set_task_category(&hash, "boost-cat").unwrap();
    qb.set_task_limits(&hash, Some(5000), Some(10000)).unwrap();

    let disk = qb.disk_free_space().unwrap();
    assert_eq!(disk.status, DiskSpaceStatus::Available);
    assert_eq!(disk.available_bytes, Some(9_000_000));

    *cap.maindata_error.lock() = true;
    assert!(qb.disk_free_space().is_err());

    let calls = cap.calls.lock().join("\n");
    assert!(calls.contains("/api/v2/torrents/addTags"), "{calls}");
    assert!(calls.contains("/api/v2/torrents/removeTags"), "{calls}");
    assert!(calls.contains("/api/v2/torrents/createCategory"), "{calls}");
    assert!(calls.contains("/api/v2/torrents/setCategory"), "{calls}");
    assert!(calls.contains("/api/v2/torrents/setUploadLimit"), "{calls}");
    assert!(
        calls.contains("/api/v2/torrents/setDownloadLimit"),
        "{calls}"
    );
}

#[test]
fn transmission_downloader_boost_ops_wire_protocol() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let calls_srv = calls.clone();
    let hash = sample_payload().info_hash().to_string();
    let hash_srv = hash.clone();
    let server = FakeServer::start(move |req, stream| {
        calls_srv.lock().push(req.raw.clone());
        let body = if req.raw.contains("session-get") {
            r#"{"result":"success","arguments":{"rpc-version":17,"download-dir":"/tr/dl"}}"#
                .to_string()
        } else if req.raw.contains("free-space") {
            r#"{"result":"success","arguments":{"path":"/tr/dl","size-bytes":12345678}}"#
                .to_string()
        } else if req.raw.contains("torrent-get") {
            format!(
                r#"{{"result":"success","arguments":{{"torrents":[{{"id":1,"hashString":"{hash_srv}","downloadDir":"/tr/dl","totalSize":1024,"labels":["crawler-media","cm-op:1"],"files":[{{"name":"movie.mkv","length":1024,"bytesCompleted":512}}],"fileStats":[{{"wanted":true}}]}}]}}}}"#
            )
        } else {
            r#"{"result":"success","arguments":{}}"#.to_string()
        };
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let tr = TransmissionDownloader::connect(TransmissionConfig {
        url: format!("http://{}/transmission/rpc", server.addr()),
        username: None,
        password: None,
        path_maps: vec![PathMap {
            from: "/tr/dl".into(),
            to: PathBuf::from("/host/tr"),
        }],
    })
    .unwrap();

    let files = tr.task_files_by_hash(&hash).unwrap().unwrap();
    assert_eq!(files.files, vec![PathBuf::from("/host/tr/movie.mkv")]);
    assert_eq!(files.total_bytes, 1024);

    tr.set_task_tags(&hash, &["tr-tag".into()]).unwrap();
    tr.set_task_limits(&hash, Some(4096), None).unwrap();
    let disk = tr.disk_free_space().unwrap();
    assert_eq!(disk.status, DiskSpaceStatus::Available);
    assert_eq!(disk.available_bytes, Some(12_345_678));

    tr.delete_task(&hash, false).unwrap();
    let joined = calls.lock().join("\n");
    assert!(joined.contains("free-space"), "{joined}");
    assert!(joined.contains("torrent-set"), "{joined}");
    assert!(joined.contains("\"delete-local-data\":false"), "{joined}");
}

#[test]
fn memory_and_qbit_and_transmission_reject_comma_control_and_unowned_system_tags() {
    let dir = tempfile::tempdir().unwrap();
    let mem = MemoryDownloader::new(dir.path().join("stage"));
    let payload = sample_payload();
    let hash = payload.info_hash().to_string();
    mem.submit_verified(
        &payload,
        &SubmitOptions {
            ownership_tag: "cm-op:1".into(),
            extra_tags: vec!["user-a".into()],
            ..Default::default()
        },
    )
    .unwrap();
    for bad in [
        "ok,crawler-media-claimed",
        "bad\ntag",
        downloader::TASK_TAG,
        downloader::CLAIMED_TAG,
        "crawler-media-owned-forged",
        "cm-op:forged",
        "cm-sub:forged",
    ] {
        assert!(
            matches!(
                mem.set_task_tags(&hash, &[bad.into()]),
                Err(downloader::DownloaderError::Message(_))
            ),
            "memory set_task_tags must reject `{bad}`"
        );
    }
    mem.set_task_tags(&hash, &["cm-op:1".into(), "user-b".into()])
        .unwrap();

    let cap = Arc::new(QbitCapture::default());
    let hash_qb = hash.clone();
    let cap_srv = cap.clone();
    let qb_srv = FakeServer::start(move |req, stream| {
        let (status, body, extra) =
            if req.raw.contains("/api/v2/torrents/info") && !req.raw.contains(&hash_qb) {
                ("200 OK", "[]".to_string(), "")
            } else {
                qbit_route(&req.raw, &cap_srv, &hash_qb, "/container/dl")
            };
        let resp = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });
    let qb = QbitDownloader::connect(QbitConfig {
        url: format!("http://{}", qb_srv.addr()),
        username: "admin".into(),
        password: "pw".into(),
        api_key: None,
        category: None,
        path_maps: vec![],
    })
    .unwrap();

    let magnet = SubmitPayload::Magnet {
        uri: "magnet:?xt=urn:btih:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
        info_hash: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
    };
    assert!(matches!(
        qb.submit_verified(
            &magnet,
            &SubmitOptions {
                ownership_tag: "cm-op:2".into(),
                extra_tags: vec!["plugin:p:x,crawler-media-claimed".into()],
                ..Default::default()
            }
        ),
        Err(downloader::DownloaderError::Message(_))
    ));
    assert!(matches!(
        qb.submit_verified(
            &magnet,
            &SubmitOptions {
                ownership_tag: "cm-op:2\n".into(),
                ..Default::default()
            }
        ),
        Err(downloader::DownloaderError::Message(_))
    ));
    for bad in [
        "ok,crawler-media-claimed",
        "bad\0tag",
        downloader::CLAIMED_TAG,
        "crawler-media-owned-forged",
        "cm-op:forged",
        "cm-sub:forged",
    ] {
        assert!(
            matches!(
                qb.set_task_tags(&hash, &[bad.into()]),
                Err(downloader::DownloaderError::Message(_))
            ),
            "qbit set_task_tags must reject `{bad}`"
        );
    }
    qb.set_task_tags(&hash, &["cm-op:1".into(), "qb-ok".into()])
        .unwrap();
}

#[test]
fn transmission_set_task_tags_rejects_comma_control_and_unowned_system_tags() {
    let hash = sample_payload().info_hash().to_string();
    let hash_srv = hash.clone();
    let server = FakeServer::start(move |req, stream| {
        let body = if req.raw.contains("session-get") {
            r#"{"result":"success","arguments":{"rpc-version":17,"download-dir":"/tr/dl"}}"#
                .to_string()
        } else if req.raw.contains("torrent-get") {
            format!(
                r#"{{"result":"success","arguments":{{"torrents":[{{"id":1,"hashString":"{hash_srv}","downloadDir":"/tr/dl","totalSize":1024,"labels":["crawler-media","cm-op:1"],"files":[]}}]}}}}"#
            )
        } else {
            r#"{"result":"success","arguments":{}}"#.to_string()
        };
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });
    let tr = TransmissionDownloader::connect(TransmissionConfig {
        url: format!("http://{}/transmission/rpc", server.addr()),
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();
    for bad in [
        "ok,crawler-media-claimed",
        "bad\ttag",
        downloader::CLAIMED_TAG,
        "crawler-media-owned-forged",
        "cm-op:forged",
        "cm-sub:forged",
    ] {
        assert!(
            matches!(
                tr.set_task_tags(&hash, &[bad.into()]),
                Err(downloader::DownloaderError::Message(_))
            ),
            "transmission set_task_tags must reject `{bad}`"
        );
    }
    tr.set_task_tags(&hash, &["cm-op:1".into(), "tr-ok".into()])
        .unwrap();
}

#[test]
fn submit_verified_exact_owned_merges_extra_tags_for_memory_and_qbit() {
    let payload = sample_payload();
    let hash = payload.info_hash().to_string();
    let torrent_2 = domain::Torrent {
        site_id: domain::SiteId::new(),
        id: Some("2".into()),
        title: "Sample.Movie.2025.1080p".into(),
        enclosure: "https://pt.example/download.php?id=999".into(),
        size_bytes: Some(1024),
        seeders: Some(5),
        leechers: None,
        snatched: None,
        upload_time: None,
        poster_url: None,
        free: true,
        hr: Some(false),
        imdb_id: None,
        detail_url: None,
        category: None,
        free_2x: false,
        free_until: None,
    };
    let enc_tag_1 = downloader::ownership_tag("https://pt.example/download.php?id=111");
    let enc_tag_2 = downloader::ownership_tag(&torrent_2.enclosure);
    let resubmit_opts = SubmitOptions {
        ownership_tag: "cm-op:1".into(),
        extra_tags: vec!["cm-sub:sub-2".into(), enc_tag_2.clone()],
        ..Default::default()
    };

    let dir = tempfile::tempdir().unwrap();
    let mem = MemoryDownloader::new(dir.path().join("stage"));
    mem.submit_verified(
        &payload,
        &SubmitOptions {
            ownership_tag: "cm-op:1".into(),
            extra_tags: vec![enc_tag_1.clone()],
            ..Default::default()
        },
    )
    .unwrap();
    let mem_res = mem.submit_verified(&payload, &resubmit_opts).unwrap();
    assert!(matches!(
        mem_res,
        downloader::ClientSubmitResult::Added {
            already_owned: true,
            ..
        }
    ));
    assert_eq!(mem.owned_identity(&torrent_2).unwrap(), Some(hash.clone()));
    assert!(
        mem.task_snapshots().unwrap()[0]
            .owned_tags
            .contains(&enc_tag_2)
    );

    let qb_tags = Arc::new(Mutex::new(format!("crawler-media,cm-op:1,{enc_tag_1}")));
    let qb_tags_srv = qb_tags.clone();
    let hash_qb = hash.clone();
    let qb_srv = FakeServer::start(move |req, stream| {
        let (status, body, extra) = if req.raw.starts_with("POST /api/v2/auth/login") {
            ("200 OK", "Ok.".to_string(), "Set-Cookie: SID=s1\r\n")
        } else if req.raw.contains("/api/v2/torrents/addTags") {
            if let Some((_, form)) = req.raw.split_once("\r\n\r\n") {
                for pair in form.split('&') {
                    if let Some(val) = pair.strip_prefix("tags=") {
                        let decoded = val.replace("%3A", ":").replace("%2C", ",");
                        qb_tags_srv.lock().push_str(&format!(",{decoded}"));
                    }
                }
            }
            ("200 OK", "Ok.".to_string(), "")
        } else if req.raw.contains("/api/v2/torrents/info") {
            let cur = qb_tags_srv.lock().clone();
            (
                "200 OK",
                format!(
                    r#"[{{"hash":"{hash_qb}","name":"movie.mkv","progress":1.0,"state":"uploading","save_path":"/dl","tags":"{cur}","size":1024}}]"#
                ),
                "",
            )
        } else if req.raw.contains("/api/v2/torrents/files") {
            (
                "200 OK",
                r#"[{"name":"movie.mkv","progress":1.0,"priority":1,"size":1024}]"#.to_string(),
                "",
            )
        } else {
            ("200 OK", "Ok.".to_string(), "")
        };
        let resp = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });
    let qb = QbitDownloader::connect(QbitConfig {
        url: format!("http://{}", qb_srv.addr()),
        username: "admin".into(),
        password: "pw".into(),
        api_key: None,
        category: None,
        path_maps: vec![],
    })
    .unwrap();
    let qb_res = qb.submit_verified(&payload, &resubmit_opts).unwrap();
    assert!(matches!(
        qb_res,
        downloader::ClientSubmitResult::Added {
            already_owned: true,
            ..
        }
    ));
    assert_eq!(
        qb.completed_files(&torrent_2).unwrap(),
        vec![PathBuf::from("/dl/movie.mkv")]
    );
}

#[test]
fn submit_verified_exact_owned_merges_extra_tags_for_transmission() {
    let payload = sample_payload();
    let hash = payload.info_hash().to_string();
    let torrent_2 = domain::Torrent {
        site_id: domain::SiteId::new(),
        id: Some("2".into()),
        title: "Sample.Movie.2025.1080p".into(),
        enclosure: "https://pt.example/download.php?id=999".into(),
        size_bytes: Some(1024),
        seeders: Some(5),
        leechers: None,
        snatched: None,
        upload_time: None,
        poster_url: None,
        free: true,
        hr: Some(false),
        imdb_id: None,
        detail_url: None,
        category: None,
        free_2x: false,
        free_until: None,
    };
    let enc_tag_1 = downloader::ownership_tag("https://pt.example/download.php?id=111");
    let enc_tag_2 = downloader::ownership_tag(&torrent_2.enclosure);
    let resubmit_opts = SubmitOptions {
        ownership_tag: "cm-op:1".into(),
        extra_tags: vec!["cm-sub:sub-2".into(), enc_tag_2],
        ..Default::default()
    };

    let tr_labels = Arc::new(Mutex::new(vec![
        "crawler-media".to_string(),
        "cm-op:1".to_string(),
        enc_tag_1,
    ]));
    let tr_labels_srv = tr_labels.clone();
    let hash_tr = hash.clone();
    let tr_srv = FakeServer::start(move |req, stream| {
        let body = if req.raw.contains("session-get") {
            r#"{"result":"success","arguments":{"rpc-version":17,"download-dir":"/tr/dl"}}"#
                .to_string()
        } else if req.raw.contains("torrent-set") {
            if let Some((_, json_str)) = req.raw.split_once("\r\n\r\n")
                && let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str)
                && let Some(arr) = v["arguments"]["labels"].as_array()
            {
                *tr_labels_srv.lock() = arr
                    .iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect();
            }
            r#"{"result":"success","arguments":{}}"#.to_string()
        } else if req.raw.contains("torrent-get") {
            let cur = serde_json::to_string(&*tr_labels_srv.lock()).unwrap();
            format!(
                r#"{{"result":"success","arguments":{{"torrents":[{{"id":1,"hashString":"{hash_tr}","status":6,"error":0,"percentDone":1.0,"downloadDir":"/tr/dl","totalSize":1024,"labels":{cur},"files":[{{"name":"movie.mkv","length":1024,"bytesCompleted":1024}}],"fileStats":[{{"wanted":true}}]}}]}}}}"#
            )
        } else {
            r#"{"result":"success","arguments":{}}"#.to_string()
        };
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });
    let tr = TransmissionDownloader::connect(TransmissionConfig {
        url: format!("http://{}/transmission/rpc", tr_srv.addr()),
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();
    let tr_res = tr.submit_verified(&payload, &resubmit_opts).unwrap();
    assert!(matches!(
        tr_res,
        downloader::ClientSubmitResult::Added {
            already_owned: true,
            ..
        }
    ));
    assert_eq!(tr.owned_identity(&torrent_2).unwrap(), Some(hash));
}
