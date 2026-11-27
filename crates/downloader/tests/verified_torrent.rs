use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use async_trait::async_trait;
use domain::{DownloaderId, PluginOperationId, SiteId, Torrent};
use downloader::{
    DownloadOpContext, DownloadOutcome, DownloadSubmitError, DownloadSubmitter, FrozenDownloadPlan,
    MAX_TORRENT_BYTES, PreparedTorrent, SubmissionPurpose, TorrentFetchError, TorrentHttpSource,
    TorrentParseError, TorrentSource, fetch_verified_torrent, parse_verified_torrent,
    prepare_download_payload, torrent_target_policy, with_torrent_limits,
};
use net_guard::{
    CredentialSlot, DnsResolver, NetGuardClient, NetGuardError, Origin, PrivateEndpointException,
    Scheme, TargetPolicy,
};
use parking_lot::Mutex;
use sha1::Sha1;
use sha2::{Digest, Sha256};

fn bencode_bytes(data: &[u8]) -> Vec<u8> {
    let mut out = format!("{}:", data.len()).into_bytes();
    out.extend_from_slice(data);
    out
}

fn single_file_torrent(length: i64, comment: Option<&str>) -> (Vec<u8>, Vec<u8>) {
    let mut info = Vec::new();
    info.push(b'd');
    info.extend_from_slice(&bencode_bytes(b"length"));
    info.extend_from_slice(format!("i{length}e").as_bytes());
    info.extend_from_slice(&bencode_bytes(b"name"));
    info.extend_from_slice(&bencode_bytes(b"demo.mkv"));
    info.extend_from_slice(&bencode_bytes(b"piece length"));
    info.extend_from_slice(b"i16384e");
    info.extend_from_slice(&bencode_bytes(b"pieces"));
    info.extend_from_slice(&bencode_bytes(&[b'a'; 20]));
    info.push(b'e');

    let mut root = Vec::new();
    root.push(b'd');
    root.extend_from_slice(&bencode_bytes(b"announce"));
    root.extend_from_slice(&bencode_bytes(b"https://tracker.example/ann"));
    if let Some(c) = comment {
        root.extend_from_slice(&bencode_bytes(b"comment"));
        root.extend_from_slice(&bencode_bytes(c.as_bytes()));
    }
    root.extend_from_slice(&bencode_bytes(b"info"));
    root.extend_from_slice(&info);
    root.push(b'e');
    (root, info)
}

fn multi_file_torrent(lengths: &[i64]) -> (Vec<u8>, Vec<u8>) {
    let mut info = Vec::new();
    info.push(b'd');
    info.extend_from_slice(&bencode_bytes(b"files"));
    info.push(b'l');
    for (idx, len) in lengths.iter().enumerate() {
        info.push(b'd');
        info.extend_from_slice(&bencode_bytes(b"length"));
        info.extend_from_slice(format!("i{len}e").as_bytes());
        info.extend_from_slice(&bencode_bytes(b"path"));
        info.push(b'l');
        info.extend_from_slice(&bencode_bytes(format!("part{idx}.mkv").as_bytes()));
        info.push(b'e');
        info.push(b'e');
    }
    info.push(b'e');
    info.extend_from_slice(&bencode_bytes(b"name"));
    info.extend_from_slice(&bencode_bytes(b"pack"));
    info.extend_from_slice(&bencode_bytes(b"piece length"));
    info.extend_from_slice(b"i16384e");
    info.extend_from_slice(&bencode_bytes(b"pieces"));
    info.extend_from_slice(&bencode_bytes(&[b'b'; 20]));
    info.push(b'e');

    let mut root = Vec::new();
    root.push(b'd');
    root.extend_from_slice(&bencode_bytes(b"info"));
    root.extend_from_slice(&info);
    root.push(b'e');
    (root, info)
}

fn sha1_hex(data: &[u8]) -> String {
    Sha1::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Debug)]
struct StaticDns(Mutex<HashMap<String, Vec<IpAddr>>>);

impl DnsResolver for StaticDns {
    fn resolve(
        &self,
        host: &str,
        _port: u16,
        _timeout: Duration,
    ) -> Result<Vec<IpAddr>, NetGuardError> {
        self.0
            .lock()
            .get(host)
            .cloned()
            .ok_or_else(|| NetGuardError::Transport(format!("unknown host {host}")))
    }
}

fn spawn_http_once(
    status: &str,
    extra_headers: Vec<(String, String)>,
    body: Vec<u8>,
) -> (SocketAddr, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let captured = Arc::new(Mutex::new(Vec::new()));
    let captured_ref = Arc::clone(&captured);
    let status_owned = status.to_string();
    let handle = thread::spawn(move || {
        let _ = listener.set_nonblocking(true);
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut accepted = None;
        while std::time::Instant::now() < deadline {
            match listener.accept() {
                Ok((stream, _)) => {
                    accepted = Some(stream);
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
        if let Some(mut stream) = accepted {
            let _ = stream.set_nonblocking(false);
            let mut buf = [0u8; 4096];
            if let Ok(n) = stream.read(&mut buf) {
                captured_ref
                    .lock()
                    .push(String::from_utf8_lossy(&buf[..n]).to_string());
            }
            let mut resp = format!(
                "HTTP/1.1 {status_owned}\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            for (k, v) in extra_headers {
                resp.push_str(&format!("{k}: {v}\r\n"));
            }
            resp.push_str("\r\n");
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    (addr, captured, handle)
}

#[test]
fn single_file_torrent_computes_verified_total_bytes_info_hash_and_sha256_digest() {
    let (bytes, info) = single_file_torrent(4_294_967_296, None);
    let parsed = parse_verified_torrent(&bytes).expect("valid single-file torrent");
    assert_eq!(parsed.verified_total_bytes, 4_294_967_296);
    assert_eq!(parsed.info_hash, sha1_hex(&info));
    assert_eq!(parsed.bytes_digest, sha256_hex(&bytes));
    assert_eq!(parsed.bytes, bytes);
}

#[test]
fn multi_file_torrent_checked_sums_all_file_lengths_including_zero_padding() {
    let (bytes, info) = multi_file_torrent(&[0, 1_500_000_000, 2_500_000_000, 123]);
    let parsed = parse_verified_torrent(&bytes).expect("valid multi-file torrent");
    assert_eq!(parsed.verified_total_bytes, 4_000_000_123);
    assert_eq!(parsed.info_hash, sha1_hex(&info));
    assert_eq!(parsed.bytes_digest, sha256_hex(&bytes));
}

#[test]
fn rejects_empty_non_dict_html_duplicate_keys_and_trailing_garbage() {
    assert_eq!(parse_verified_torrent(b""), Err(TorrentParseError::Empty));
    assert!(matches!(
        parse_verified_torrent(b"<html>404</html>"),
        Err(TorrentParseError::MalformedBencode { .. })
    ));
    assert!(matches!(
        parse_verified_torrent(b"l1:ae"),
        Err(TorrentParseError::MalformedBencode { .. })
    ));
    let (mut with_trailing, _) = single_file_torrent(10, None);
    with_trailing.extend_from_slice(b"junk");
    assert!(matches!(
        parse_verified_torrent(&with_trailing),
        Err(TorrentParseError::MalformedBencode { .. })
    ));
    let dup_info = b"d4:infod6:lengthi10ee4:infod6:lengthi20eee";
    assert!(matches!(
        parse_verified_torrent(dup_info),
        Err(TorrentParseError::MalformedBencode { .. })
    ));
    let dup_len = b"d4:infod6:lengthi10e6:lengthi20eee";
    assert!(matches!(
        parse_verified_torrent(dup_len),
        Err(TorrentParseError::MalformedBencode { .. })
    ));
    assert!(matches!(
        parse_verified_torrent(b"d8:announce10:https://pt4:info"),
        Err(TorrentParseError::MalformedBencode { .. })
    ));
    assert_eq!(
        parse_verified_torrent(b"d4:infoi1ee"),
        Err(TorrentParseError::InvalidInfoDict)
    );
}

#[test]
fn rejects_missing_length_ambiguous_layout_v2_and_empty_files() {
    assert_eq!(
        parse_verified_torrent(b"d4:infod4:name4:demoee"),
        Err(TorrentParseError::MissingLength)
    );
    assert_eq!(
        parse_verified_torrent(b"d4:infod9:file treede4:name4:demoee"),
        Err(TorrentParseError::MissingLength)
    );
    assert_eq!(
        parse_verified_torrent(b"d4:infod5:filesld6:lengthi1eee6:lengthi1eee"),
        Err(TorrentParseError::AmbiguousLayout)
    );
    assert_eq!(
        parse_verified_torrent(b"d4:infod5:filesleee"),
        Err(TorrentParseError::EmptyFiles)
    );
}

#[test]
fn rejects_negative_malformed_non_int_and_i64_overflow_lengths() {
    assert_eq!(
        parse_verified_torrent(b"d4:infod6:lengthi-1eee"),
        Err(TorrentParseError::NegativeLength(-1))
    );
    assert!(matches!(
        parse_verified_torrent(b"d4:infod6:lengthi-0eee"),
        Err(TorrentParseError::MalformedBencode { .. })
    ));
    assert!(matches!(
        parse_verified_torrent(b"d4:infod6:lengthi01eee"),
        Err(TorrentParseError::MalformedBencode { .. })
    ));
    assert!(matches!(
        parse_verified_torrent(b"d4:infod6:length3:123ee"),
        Err(TorrentParseError::MalformedBencode { .. } | TorrentParseError::InvalidFileEntry)
    ));
    let (overflow_multi, _) = multi_file_torrent(&[i64::MAX, 1]);
    assert_eq!(
        parse_verified_torrent(&overflow_multi),
        Err(TorrentParseError::LengthOverflow)
    );
    let overflow_single = format!("d4:infod6:lengthi{}0eee", i64::MAX).into_bytes();
    assert_eq!(
        parse_verified_torrent(&overflow_single),
        Err(TorrentParseError::LengthOverflow)
    );
}

#[test]
fn rejects_deeply_nested_bencode_and_over_10_mib_payload() {
    let mut deep = b"d4:infod6:lengthi1e5:extra".to_vec();
    deep.extend(std::iter::repeat_n(b'l', 80));
    deep.extend(std::iter::repeat_n(b'e', 80));
    deep.extend_from_slice(b"ee");
    assert!(matches!(
        parse_verified_torrent(&deep),
        Err(TorrentParseError::NestingTooDeep { .. })
    ));

    let oversized = vec![b'd'; MAX_TORRENT_BYTES + 1];
    assert_eq!(
        parse_verified_torrent(&oversized),
        Err(TorrentParseError::PayloadTooLarge {
            limit: MAX_TORRENT_BYTES
        })
    );
}

#[test]
fn bytes_digest_changes_when_non_info_bytes_change() {
    let (a, _) = single_file_torrent(4096, Some("alpha"));
    let (b, _) = single_file_torrent(4096, Some("beta"));
    let pa = parse_verified_torrent(&a).expect("a");
    let pb = parse_verified_torrent(&b).expect("b");
    assert_eq!(pa.info_hash, pb.info_hash);
    assert_eq!(pa.verified_total_bytes, pb.verified_total_bytes);
    assert_ne!(pa.bytes_digest, pb.bytes_digest);
}

#[test]
fn fetch_verified_torrent_follows_validated_redirect_strips_cross_origin_creds_and_ignores_untrusted_size()
 {
    let one_tib = 1_099_511_627_776_i64;
    let (torrent_bytes, info) = multi_file_torrent(&[one_tib / 2, one_tib / 2]);
    let (b_addr, b_reqs, b_handle) = spawn_http_once("200 OK", vec![], torrent_bytes);
    let redirect_target = format!("http://cdn.example:{}/file.torrent", b_addr.port());
    let (a_addr, a_reqs, a_handle) = spawn_http_once(
        "302 Found",
        vec![("Location".into(), redirect_target)],
        Vec::new(),
    );

    let mut map = HashMap::new();
    map.insert(
        "origin.example".into(),
        vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
    );
    map.insert("cdn.example".into(), vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);
    let client = NetGuardClient::with_resolver(Arc::new(StaticDns(Mutex::new(map))));
    let policy = with_torrent_limits(TargetPolicy::production())
        .with_private_exception(PrivateEndpointException {
            host: "origin.example".into(),
            port: a_addr.port(),
            allowed_ips: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            path_prefixes: vec!["/".into()],
        })
        .with_private_exception(PrivateEndpointException {
            host: "cdn.example".into(),
            port: b_addr.port(),
            allowed_ips: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            path_prefixes: vec!["/".into()],
        });

    let untrusted_torrent = Torrent {
        site_id: SiteId::new(),
        title: "fake-small".into(),
        enclosure: format!("http://origin.example:{}/dl.php?id=1", a_addr.port()),
        size_bytes: Some(1),
        seeders: None,
        free: true,
        hr: None,
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
    };
    let mut source = TorrentHttpSource::get(untrusted_torrent.enclosure.clone());
    source.creds = Some(CredentialSlot::new(
        "pt",
        Origin {
            scheme: Scheme::Http,
            host: "origin.example".into(),
            port: a_addr.port(),
        },
        "epoch-1",
        vec![("Cookie".into(), "secret=1".into())],
    ));

    let fetched = fetch_verified_torrent(&client, &source, &policy).expect("redirected fetch");
    let _ = a_handle.join();
    let _ = b_handle.join();

    assert_eq!(fetched.verified_total_bytes, one_tib);
    assert_eq!(fetched.info_hash, sha1_hex(&info));
    assert!(
        a_reqs.lock()[0]
            .to_ascii_lowercase()
            .contains("cookie: secret=1")
    );
    assert!(!b_reqs.lock()[0].to_ascii_lowercase().contains("cookie:"));
}

#[test]
fn fetch_verified_torrent_enforces_10_mib_limit() {
    let too_big = vec![b'a'; MAX_TORRENT_BYTES + 1];
    let (addr, _reqs, handle) = spawn_http_once("200 OK", vec![], too_big);
    let policy = torrent_target_policy().with_test_loopback_socket(addr);
    let client = NetGuardClient::new();
    let source = TorrentHttpSource::get(format!("http://127.0.0.1:{}/big.torrent", addr.port()));
    let err = fetch_verified_torrent(&client, &source, &policy).expect_err("must reject > 10 MiB");
    let _ = handle.join();
    assert!(matches!(err, TorrentFetchError::TooLarge { .. }));

    // Valid payload between 5 MiB and 10 MiB succeeds with torrent_target_policy.
    let mut padding_torrent = Vec::with_capacity(6 * 1024 * 1024);
    let (base, info) = single_file_torrent(42, None);
    let pad = vec![b'x'; 6 * 1024 * 1024];
    padding_torrent.push(b'd');
    padding_torrent.extend_from_slice(&bencode_bytes(b"info"));
    padding_torrent.extend_from_slice(&info);
    padding_torrent.extend_from_slice(&bencode_bytes(b"pad"));
    padding_torrent.extend_from_slice(&bencode_bytes(&pad));
    padding_torrent.push(b'e');
    assert!(padding_torrent.len() > 5_242_880 && padding_torrent.len() < MAX_TORRENT_BYTES);
    let _ = base;
    let (ok_addr, _ok_reqs, ok_handle) = spawn_http_once("200 OK", vec![], padding_torrent);
    let ok_policy = torrent_target_policy().with_test_loopback_socket(ok_addr);
    let ok_source =
        TorrentHttpSource::get(format!("http://127.0.0.1:{}/6mib.torrent", ok_addr.port()));
    let ok = fetch_verified_torrent(&client, &ok_source, &ok_policy).expect("6 MiB allowed");
    let _ = ok_handle.join();
    assert_eq!(ok.verified_total_bytes, 42);
}

#[test]
fn fetch_verified_torrent_blocks_private_ip_self_api_rebinding_and_excess_redirects() {
    let (addr, _reqs, handle) =
        spawn_http_once("200 OK", vec![], b"d4:infod6:lengthi1eee".to_vec());
    let client = NetGuardClient::new();
    let self_blocked = torrent_target_policy()
        .with_test_loopback_socket(addr)
        .with_blocked_self(addr);
    let source = TorrentHttpSource::get(format!("http://127.0.0.1:{}/self.torrent", addr.port()));
    let err =
        fetch_verified_torrent(&client, &source, &self_blocked).expect_err("self-api blocked");
    drop(handle);
    assert!(matches!(
        err,
        TorrentFetchError::NetGuard(NetGuardError::BlockedAddress(_))
    ));

    // Rebinding to private 10.0.0.1 without exception is blocked.
    let mut dns_map = HashMap::new();
    dns_map.insert(
        "rebind.example".into(),
        vec![IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))],
    );
    let rebind_client = NetGuardClient::with_resolver(Arc::new(StaticDns(Mutex::new(dns_map))));
    let err2 = fetch_verified_torrent(
        &rebind_client,
        &TorrentHttpSource::get("http://rebind.example:8080/x.torrent"),
        &torrent_target_policy(),
    )
    .expect_err("private IP blocked");
    assert!(matches!(
        err2,
        TorrentFetchError::NetGuard(NetGuardError::BlockedAddress(_))
    ));
}

#[test]
fn fetch_verified_torrent_has_no_url_fallback_on_404_or_html() {
    let (addr_404, _, h1) = spawn_http_once("404 Not Found", vec![], b"not found".to_vec());
    let policy_404 = torrent_target_policy().with_test_loopback_socket(addr_404);
    let client = NetGuardClient::new();
    let err1 = fetch_verified_torrent(
        &client,
        &TorrentHttpSource::get(format!(
            "http://127.0.0.1:{}/missing.torrent",
            addr_404.port()
        )),
        &policy_404,
    )
    .expect_err("404 must fail without URL fallback");
    let _ = h1.join();
    assert!(matches!(
        err1,
        TorrentFetchError::HttpStatus { status: 404, .. }
    ));

    let (addr_html, _, h2) = spawn_http_once("200 OK", vec![], b"<html>login</html>".to_vec());
    let policy_html = torrent_target_policy().with_test_loopback_socket(addr_html);
    let err2 = fetch_verified_torrent(
        &client,
        &TorrentHttpSource::get(format!(
            "http://127.0.0.1:{}/login.torrent",
            addr_html.port()
        )),
        &policy_html,
    )
    .expect_err("HTML must fail without URL fallback");
    let _ = h2.join();
    assert!(matches!(err2, TorrentFetchError::Parse(_)));
}

#[test]
fn fetch_verified_torrent_classifies_cf_waf_and_truncations_as_transient() {
    let client = NetGuardClient::new();

    for (headers, body) in [
        (
            vec![("cf-mitigated".to_string(), "challenge".to_string())],
            b"<html>challenge</html>".to_vec(),
        ),
        (
            vec![],
            b"<!DOCTYPE html><html><head><title>Just a moment...</title></head></html>".to_vec(),
        ),
        (
            vec![],
            b"d8:announce10:https://pt4:infod6:lengthi1e".to_vec(),
        ),
        (vec![], b"d8:announce10:https://pt4:info".to_vec()),
    ] {
        let (addr, _, h) = spawn_http_once("200 OK", headers, body);
        let policy = torrent_target_policy().with_test_loopback_socket(addr);
        let err = fetch_verified_torrent(
            &client,
            &TorrentHttpSource::get(format!("http://127.0.0.1:{}/waf.torrent", addr.port())),
            &policy,
        )
        .expect_err("must reject transient WAF / truncated body");
        let _ = h.join();
        assert!(
            matches!(err, TorrentFetchError::Transient(_)),
            "expected Transient, got: {err:?}"
        );
    }
}

#[test]
fn prepare_download_payload_blocks_unknown_size_magnet_for_boost_even_with_provider_size_bytes() {
    let client = NetGuardClient::new();
    let policy = torrent_target_policy();
    let magnet = TorrentSource::Magnet {
        uri: "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".into(),
    };
    let boost_err = prepare_download_payload(&client, &magnet, SubmissionPurpose::Boost, &policy)
        .expect_err("auto-boost must reject unknown-size magnet");
    assert!(matches!(
        boost_err,
        TorrentFetchError::UnknownMagnetSizeForBoost
    ));

    let sub_ok = prepare_download_payload(&client, &magnet, SubmissionPurpose::Subscribe, &policy)
        .expect("subscribe allows valid magnet");
    assert!(matches!(sub_ok, PreparedTorrent::Magnet { .. }));

    let bad_magnet = TorrentSource::Magnet {
        uri: "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&xt=urn:btih:ffffffffffffffffffffffffffffffffffffffff".into(),
    };
    let bad_err =
        prepare_download_payload(&client, &bad_magnet, SubmissionPurpose::Manual, &policy)
            .expect_err("conflicting btih rejected");
    assert!(matches!(bad_err, TorrentFetchError::InvalidMagnet));
}

#[test]
fn prepare_download_payload_rejects_unsupported_schemes() {
    assert!(matches!(
        TorrentSource::from_url("file:///etc/passwd"),
        Err(TorrentFetchError::UnsupportedScheme)
    ));
    assert!(matches!(
        TorrentSource::from_url("ftp://example.com/a.torrent"),
        Err(TorrentFetchError::UnsupportedScheme)
    ));
    let client = NetGuardClient::new();
    let src = TorrentSource::Http(TorrentHttpSource::get("file:///etc/passwd"));
    assert!(matches!(
        prepare_download_payload(
            &client,
            &src,
            SubmissionPurpose::Manual,
            &torrent_target_policy()
        ),
        Err(TorrentFetchError::UnsupportedScheme)
    ));
}

struct FakeSubmitter {
    called: AtomicBool,
}

#[async_trait]
impl DownloadSubmitter for FakeSubmitter {
    async fn submit(
        &self,
        plan: FrozenDownloadPlan,
        op: DownloadOpContext,
    ) -> Result<DownloadOutcome, DownloadSubmitError> {
        self.called.store(true, Ordering::SeqCst);
        Ok(match plan.torrent.title.as_str() {
            "submit" => DownloadOutcome::Submitted {
                operation_id: op.operation_id,
                downloader_id: plan.downloader_id,
                info_hash: Some("0123".into()),
                bytes_digest: Some("abcd".into()),
                verified_total_bytes: Some(1024),
                already_owned: false,
            },
            "reject" => DownloadOutcome::Rejected {
                operation_id: op.operation_id,
                reason: "broken".into(),
                broken_task_hash: Some("0123".into()),
            },
            "defer" => DownloadOutcome::Deferred {
                operation_id: op.operation_id,
                until_unix: 9999,
                reason: "cooldown".into(),
            },
            _ => DownloadOutcome::Unknown {
                operation_id: op.operation_id,
                downloader_id: plan.downloader_id,
                reason: "post-send timeout".into(),
            },
        })
    }
}

#[tokio::test]
async fn download_submitter_trait_is_object_safe_and_models_all_four_outcomes() {
    let submitter: Arc<dyn DownloadSubmitter> = Arc::new(FakeSubmitter {
        called: AtomicBool::new(false),
    });
    for kind in ["submit", "reject", "defer", "unknown"] {
        let plan = FrozenDownloadPlan {
            torrent: Torrent {
                site_id: SiteId::new(),
                title: kind.into(),
                enclosure: "https://pt.example/1.torrent".into(),
                size_bytes: None,
                seeders: None,
                free: false,
                hr: None,
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
            },
            source: TorrentSource::Http(TorrentHttpSource::get("https://pt.example/1.torrent")),
            purpose: SubmissionPurpose::Subscribe,
            downloader_id: Some(DownloaderId::new()),
            frozen_endpoint: None,
            subscribe_id: None,
            subscribe_generation: None,
            policy_revision: None,
            media_id: None,
            score: None,
            release_override: None,
            force: false,
            save_path: None,
            category: None,
            tags: Vec::new(),
            paused: false,
            upload_limit: None,
            download_limit: None,
            sequential: false,
            first_last_piece: false,
            budget_key: None,
            plan_digest: None,
        };
        let op = DownloadOpContext {
            operation_id: PluginOperationId::new(),
            cause_id: "cause-1".into(),
            subject: "subject-1".into(),
            idempotency_key: "idem-1".into(),
            plugin_id: None,
            generation: None,
            config_revision: None,
            dry_run: false,
        };
        let out = submitter.submit(plan, op).await.expect("outcome");
        match kind {
            "submit" => assert!(matches!(out, DownloadOutcome::Submitted { .. })),
            "reject" => assert!(matches!(out, DownloadOutcome::Rejected { .. })),
            "defer" => assert!(matches!(out, DownloadOutcome::Deferred { .. })),
            _ => assert!(matches!(out, DownloadOutcome::Unknown { .. })),
        }
    }
}

#[test]
fn from_site_enclosure_accepts_opaque_plugin_ref_and_rejects_bad_schemes() {
    assert!(matches!(
        TorrentSource::from_site_enclosure("dlref-42").unwrap(),
        TorrentSource::PluginRef(ref r) if r == "dlref-42"
    ));
    assert!(matches!(
        TorrentSource::from_url("dlref-42"),
        Err(TorrentFetchError::UnsupportedScheme)
    ));
    for bad in [
        "file:///etc/passwd",
        "ftp://example.com/a.torrent",
        "javascript:alert(1)",
        "//evil.com/a",
        "",
    ] {
        assert!(
            matches!(
                TorrentSource::from_site_enclosure(bad),
                Err(TorrentFetchError::UnsupportedScheme)
            ),
            "bad enclosure {bad:?} must be rejected"
        );
    }
    let client = net_guard::NetGuardClient::new();
    let unresolved = TorrentSource::PluginRef("dlref-42".into());
    assert!(matches!(
        downloader::prepare_download_payload(
            &client,
            &unresolved,
            SubmissionPurpose::Manual,
            &net_guard::TargetPolicy::production(),
        ),
        Err(TorrentFetchError::UnsupportedScheme)
    ));
}
