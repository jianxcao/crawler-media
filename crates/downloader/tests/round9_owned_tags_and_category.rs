use downloader::{Downloader, MemoryDownloader, SubmitOptions, SubmitPayload, owned_tags_from};
use tempfile::TempDir;

#[test]
fn owned_tags_from_preserves_cm_op_tags_and_set_task_category_blocks_path_traversal() {
    let tags = owned_tags_from(["cm-op:op-r9-1", "crawler-media-owned-abc", "manual-tag"]);
    assert_eq!(
        tags,
        vec![
            "cm-op:op-r9-1".to_string(),
            "crawler-media-owned-abc".to_string()
        ]
    );

    let tmp = TempDir::new().expect("tmp");
    let dl = MemoryDownloader::new(tmp.path());
    let hash = "1111222211112222111122221111222211112222";
    dl.submit_verified(
        &SubmitPayload::Magnet {
            uri: format!("magnet:?xt=urn:btih:{hash}&dn=Test.Movie.2026"),
            info_hash: hash.into(),
        },
        &SubmitOptions {
            ownership_tag: "cm-op:op-r9-1".into(),
            ..Default::default()
        },
    )
    .expect("submit magnet");

    let snaps = dl.task_snapshots().expect("snapshots");
    assert!(
        snaps[0].owned_tags.contains(&"cm-op:op-r9-1".to_string()),
        "submit_verified snapshot must preserve cm-op:* ownership_tag: {:?}",
        snaps[0].owned_tags
    );

    for bad_cat in [
        "../outside",
        "movies/../outside",
        "/abs/path",
        "movies\\sub",
        "C:/escape",
        "movies//sub",
        "movies/sub/",
        "movies\ninj",
    ] {
        assert!(
            dl.set_task_category(hash, bad_cat).is_err(),
            "set_task_category must reject unsafe category `{bad_cat:?}`"
        );
    }

    dl.set_task_category(hash, "movies/hd")
        .expect("valid nested category");
    dl.set_task_category(hash, "")
        .expect("empty category clears assignment");
}

#[test]
fn memory_downloader_deduplicates_remembered_http_task_and_purges_on_delete() {
    let tmp = TempDir::new().expect("tmp");
    let dl = MemoryDownloader::new(tmp.path());
    let hash = "aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111";
    let torrent = domain::Torrent {
        site_id: domain::SiteId::default(),
        title: "Show.S01E01.1080p.mkv".into(),
        enclosure: "https://pt.example/download/77.torrent".into(),
        size_bytes: Some(2048),
        seeders: Some(5),
        free: false,
        hr: Some(false),
        imdb_id: None,
        id: None,
        leechers: Some(1),
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    };
    dl.submit_verified(
        &SubmitPayload::Magnet {
            uri: format!("magnet:?xt=urn:btih:{hash}&dn=Show.S01E01"),
            info_hash: hash.into(),
        },
        &SubmitOptions {
            ownership_tag: "cm-op:op-http-1".into(),
            paused: true,
            ..Default::default()
        },
    )
    .expect("submit");
    dl.remember_identity(&torrent, hash)
        .expect("remember_identity");

    let snaps = dl.task_snapshots().expect("snapshots");
    assert_eq!(
        snaps.len(),
        1,
        "remember_identity + submit_verified must emit 1 deduplicated snapshot, got: {snaps:?}"
    );
    assert_eq!(snaps[0].info_hash, hash);
    assert_eq!(snaps[0].state, "pausedDL");

    dl.delete_task(hash, false).expect("delete_task");
    assert!(
        dl.task_snapshots()
            .expect("snapshots after delete")
            .is_empty(),
        "delete_task must also purge remembered self.added ghost entry"
    );
}

#[test]
fn prepare_download_payload_redacts_rss_download_passkey_in_fetch_error() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let srv = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    let client = net_guard::NetGuardClient::new();
    let policy = net_guard::TargetPolicy::production().with_test_loopback_socket(addr);
    let src = downloader::TorrentSource::Http(downloader::TorrentHttpSource::get(format!(
        "http://{addr}/rss/download/0123456789abcdef0123456789abcdef/77.torrent"
    )));
    let err = downloader::prepare_download_payload(
        &client,
        &src,
        downloader::SubmissionPurpose::Manual,
        &policy,
    )
    .expect_err("404 status must fail fetch");
    let _ = srv.join();
    let msg = err.to_string();
    assert!(
        !msg.contains("0123456789abcdef0123456789abcdef"),
        "TorrentFetchError leaked /rss/download/<passkey>: {msg}"
    );
}
