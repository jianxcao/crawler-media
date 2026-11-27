#[path = "../../api/tests/support/lifecycle.rs"]
#[allow(dead_code)]
mod lifecycle;

use lifecycle::FakeServer;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;

use domain::{SiteId, Torrent};
use downloader::{
    ClientSubmitResult, Downloader, DownloaderError, QbitConfig, QbitDownloader, SubmitOptions,
    SubmitPayload, parse_verified_torrent,
};

struct Capture {
    adds: Mutex<Vec<Vec<u8>>>,
    deletes: Mutex<Vec<String>>,
    /// pause / resume / setPreferences / addTags requests, in arrival order.
    posts: Mutex<Vec<String>>,
    injected_infos: Mutex<Option<String>>,
    drop_after_add_headers: Mutex<bool>,
    fail_adds: AtomicBool,
}

impl Capture {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            adds: Mutex::new(Vec::new()),
            deletes: Mutex::new(Vec::new()),
            posts: Mutex::new(Vec::new()),
            injected_infos: Mutex::new(None),
            drop_after_add_headers: Mutex::new(false),
            fail_adds: AtomicBool::new(false),
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

fn match_fake_qbit_route(
    req: &str,
    raw_bytes: &[u8],
    capture: &Arc<Capture>,
    save_path: &str,
) -> (&'static str, String, &'static str) {
    if req.starts_with("POST /api/v2/auth/login") {
        (
            "200 OK",
            "Ok.".into(),
            "Set-Cookie: SID=test-sid; HttpOnly\r\n",
        )
    } else if req.starts_with("POST /api/v2/torrents/add") {
        capture.adds.lock().push(raw_bytes.to_vec());
        if capture.fail_adds.load(Ordering::SeqCst) {
            ("200 OK", "Fails.".into(), "")
        } else {
            ("200 OK", "Ok.".into(), "")
        }
    } else if req.starts_with("GET /api/v2/app/version") {
        ("200 OK", "v5.3.0".into(), "")
    } else if req.contains("/api/v2/torrents/info") {
        let injected = capture.injected_infos.lock().clone();
        let body = if let Some(custom) = injected {
            custom
        } else if capture.adds.lock().is_empty() {
            "[]".into()
        } else {
            let parsed = parse_verified_torrent(SAMPLE_TORRENT).unwrap();
            let tag = downloader::ownership_tag("https://pt.example/download.php?id=1");
            format!(
                r#"[{{"hash":"{}","name":"The.Matrix.1999.2160p.BluRay.x265-GROUP","progress":1.0,"save_path":"{save_path}","state":"stalledDL","tags":"other,crawler-media,{tag}","size":1000,"downloaded":1000,"uploaded":7,"dlspeed":128,"upspeed":64}}]"#,
                parsed.info_hash
            )
        };
        ("200 OK", body, "")
    } else if req.contains("/api/v2/torrents/files") {
        (
            "200 OK",
            r#"[{"name":"movie.mkv","progress":1.0}]"#.into(),
            "",
        )
    } else if req.contains("/api/v2/torrents/delete") {
        capture.deletes.lock().push(req.to_string());
        ("200 OK", "Ok.".into(), "")
    } else if req.contains("/api/v2/torrents/pause")
        || req.contains("/api/v2/torrents/resume")
        || req.contains("/api/v2/torrents/addTags")
        || req.contains("/api/v2/app/setPreferences")
    {
        capture.posts.lock().push(req.to_string());
        ("200 OK", "Ok.".into(), "")
    } else if req.contains("/api/v2/app/preferences") {
        ("200 OK", r#"{"dl_limit":1048576,"up_limit":0}"#.into(), "")
    } else {
        ("404 Not Found", "no".into(), "")
    }
}

fn serve(capture: Arc<Capture>, save_path: String) -> (String, FakeServer) {
    let server = FakeServer::start(move |request, stream| {
        if request.raw.starts_with("POST /api/v2/torrents/add")
            && *capture.drop_after_add_headers.lock()
        {
            capture.adds.lock().push(request.raw.as_bytes().to_vec());
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 500\r\nConnection: close\r\n\r\npartial",
            );
            return;
        }
        let (status, body, extra) =
            match_fake_qbit_route(&request.raw, request.raw.as_bytes(), &capture, &save_path);
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n{extra}\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
    });
    (server.url(), server)
}

fn connect(url: &str) -> QbitDownloader {
    QbitDownloader::connect(QbitConfig {
        url: url.into(),
        username: "admin".into(),
        password: "pass".into(),
        api_key: None,
        category: Some("crawler-media".into()),
        path_maps: vec![],
    })
    .unwrap()
}

fn torrent_at(enclosure: &str) -> Torrent {
    Torrent {
        site_id: SiteId::new(),
        title: "The.Matrix.1999.2160p.BluRay.x265-GROUP".into(),
        enclosure: enclosure.into(),
        size_bytes: Some(1000),
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
        free_2x: false,
        free_until: None,
    }
}

fn torrent() -> Torrent {
    torrent_at("https://pt.example/download.php?id=1")
}

#[test]
fn submit_verified_uploads_exact_torrent_bytes_without_urls_or_temp_leaks() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = connect(&url);

    let mut opts = default_options();
    opts.ownership_tag = "cm-op:qbit-temp-leak-test".into();
    opts.save_path = Some("/downloads/manual".into());
    opts.extra_tags = vec!["plugin:p1:tag".into()];
    opts.upload_limit = Some(2048);
    opts.download_limit = Some(4096);
    opts.sequential = true;
    opts.first_last_piece = true;

    let res = dl.submit_verified(&sample_payload(), &opts).unwrap();
    assert!(matches!(
        res,
        ClientSubmitResult::Added {
            already_owned: false,
            ..
        }
    ));

    let adds = capture.adds.lock();
    assert_eq!(adds.len(), 1);
    let raw = String::from_utf8_lossy(&adds[0]);
    assert!(raw.contains("name=\"torrents\""));
    assert!(!raw.contains("name=\"urls\""));
    assert!(
        adds[0]
            .windows(SAMPLE_TORRENT.len())
            .any(|w| w == SAMPLE_TORRENT)
    );
    assert!(raw.contains("name=\"savepath\"\r\n\r\n/downloads/manual"));
    assert!(raw.contains("name=\"paused\"\r\n\r\nfalse"));
    assert!(raw.contains("name=\"stopped\"\r\n\r\nfalse"));
    assert!(raw.contains(downloader::TASK_TAG));
    assert!(raw.contains("plugin:p1:tag"));
    assert!(raw.contains("name=\"upLimit\"\r\n\r\n2048"));
    assert!(raw.contains("name=\"dlLimit\"\r\n\r\n4096"));
    assert!(raw.contains("name=\"sequentialDownload\"\r\n\r\ntrue"));
    assert!(raw.contains("name=\"firstLastPiecePrio\"\r\n\r\ntrue"));

    let leaked: Vec<_> = std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let s = name.to_string_lossy();
            s.starts_with(&format!("cm-torrent-{}-", std::process::id()))
                && s.contains("qbit-temp-leak-test")
        })
        .collect();
    assert!(leaked.is_empty(), "temp file leaked: {leaked:?}");
}

#[test]
fn submit_verified_handles_post_send_body_loss_foreign_duplicate_and_broken() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = connect(&url);
    let payload = sample_payload();
    let parsed = parse_verified_torrent(SAMPLE_TORRENT).unwrap();

    // 1. Post-send truncated response returns UnknownOutcome.
    *capture.drop_after_add_headers.lock() = true;
    let unknown = dl.submit_verified(&payload, &default_options()).unwrap();
    assert!(matches!(unknown, ClientSubmitResult::UnknownOutcome { .. }));
    *capture.drop_after_add_headers.lock() = false;
    capture.adds.lock().clear();

    // 2. Foreign task with same info_hash returns ForeignDuplicate with ZERO addTags/resume calls.
    *capture.injected_infos.lock() = Some(format!(
        r#"[{{"hash":"{}","name":"Foreign","progress":0.0,"save_path":"/downloads","state":"pausedDL","tags":"other","size":1000}}]"#,
        parsed.info_hash
    ));
    let foreign = dl.submit_verified(&payload, &default_options()).unwrap();
    assert_eq!(
        foreign,
        ClientSubmitResult::ForeignDuplicate {
            info_hash: parsed.info_hash.clone()
        }
    );
    assert!(capture.adds.lock().is_empty());
    assert!(capture.posts.lock().is_empty());

    // 3. Existing broken task returns ExistingBroken.
    *capture.injected_infos.lock() = Some(format!(
        r#"[{{"hash":"{}","name":"Broken","progress":0.2,"save_path":"/downloads","state":"missingFiles","tags":"{}","size":1000}}]"#,
        parsed.info_hash,
        default_options().ownership_tag
    ));
    let broken = dl.submit_verified(&payload, &default_options()).unwrap();
    assert_eq!(
        broken,
        ClientSubmitResult::ExistingBroken {
            info_hash: parsed.info_hash.clone()
        }
    );

    let q = dl
        .query_task_status(&parsed.info_hash, &default_options().ownership_tag)
        .unwrap();
    assert!(q.exists);
    assert!(q.owned_by_tag);
    assert!(q.broken);
}

#[test]
fn submit_verified_is_idempotent_for_owned_task_and_exposes_files() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("movie.mkv");
    std::fs::write(&file, b"bytes").unwrap();
    let capture = Capture::new();
    let (url, _server) = serve(
        capture.clone(),
        dir.path().display().to_string().replace('\\', "/"),
    );
    let dl = connect(&url);

    let payload = sample_payload();
    let opts = default_options();
    assert!(matches!(
        dl.submit_verified(&payload, &opts).unwrap(),
        ClientSubmitResult::Added {
            already_owned: false,
            ..
        }
    ));
    assert!(matches!(
        dl.submit_verified(&payload, &opts).unwrap(),
        ClientSubmitResult::Added {
            already_owned: true,
            ..
        }
    ));
    assert_eq!(capture.adds.lock().len(), 1);
    let files = dl.completed_files(&torrent()).unwrap();
    assert_eq!(files, vec![file]);
}

#[test]
fn remove_deletes_task_by_hash_keeping_files() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = connect(&url);

    dl.submit_verified(&sample_payload(), &default_options())
        .unwrap();
    dl.remove(&torrent(), false).unwrap();

    let parsed = parse_verified_torrent(SAMPLE_TORRENT).unwrap();
    let deletes = capture.deletes.lock();
    assert_eq!(deletes.len(), 1);
    assert!(deletes[0].contains(&format!("hashes={}", parsed.info_hash)));
    assert!(deletes[0].contains("deleteFiles=false"));
}

#[test]
fn delete_pause_resume_and_speed_limits_work() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = connect(&url);

    dl.delete_task("deadbeef", true).unwrap();
    dl.pause_task("abc").unwrap();
    dl.resume_task("abc").unwrap();
    let (dl_limit, ul_limit) = dl.get_speed_limits().unwrap();
    assert_eq!((dl_limit, ul_limit), (1_048_576, 0));
    dl.set_speed_limits(2_097_152, 524_288).unwrap();

    assert_eq!(capture.deletes.lock().len(), 1);
    assert_eq!(capture.posts.lock().len(), 3);
}

#[test]
fn task_snapshots_extract_our_tag_and_live_state() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = connect(&url);
    dl.submit_verified(&sample_payload(), &default_options())
        .unwrap();

    let snapshots = dl.task_snapshots().unwrap();
    assert_eq!(snapshots.len(), 1);
    let snapshot = &snapshots[0];
    assert_eq!(snapshot.tag, "crawler-media");
    assert_eq!(
        snapshot.owned_tags,
        vec![downloader::ownership_tag(
            "https://pt.example/download.php?id=1"
        )]
    );
    assert_eq!(snapshot.state, "stalledDL");

    // Detailed telemetry fields from qBittorrent /api/v2/torrents/info
    *capture.injected_infos.lock() = Some(
        r#"[{"hash":"aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111","name":"Show.S01","progress":1.0,"save_path":"/downloads/boost","state":"uploading","category":"boost","tags":"crawler-media,cm-op:op1,plugin:cm.boost:auto,站点-Audiences","size":2000,"downloaded":2000,"uploaded":6000,"dlspeed":0,"upspeed":4096,"ratio":3.0,"seeding_time":7200,"added_on":1700000000,"completion_on":1700000600,"last_activity":1700007800}]"#
            .into(),
    );
    let rich = dl.task_snapshots().unwrap();
    assert_eq!(rich.len(), 1);
    let item = &rich[0];
    assert!((item.ratio - 3.0).abs() < f64::EPSILON);
    assert_eq!(item.seeding_secs, 7200);
    assert_eq!(item.added_at, Some(1_700_000_000));
    assert_eq!(item.completed_at, Some(1_700_000_600));
    assert_eq!(item.last_activity_at, Some(1_700_007_800));
    assert_eq!(item.category.as_deref(), Some("boost"));
    assert_eq!(
        item.user_tags,
        vec![
            "plugin:cm.boost:auto".to_string(),
            "站点-Audiences".to_string()
        ]
    );
}

#[test]
fn same_size_or_different_hash_unrelated_torrent_does_not_match() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = connect(&url);

    *capture.injected_infos.lock() = Some(r#"[{"hash":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","name":"Other.Movie.2024.1080p","progress":1.0,"save_path":"/downloads","state":"uploading","tags":"crawler-media","size":1000}]"#.to_string());

    let mut target = torrent();
    target.enclosure = "magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into();
    assert!(dl.completed_files(&target).unwrap().is_empty());
    dl.remove(&target, true).unwrap();
    assert!(capture.deletes.lock().is_empty());
}

#[test]
fn api_key_auth_skips_cookie_login_and_submits_with_bearer_header() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = QbitDownloader::connect(QbitConfig {
        url,
        username: String::new(),
        password: String::new(),
        api_key: Some("qbt_test_bearer_key".into()),
        category: Some("crawler-media".into()),
        path_maps: vec![],
    })
    .unwrap();

    let res = dl
        .submit_verified(&sample_payload(), &default_options())
        .unwrap();
    assert!(matches!(
        res,
        ClientSubmitResult::Added {
            already_owned: false,
            ..
        }
    ));
    let adds = capture.adds.lock();
    assert_eq!(adds.len(), 1);
    let raw = String::from_utf8_lossy(&adds[0]);
    assert!(
        raw.to_ascii_lowercase()
            .contains("authorization: bearer qbt_test_bearer_key"),
        "expected Bearer auth header, got: {raw}"
    );
}

#[test]
fn qbit_fails_reply_is_transient() {
    let capture = Capture::new();
    capture.fail_adds.store(true, Ordering::SeqCst);
    let (url, _server) = serve(capture, "/downloads".into());
    let dl = connect(&url);

    let err = dl
        .submit_verified(&sample_payload(), &default_options())
        .unwrap_err();
    assert!(matches!(err, DownloaderError::Message(_)), "{err:?}");
}

#[test]
fn broken_foreign_task_returns_foreign_duplicate_before_existing_broken() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = connect(&url);
    let payload = sample_payload();
    let parsed = parse_verified_torrent(SAMPLE_TORRENT).unwrap();

    for (state, tags) in [("error", "user-tag"), ("missingFiles", "")] {
        *capture.injected_infos.lock() = Some(format!(
            r#"[{{"hash":"{}","name":"UserTask","progress":0.1,"save_path":"/downloads","state":"{state}","tags":"{tags}","size":1000}}]"#,
            parsed.info_hash
        ));
        let res = dl.submit_verified(&payload, &default_options()).unwrap();
        assert_eq!(
            res,
            ClientSubmitResult::ForeignDuplicate {
                info_hash: parsed.info_hash.clone()
            },
            "foreign broken task ({state}, tags={tags:?}) must return ForeignDuplicate"
        );
    }

    for instance_tag in [downloader::TASK_TAG, "cm-op:earlier-op"] {
        *capture.injected_infos.lock() = Some(format!(
            r#"[{{"hash":"{}","name":"OurBroken","progress":0.1,"save_path":"/downloads","state":"error","tags":"{instance_tag}","size":1000}}]"#,
            parsed.info_hash
        ));
        let res = dl.submit_verified(&payload, &default_options()).unwrap();
        assert_eq!(
            res,
            ClientSubmitResult::ExistingBroken {
                info_hash: parsed.info_hash.clone()
            },
            "instance-owned broken task ({instance_tag}) must return ExistingBroken"
        );
    }

    let tmp = tempfile::tempdir().unwrap();
    let mem = downloader::MemoryDownloader::new(tmp.path());
    mem.inject_foreign_hash(&parsed.info_hash);
    mem.inject_broken_hash(&parsed.info_hash);
    assert_eq!(
        mem.submit_verified(&payload, &default_options()).unwrap(),
        ClientSubmitResult::ForeignDuplicate {
            info_hash: parsed.info_hash
        }
    );
}
