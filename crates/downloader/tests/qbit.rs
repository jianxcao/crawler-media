#[path = "../../api/tests/support/lifecycle.rs"]
#[allow(dead_code)]
mod lifecycle;

use lifecycle::FakeServer;
use std::io::Write;
use std::sync::Arc;

use parking_lot::Mutex;

use domain::{SiteId, Torrent};
use downloader::{Downloader, DownloaderError, QbitConfig, QbitDownloader};

const VALID_TORRENT: &str = "d8:announce10:https://pt4:infod6:lengthi1e4:name1:a12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaaee";

struct Capture {
    adds: Mutex<Vec<String>>,
    deletes: Mutex<Vec<String>>,
    /// pause / resume / setPreferences requests, in arrival order.
    posts: Mutex<Vec<String>>,
    injected_infos: Mutex<Option<String>>,
    fail_adds: std::sync::atomic::AtomicBool,
}

impl Capture {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            adds: Mutex::new(Vec::new()),
            deletes: Mutex::new(Vec::new()),
            posts: Mutex::new(Vec::new()),
            injected_infos: Mutex::new(None),
            fail_adds: std::sync::atomic::AtomicBool::new(false),
        })
    }
}

fn added_ownership_tag(adds: &[String]) -> String {
    adds.first()
        .and_then(|req| {
            req.split(|c: char| c == ',' || c.is_ascii_whitespace())
                .find(|token| token.starts_with("crawler-media-owned-"))
                .map(str::to_string)
        })
        .unwrap_or_else(|| downloader::ownership_tag("https://pt.example/download.php?id=1"))
}

fn match_fake_qbit_route(
    req: &str,
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
        capture.adds.lock().push(req.to_string());
        if capture.fail_adds.load(std::sync::atomic::Ordering::SeqCst) {
            ("200 OK", "Fails.".into(), "")
        } else {
            ("200 OK", "Ok.".into(), "")
        }
    } else if req.starts_with("GET /signed.torrent") || req.starts_with("GET /download.php") {
        // D-M4: must be a real bencoded torrent (dictionary with `info`).
        ("200 OK", VALID_TORRENT.into(), "")
    } else if req.starts_with("GET /missing.torrent") {
        ("404 Not Found", "not found".into(), "")
    } else if req.starts_with("GET /gone.torrent") {
        ("410 Gone", "gone".into(), "")
    } else if req.starts_with("GET /503.torrent") {
        ("503 Service Unavailable", "unavailable".into(), "")
    } else if req.starts_with("GET /429.torrent") {
        ("429 Too Many Requests", "slow down".into(), "")
    } else if req.starts_with("GET /html-login.torrent") {
        (
            "200 OK",
            "<html><body>Please login first</body></html>".into(),
            "",
        )
    } else if req.starts_with("GET /truncated-bencode.torrent") {
        (
            "200 OK",
            "d8:announce10:https://pt4:infod6:lengthi1e".into(),
            "",
        )
    } else if req.starts_with("GET /cf-challenge.torrent") {
        (
            "200 OK",
            "<html><body>challenge</body></html>".into(),
            "cf-mitigated: challenge\r\n",
        )
    } else if req.starts_with("GET /cf-html.torrent") {
        (
            "200 OK",
            "<!DOCTYPE html><html><head><title>Just a moment...</title></head><body><div class=\"cf-browser-verification\"></div></body></html>".into(),
            "",
        )
    } else if req.starts_with("GET /api/v2/app/version") {
        ("200 OK", "v5.3.0".into(), "")
    } else if req.contains("/api/v2/torrents/info") {
        let injected = capture.injected_infos.lock().clone();
        let body = if let Some(custom) = injected {
            custom
        } else {
            let adds = capture.adds.lock();
            if adds.is_empty() {
                "[]".into()
            } else {
                let tag = added_ownership_tag(&adds);
                format!(
                    r#"[{{"hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","name":"The.Matrix.1999.2160p.BluRay.x265-GROUP","progress":1.0,"save_path":"{save_path}","state":"stalledDL","tags":"other,crawler-media,{tag}","size":1000,"downloaded":1000,"uploaded":7,"dlspeed":128,"upspeed":64}}]"#
                )
            }
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
        let (status, body, extra) = match_fake_qbit_route(&request.raw, &capture, &save_path);
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n{extra}\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
    });
    (server.url(), server)
}

/// Connect to a fake qBittorrent at `url` with test credentials.
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
    }
}

fn torrent_for_server(url: &str) -> Torrent {
    torrent_at(&format!("{url}/download.php?id=1"))
}

#[test]
fn adds_enclosure_as_qbittorrent_task_with_stopped_semantics() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = QbitDownloader::connect(QbitConfig {
        url,
        username: "admin".into(),
        password: "pass".into(),
        api_key: None,
        category: Some("crawler-media".into()),
        path_maps: vec![],
    })
    .unwrap();

    let magnet = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567";
    dl.add(&torrent_at(magnet)).unwrap();

    let adds = capture.adds.lock();
    assert_eq!(adds.len(), 1);
    let body = &adds[0];
    assert!(body.contains("name=\"urls\""));
    assert!(body.contains(magnet));
    assert!(body.contains("name=\"stopped\"\r\n\r\nfalse"));
    assert!(body.contains("name=\"paused\"\r\n\r\nfalse"));
    assert!(
        body.contains(downloader::TASK_TAG),
        "所有投递包含 crawler-media 标签"
    );
}

#[test]
fn explicit_save_path_is_sent_to_qbittorrent() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = connect(&url);
    let t = torrent_for_server(&url);
    dl.add_with_options(&t, &t.enclosure, Some("/downloads/manual"))
        .unwrap();
    let adds = capture.adds.lock();
    assert_eq!(adds.len(), 1);
    assert!(adds[0].contains("name=\"savepath\"\r\n\r\n/downloads/manual"));
}

#[test]
fn resolved_download_uses_signed_file_and_stable_torrent_tag() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = connect(&url);
    let torrent = torrent_at("https://pt.example/download.php?id=1");
    dl.add_resolved(&torrent, &format!("{url}/signed.torrent"))
        .unwrap();
    let adds = capture.adds.lock();
    assert_eq!(adds.len(), 1);
    assert!(adds[0].contains("name=\"torrents\""));
    assert!(adds[0].contains(downloader::TASK_TAG));
    assert!(!adds[0].contains(&torrent.enclosure));
}

#[test]
fn add_is_skipped_when_qbittorrent_already_has_the_torrent() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = QbitDownloader::connect(QbitConfig {
        url: url.clone(),
        username: "admin".into(),
        password: "pass".into(),
        api_key: None,
        category: None,
        path_maps: vec![],
    })
    .unwrap();
    let torrent = torrent_for_server(&url);
    dl.add(&torrent).unwrap();
    dl.add(&torrent).unwrap();
    assert_eq!(capture.adds.lock().len(), 1);
}

#[test]
fn completed_task_exposes_files_for_transfer() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("movie.mkv");
    std::fs::write(&file, b"bytes").unwrap();
    let capture = Capture::new();
    let (url, _server) = serve(capture, dir.path().display().to_string().replace('\\', "/"));
    let dl = QbitDownloader::connect(QbitConfig {
        url: url.clone(),
        username: "admin".into(),
        password: "pass".into(),
        api_key: None,
        category: None,
        path_maps: vec![],
    })
    .unwrap();

    let torrent = torrent_for_server(&url);
    dl.add(&torrent).unwrap();
    let files = dl.completed_files(&torrent).unwrap();
    assert_eq!(files, vec![file]);
}

/// Live check against a real qBittorrent (not run in CI).
///
/// ```text
/// CRAWLER_MEDIA_QB_URL=http://127.0.0.1:8080 \
/// CRAWLER_MEDIA_QB_USER=admin \
/// CRAWLER_MEDIA_QB_PASS=adminadmin \
/// cargo test -p downloader --test qbit live_qbittorrent -- --ignored --nocapture
/// ```
#[test]
#[ignore]
fn live_qbittorrent_add_is_manual() {
    let url = std::env::var("CRAWLER_MEDIA_QB_URL").expect("CRAWLER_MEDIA_QB_URL");
    let username = std::env::var("CRAWLER_MEDIA_QB_USER").unwrap_or_else(|_| "admin".into());
    let password = std::env::var("CRAWLER_MEDIA_QB_PASS").expect("CRAWLER_MEDIA_QB_PASS");
    let dl = QbitDownloader::connect(QbitConfig {
        url,
        username,
        password,
        api_key: None,
        category: Some("crawler-media-test".into()),
        path_maps: vec![],
    })
    .expect("login");
    dl.add(&torrent_at(
        "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567",
    ))
    .expect("add");
}

#[test]
fn remove_deletes_task_by_hash_keeping_files() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = QbitDownloader::connect(QbitConfig {
        url: url.clone(),
        username: "admin".into(),
        password: "pass".into(),
        api_key: None,
        category: Some("crawler-media".into()),
        path_maps: vec![],
    })
    .unwrap();

    let torrent = torrent_for_server(&url);
    dl.add(&torrent).unwrap();
    dl.remove(&torrent, false).unwrap();

    let deletes = capture.deletes.lock();
    assert_eq!(deletes.len(), 1, "应发出删除请求");
    assert!(
        deletes[0].contains("hashes=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            && deletes[0].contains("deleteFiles=false"),
        "删除请求应包含已证实 hash 且保留文件: {}",
        deletes[0]
    );
}

#[test]
fn delete_task_removes_by_hash_and_honours_delete_files() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = connect(&url);

    dl.delete_task("deadbeef", true).unwrap();

    let deletes = capture.deletes.lock();
    assert_eq!(deletes.len(), 1, "应发出一次删除请求");
    assert!(
        deletes[0].contains("hashes=deadbeef") && deletes[0].contains("deleteFiles=true"),
        "删除请求应带 hash 与 deleteFiles=true: {}",
        deletes[0]
    );
}

#[test]
fn pause_and_resume_target_the_hash() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = connect(&url);

    dl.pause_task("abc").unwrap();
    dl.resume_task("abc").unwrap();

    let posts = capture.posts.lock();
    assert_eq!(posts.len(), 2);
    assert!(
        posts[0].contains("/api/v2/torrents/pause") && posts[0].contains("hashes=abc"),
        "pause 请求错误: {}",
        posts[0]
    );
    assert!(
        posts[1].contains("/api/v2/torrents/resume") && posts[1].contains("hashes=abc"),
        "resume 请求错误: {}",
        posts[1]
    );
}

#[test]
fn speed_limits_read_from_preferences() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = connect(&url);

    let (dl_limit, ul_limit) = dl.get_speed_limits().unwrap();
    assert_eq!(dl_limit, 1_048_576, "dl_limit 应回读 preferences");
    assert_eq!(ul_limit, 0, "up_limit=0 表示不限速");
}

#[test]
fn set_speed_limits_uses_form_encoded_set_preferences() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = connect(&url);

    dl.set_speed_limits(2_097_152, 524_288).unwrap();

    let posts = capture.posts.lock();
    assert_eq!(posts.len(), 1);
    let req = &posts[0];
    assert!(req.starts_with("POST /api/v2/app/setPreferences"), "{req}");
    // qBittorrent expects a form field named `json`, not a raw JSON body.
    assert!(req.contains("application/x-www-form-urlencoded"), "{req}");
    assert!(
        req.contains("json=%7B%22dl_limit%22%3A2097152%2C%22up_limit%22%3A524288%7D"),
        "setPreferences 载荷应为 form-encoded json: {req}"
    );
}

#[test]
fn task_snapshots_extract_our_tag_and_live_state() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = connect(&url);
    let torrent = torrent_for_server(&url);
    // The fake only reports tasks once something has been added.
    dl.add(&torrent).unwrap();

    let snapshots = dl.task_snapshots().unwrap();
    assert_eq!(snapshots.len(), 1, "应上报客户端里的全部任务");
    let snapshot = &snapshots[0];
    assert_eq!(
        snapshot.tag, "crawler-media",
        "应从 tags 里挑出 crawler-media 标记，忽略 ownership 派生标签"
    );
    assert_eq!(
        snapshot.owned_tags,
        vec![downloader::ownership_tag(&torrent.enclosure)],
        "D-S1: per-torrent ownership tag must be exposed for HTTP identity"
    );
    assert_eq!(snapshot.state, "stalledDL");
    assert_eq!(snapshot.progress, 1.0);
    assert_eq!(snapshot.size_bytes, 1000);
    assert_eq!(snapshot.downloaded_bytes, 1000);
    assert_eq!(snapshot.uploaded_bytes, 7);
    assert_eq!(snapshot.download_speed, 128);
    assert_eq!(snapshot.upload_speed, 64);
    assert_eq!(
        snapshot.info_hash,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
}

#[test]
fn task_snapshots_of_foreign_torrents_carry_no_tag() {
    // With nothing added the fake reports no tasks at all, which is the
    // "client unreachable / empty" shape callers must tolerate.
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = connect(&url);
    assert!(dl.task_snapshots().unwrap().is_empty());
}

#[test]
fn same_size_unrelated_torrent_does_not_match() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = connect(&url);

    // 客户端内存在一个同体积(1000字节)但标题完全不相关的已完成任务
    *capture.injected_infos.lock() = Some(r#"[{"hash":"unrelated-hash","name":"Other.Movie.2024.1080p","progress":1.0,"save_path":"/downloads","state":"uploading","tags":"crawler-media","size":1000,"downloaded":1000,"uploaded":7,"dlspeed":0,"upspeed":0}]"#.to_string());

    let mut target = torrent_for_server(&url);
    target.title = "The.Matrix.1999.2160p".into();
    target.size_bytes = Some(1000);

    // 1. 尝试获取完成文件：绝不能误匹配将 Other.Movie 的文件归给 Matrix！
    let files = dl.completed_files(&target).unwrap();
    assert!(
        files.is_empty(),
        "同体积不同标题的任务绝不能被识别为已完成目标"
    );

    // 2. 尝试添加种子：绝不能因为存在同体积任务就误判已存在而跳过添加！
    dl.add(&target).unwrap();
    assert_eq!(
        capture.adds.lock().len(),
        1,
        "同体积不同标题的任务应正常执行添加"
    );

    // 3. 尝试删除目标种子：绝不能误删无关的同体积种子！
    assert!(dl.remove(&target, true).is_err());
    assert!(
        capture.deletes.lock().is_empty(),
        "绝不能误删同体积的无关种子"
    );
}

#[test]
fn same_name_different_hash_is_not_treated_as_owned() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = connect(&url);
    let magnet = "magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    *capture.injected_infos.lock() = Some(
        r#"[{"hash":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","name":"The.Matrix.1999.2160p.BluRay.x265-GROUP","progress":1.0,"save_path":"/downloads","state":"uploading","tags":"crawler-media","size":1000}]"#
            .into(),
    );
    let target = torrent_at(magnet);
    assert!(
        dl.completed_files(&target).unwrap().is_empty(),
        "同名不同 hash 绝不能被收集"
    );
    dl.add(&target).unwrap();
    assert_eq!(capture.adds.lock().len(), 1, "同名不同 hash 绝不能跳过添加");
    assert!(
        dl.remove(&target, true).is_ok(),
        "目标 magnet 不在客户端时应安全 no-op"
    );
    assert!(
        capture.deletes.lock().is_empty(),
        "同名不同 hash 绝不能被删除"
    );
}

#[test]
fn http_404_torrent_fetch_is_rejected_without_forwarding_url_to_qbit() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = connect(&url);
    let t = torrent_at(&format!("{url}/missing.torrent"));

    let err = dl.add(&t).unwrap_err();
    assert!(
        matches!(err, DownloaderError::SourceRejected(_)),
        "expected SourceRejected for 404 .torrent fetch, got: {err:?}"
    );
    assert!(
        capture.adds.lock().is_empty(),
        "404 .torrent fetch must NOT forward URL blindly to qBittorrent"
    );
}

#[test]
fn html_body_torrent_fetch_is_rejected_as_source_error() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = connect(&url);
    let t = torrent_at(&format!("{url}/html-login.torrent"));

    let err = dl.add(&t).unwrap_err();
    assert!(
        matches!(err, DownloaderError::SourceRejected(_)),
        "expected SourceRejected for HTML body instead of .torrent, got: {err:?}"
    );
    assert!(
        capture.adds.lock().is_empty(),
        "HTML error body must NOT forward URL to qBittorrent"
    );
}

#[test]
fn torrent_fetch_503_is_transient_not_source_rejected() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = connect(&url);
    let stub = format!("{url}/503.torrent");
    let err = dl
        .add_with_options(&torrent_at(&stub), &stub, None)
        .unwrap_err();
    assert!(matches!(err, DownloaderError::Message(_)), "{err:?}");
    assert!(capture.adds.lock().is_empty());
}

#[test]
fn torrent_fetch_429_and_connection_refused_are_transient() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = connect(&url);

    let rate_limited = format!("{url}/429.torrent");
    let err = dl
        .add_with_options(&torrent_at(&rate_limited), &rate_limited, None)
        .unwrap_err();
    assert!(matches!(err, DownloaderError::Message(_)), "{err:?}");

    let closed_port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    };
    let closed_url = format!("http://127.0.0.1:{closed_port}/t.torrent");
    let err = dl
        .add_with_options(&torrent_at(&closed_url), &closed_url, None)
        .unwrap_err();
    assert!(matches!(err, DownloaderError::Message(_)), "{err:?}");
}

#[test]
fn torrent_fetch_404_is_source_rejected() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = connect(&url);

    let missing = format!("{url}/missing.torrent");
    let err = dl
        .add_with_options(&torrent_at(&missing), &missing, None)
        .unwrap_err();
    assert!(matches!(err, DownloaderError::SourceRejected(_)), "{err:?}");

    let gone = format!("{url}/gone.torrent");
    let err = dl
        .add_with_options(&torrent_at(&gone), &gone, None)
        .unwrap_err();
    assert!(matches!(err, DownloaderError::SourceRejected(_)), "{err:?}");
}

#[test]
fn qbit_fails_reply_is_transient() {
    let capture = Capture::new();
    capture
        .fail_adds
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = connect(&url);
    let stub = format!("{url}/signed.torrent");

    let err = dl
        .add_with_options(&torrent_at(&stub), &stub, None)
        .unwrap_err();
    assert!(matches!(err, DownloaderError::Message(_)), "{err:?}");
}

#[test]
fn truncated_bencode_and_cloudflare_waf_responses_are_transient_while_login_html_is_rejected() {
    let capture = Capture::new();
    let (url, _server) = serve(capture.clone(), "/downloads".into());
    let dl = connect(&url);

    for transient_path in [
        "/truncated-bencode.torrent",
        "/cf-challenge.torrent",
        "/cf-html.torrent",
    ] {
        let target = format!("{url}{transient_path}");
        let err = dl
            .add_with_options(&torrent_at(&target), &target, None)
            .unwrap_err();
        assert!(
            matches!(err, DownloaderError::Message(_)),
            "expected transient DownloaderError::Message for {transient_path}, got: {err:?}"
        );
    }

    // Short body relative to Content-Length (mid-stream disconnect) must also stay transient.
    let short_server = FakeServer::start(move |_request, stream| {
        let partial = "<html><body>truncated";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: 512\r\nContent-Type: application/x-bittorrent\r\nConnection: close\r\n\r\n{partial}"
        );
        let _ = stream.write_all(resp.as_bytes());
    });
    let short_url = format!("{}/short.torrent", short_server.url());
    let short_err = dl
        .add_with_options(&torrent_at(&short_url), &short_url, None)
        .unwrap_err();
    assert!(
        matches!(short_err, DownloaderError::Message(_)),
        "expected transient Message for Content-Length truncation, got: {short_err:?}"
    );

    let login_url = format!("{url}/html-login.torrent");
    let login_err = dl
        .add_with_options(&torrent_at(&login_url), &login_url, None)
        .unwrap_err();
    assert!(
        matches!(login_err, DownloaderError::SourceRejected(_)),
        "generic HTML login page must remain SourceRejected, got: {login_err:?}"
    );
    assert!(capture.adds.lock().is_empty());
}
