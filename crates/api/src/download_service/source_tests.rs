use super::*;
use domain::{SiteId, Torrent};
use indexer::{
    AuthType, FetchMethod, FetchRequest, IndexerError, IndexerProvider, ProviderAvailability,
    ProviderCallContext, ProviderCapability, ProviderDescriptor, ProviderSource, RegistryCandidate,
    ResolvedDownload, SearchQuery,
};
use std::sync::Arc;

struct StubReqProvider {
    desc: ProviderDescriptor,
    resolved: ResolvedDownload,
}

impl IndexerProvider for StubReqProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.desc
    }
    fn search(
        &self,
        _: &ProviderCallContext<'_>,
        _: &domain::Site,
        _: &SearchQuery<'_>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        Ok(vec![])
    }
    fn rss(
        &self,
        _: &ProviderCallContext<'_>,
        _: &domain::Site,
        _: Option<&str>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        Ok(vec![])
    }
    fn resolve_download(
        &self,
        _: &ProviderCallContext<'_>,
        _: &domain::Site,
        _: &Torrent,
    ) -> Result<Option<ResolvedDownload>, IndexerError> {
        Ok(Some(self.resolved.clone()))
    }
}

struct StubNoopFetcher;
impl indexer::Fetcher for StubNoopFetcher {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

fn test_state_with_resolved(provider_id: &str, resolved: ResolvedDownload) -> ApiState {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::Store::open(dir.path().join("data")).unwrap();
    let desc = ProviderDescriptor {
        id: provider_id.into(),
        name: "Stub".into(),
        kind: "wasm".into(),
        source: ProviderSource::Builtin,
        auth_types: vec![AuthType::Cookie],
        capabilities: vec![ProviderCapability::ResolveDownload],
        availability: ProviderAvailability::Enabled,
        hidden_from_catalog: false,
    };
    let candidate = RegistryCandidate::from_builtin()
        .unwrap()
        .add_indexer_provider(Arc::new(StubReqProvider { desc, resolved }))
        .unwrap();
    let dl = Arc::new(downloader::MemoryDownloader::new(dir.path().join("stage")));
    std::mem::forget(dir);
    let state = ApiState::new(
        store,
        "tok".into(),
        indexer::RegistrySnapshot::builtin().unwrap(),
        Arc::new(StubNoopFetcher),
        dl,
    )
    .unwrap();
    state.indexer.publish(candidate).unwrap();
    state
}

fn sample_site(profile_id: &str, url: &str) -> domain::Site {
    domain::Site {
        id: SiteId::new(),
        name: "S".into(),
        url: url.into(),
        profile_id: profile_id.into(),
        cookie: Some("uid=1; pass=SITE_COOKIE".into()),
        api_key: Some("SITE_API_KEY".into()),
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: domain::MaintenanceVia::Http,
        browser_search: false,
        timezone: "UTC".into(),
    }
}

fn sample_plan(site_id: SiteId, url: &str) -> FrozenDownloadPlan {
    FrozenDownloadPlan {
        torrent: Torrent {
            site_id,
            id: Some("1".into()),
            title: "T".into(),
            enclosure: url.into(),
            size_bytes: None,
            seeders: None,
            leechers: None,
            snatched: None,
            upload_time: None,
            poster_url: None,
            free: false,
            hr: None,
            imdb_id: None,
            detail_url: None,
            category: None,
            free_2x: false,
            free_until: None,
        },
        source: TorrentSource::from_url(url).unwrap(),
        purpose: SubmissionPurpose::Manual,
        downloader_id: None,
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
        tags: vec![],
        paused: false,
        upload_limit: None,
        download_limit: None,
        sequential: false,
        first_last_piece: false,
        budget_key: None,
        plan_digest: None,
    }
}

#[test]
fn resolve_source_request_respects_unauth_custom_origin_and_post_content_type() {
    let site = sample_site("stub_req", "https://site.example/");
    let st_unauth = test_state_with_resolved(
        "stub_req",
        ResolvedDownload::Request(FetchRequest {
            key: "k".into(),
            url: "https://site.example/public.torrent".into(),
            method: FetchMethod::PostJson,
            body: Some("{\"id\":1}".into()),
            cookie: None,
            api_key: None,
            proxy: None,
            cdp_url: None,
        }),
    );
    let src = resolve_source(
        &st_unauth,
        &sample_plan(site.id, "https://site.example/1"),
        Some(&site),
    )
    .unwrap();
    let TorrentSource::Http(http) = src else {
        panic!("expected Http")
    };
    assert!(
        http.creds.is_none(),
        "with_auth=false must not attach site creds"
    );
    assert!(
        http.headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v == "application/json")
    );

    let st_custom = test_state_with_resolved(
        "stub_req",
        ResolvedDownload::Request(FetchRequest {
            key: "k".into(),
            url: "https://cdn.other.example/dl".into(),
            method: FetchMethod::PostForm,
            body: Some("id=1".into()),
            cookie: Some("custom=1".into()),
            api_key: None,
            proxy: None,
            cdp_url: None,
        }),
    );
    let src2 = resolve_source(
        &st_custom,
        &sample_plan(site.id, "https://site.example/1"),
        Some(&site),
    )
    .unwrap();
    let TorrentSource::Http(http2) = src2 else {
        panic!("expected Http")
    };
    assert!(
        http2
            .headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("content-type")
                && v == "application/x-www-form-urlencoded")
    );
    let creds = http2.creds.expect("explicit req.cookie must bind creds");
    let hdrs = creds
        .headers_for_origin(
            &net_guard::normalize_url("https://cdn.other.example/dl", true)
                .unwrap()
                .origin,
        )
        .unwrap();
    assert_eq!(
        hdrs.len(),
        1,
        "custom origin must NOT fall back to site.api_key: {hdrs:?}"
    );
    assert_eq!(hdrs[0], ("Cookie".into(), "custom=1".into()));
}

fn minimal_torrent_bytes(name: &str, length: usize) -> (Vec<u8>, String) {
    let info_dict = format!(
        "d6:lengthi{length}e4:name{}:{name}12:piece lengthi16384e6:pieces20:01234567890123456789e",
        name.len()
    );
    let mut bytes = Vec::from(b"d8:announce20:https://tracker/test4:info");
    bytes.extend_from_slice(info_dict.as_bytes());
    bytes.push(b'e');
    let hash = downloader::torrent_info_hash(&bytes).unwrap();
    (bytes, hash)
}

struct StubTorrentHttpServer {
    addr: std::net::SocketAddr,
    hits: Arc<std::sync::atomic::AtomicUsize>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StubTorrentHttpServer {
    fn spawn(responses: Vec<Vec<u8>>) -> Self {
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let (hits, stop) = (
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicBool::new(false)),
        );
        let (hits_c, stop_c) = (Arc::clone(&hits), Arc::clone(&stop));
        let handle = std::thread::spawn(move || {
            let mut idx = 0usize;
            while !stop_c.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                };
                let _ = (stream.set_nonblocking(false), stream.read(&mut [0u8; 2048]));
                hits_c.fetch_add(1, Ordering::SeqCst);
                if let Some(raw) = responses.get(idx).or_else(|| responses.last()) {
                    let _ = stream.write_all(raw).and_then(|_| stream.flush());
                }
                idx += 1;
            }
        });
        Self {
            addr,
            hits,
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for StubTorrentHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn http_torrent_response(bytes: &[u8]) -> Vec<u8> {
    let mut resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/x-bittorrent\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    )
    .into_bytes();
    resp.extend_from_slice(bytes);
    resp
}

fn dummy_op_record(op_id: &str) -> DownloadOperationRecord {
    DownloadOperationRecord {
        operation_id: op_id.into(),
        idempotency_key: format!("idem-{op_id}"),
        source_kind: "manual".into(),
        subscribe_id: None,
        plugin_id: None,
        subject: "user:test".into(),
        cause_id: "cause".into(),
        site_id: None,
        downloader_id: None,
        endpoint_fingerprint: None,
        enclosure: "https://example.invalid/1.torrent".into(),
        ownership_tag: format!("cm-op:{op_id}"),
        info_hash: None,
        bytes_digest: None,
        verified_bytes: None,
        request_digest: "digest".into(),
        state: DownloadOperationState::Intent,
        outcome_code: None,
        error_message: None,
        revision: 1,
        created_at: 0,
        updated_at: 0,
        submitted_at: None,
        reconciled_at: None,
    }
}

#[test]
fn torrent_fetch_allows_registered_enabled_site_and_blocks_unregistered_self_and_redirects() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = crate::Store::open(dir.path().join("data")).unwrap();
    let dl = Arc::new(downloader::MemoryDownloader::new(dir.path().join("stage")));
    let self_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let self_addr = self_listener.local_addr().unwrap();
    let state = ApiState::new(
        store,
        "tok".into(),
        indexer::RegistrySnapshot::builtin().unwrap(),
        Arc::new(StubNoopFetcher),
        dl,
    )
    .unwrap()
    .with_self_listen(self_addr);

    let (torrent_bytes, expected_hash) = minimal_torrent_bytes("Loopback.PT", 2048);
    let unreg_srv = StubTorrentHttpServer::spawn(vec![http_torrent_response(&torrent_bytes)]);
    let reg_srv = StubTorrentHttpServer::spawn(vec![
        http_torrent_response(&torrent_bytes),
        http_torrent_response(&torrent_bytes),
        format!(
            "HTTP/1.1 302 Found\r\nLocation: http://{}/unreg.torrent\r\nContent-Length: 0\r\n\r\n",
            unreg_srv.addr
        )
        .into_bytes(),
    ]);
    let disabled_srv = StubTorrentHttpServer::spawn(vec![http_torrent_response(&torrent_bytes)]);

    let enabled_site = sample_site("pterclub", &format!("http://{}/", reg_srv.addr));
    state.store.insert_site(&enabled_site).unwrap();
    let mut disabled_site = sample_site("pterclub", &format!("http://{}/", disabled_srv.addr));
    disabled_site.enabled = false;
    state.store.insert_site(&disabled_site).unwrap();
    for bad_url in [
        "http://169.254.169.254/".to_string(),
        "http://0.0.0.0:18080/".to_string(),
        format!("http://{self_addr}/"),
    ] {
        state
            .store
            .insert_site(&sample_site("pterclub", &bad_url))
            .unwrap();
    }

    let dl_id = DownloaderId::new();
    let ok_direct = fetch_and_prepare_payload(
        &state,
        &sample_plan(
            enabled_site.id,
            &format!("http://{}/1.torrent", reg_srv.addr),
        ),
        Some(&enabled_site),
        &dummy_op_record("op-direct"),
        dl_id,
        None,
        0,
    )
    .unwrap();
    assert_eq!(ok_direct.info_hash, expected_hash);

    let ok_lookup = fetch_and_prepare_payload(
        &state,
        &sample_plan(SiteId::new(), &format!("http://{}/1.torrent", reg_srv.addr)),
        None,
        &dummy_op_record("op-lookup"),
        dl_id,
        None,
        0,
    )
    .unwrap();
    assert_eq!(ok_lookup.info_hash, expected_hash);

    let redir_err = fetch_and_prepare_payload(
        &state,
        &sample_plan(enabled_site.id, &format!("http://{}/redir", reg_srv.addr)),
        Some(&enabled_site),
        &dummy_op_record("op-redir"),
        dl_id,
        None,
        0,
    )
    .err()
    .expect("redirect to unregistered loopback must fail");
    assert!(redir_err.to_string().contains("blocked"), "{redir_err}");

    for (idx, (url, site_ref)) in [
        (format!("http://{}/x.torrent", unreg_srv.addr), None),
        (
            format!("http://{}/x.torrent", disabled_srv.addr),
            Some(&disabled_site),
        ),
        ("http://169.254.169.254/x.torrent".to_string(), None),
        ("http://0.0.0.0:18080/x.torrent".to_string(), None),
        (format!("http://{self_addr}/x.torrent"), None),
    ]
    .into_iter()
    .enumerate()
    {
        let err = fetch_and_prepare_payload(
            &state,
            &sample_plan(site_ref.map_or_else(SiteId::new, |s| s.id), &url),
            site_ref,
            &dummy_op_record(&format!("op-bad-{idx}")),
            dl_id,
            None,
            0,
        )
        .err()
        .unwrap_or_else(|| panic!("expected blocked error for {url}"));
        assert!(err.to_string().contains("blocked"), "{url}: {err}");
    }

    assert_eq!(unreg_srv.hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        disabled_srv.hits.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[test]
fn resolve_source_resolves_plugin_ref_and_rejects_unresolved() {
    let site = sample_site("stub_dlref", "https://site.example");
    let st = test_state_with_resolved(
        "stub_dlref",
        ResolvedDownload::Url("https://site.example/download.torrent".into()),
    );
    let mut plan = sample_plan(site.id, "https://site.example/placeholder");
    plan.torrent.enclosure = "dlref-1".into();
    plan.source = TorrentSource::PluginRef("dlref-1".into());

    let resolved = resolve_source(&st, &plan, Some(&site)).unwrap();
    assert!(
        matches!(resolved, TorrentSource::Http(ref h) if h.url == "https://site.example/download.torrent")
    );

    let no_site_err = resolve_source(&st, &plan, None).unwrap_err();
    assert!(
        no_site_err.to_string().contains("download_ref"),
        "{no_site_err}"
    );

    let builtin_site = sample_site("demo", "https://site.example");
    plan.torrent.site_id = builtin_site.id;
    let unresolved_err = resolve_source(&st, &plan, Some(&builtin_site)).unwrap_err();
    assert!(
        unresolved_err.to_string().contains("download_ref"),
        "{unresolved_err}"
    );
}
