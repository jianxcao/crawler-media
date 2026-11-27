use alloc::{format, string::String, vec, vec::Vec};

wit_bindgen::generate!({
    path: "../../crates/plugin-api/wit",
    world: "indexer-plugin",
    pub_export_macro: true,
});

use crawler::plugin::{blob, browser, config, http, kv, site, types};
use exports::crawler::plugin::{indexer, lifecycle};

#[global_allocator]
static ALLOCATOR: dlmalloc::GlobalDlmalloc = dlmalloc::GlobalDlmalloc;

// SAFETY: the C ABI caller supplies two valid readable regions of `len` bytes.
#[unsafe(no_mangle)]
unsafe extern "C" fn memcmp(
    a: *const core::ffi::c_void,
    b: *const core::ffi::c_void,
    len: usize,
) -> i32 {
    let (a, b) = (a.cast::<u8>(), b.cast::<u8>());
    for index in 0..len {
        // SAFETY: the caller's regions cover every index in this loop.
        let (a, b) = unsafe { (a.add(index).read_volatile(), b.add(index).read_volatile()) };
        if a != b {
            return i32::from(a) - i32::from(b);
        }
    }
    0
}

// SAFETY: the canonical ABI guarantees a valid prior allocation and alignment.
#[unsafe(no_mangle)]
unsafe extern "C" fn cabi_realloc(ptr: *mut u8, old: usize, align: usize, new: usize) -> *mut u8 {
    if old == 0 && new == 0 {
        return align as *mut u8;
    }
    let layout = match core::alloc::Layout::from_size_align(old.max(new), align) {
        Ok(layout) => layout,
        Err(_) => core::arch::wasm32::unreachable(),
    };
    // SAFETY: validated layout and canonical ABI ownership of the prior allocation.
    let result = unsafe {
        if old == 0 {
            alloc::alloc::alloc(layout)
        } else {
            let old_layout = core::alloc::Layout::from_size_align_unchecked(old, align);
            alloc::alloc::realloc(ptr, old_layout, new)
        }
    };
    if result.is_null() {
        core::arch::wasm32::unreachable();
    }
    result
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    core::arch::wasm32::unreachable()
}

struct IndexerFixture;

fn err(code: types::ErrorCode, msg: &str) -> types::ApiError {
    types::ApiError {
        code,
        message: String::from(msg),
        detail: None,
        retry_after_ms: None,
        operation_id: None,
    }
}

fn simple_torrent(title: String, download_ref: &str) -> types::TorrentRaw {
    types::TorrentRaw {
        provider_local_id: String::from("item-1"),
        title,
        size: 1024,
        download_ref: String::from(download_ref),
        seeders: Some(5),
        leechers: Some(1),
        published_at: Some(1_700_000_000),
        category: Some(String::from("movies")),
        free: Some(true),
        hr: Some(false),
    }
}

fn single_batch(torrent: types::TorrentRaw) -> types::TorrentBatch {
    types::TorrentBatch {
        items: vec![torrent],
        next_cursor: None,
        has_more: false,
    }
}

fn handle_http_cases(kw: &str) -> Result<Option<types::TorrentBatch>, types::ApiError> {
    if let Some(batch) = handle_http_single_cases(kw)? {
        return Ok(Some(batch));
    }
    handle_http_batch_cases(kw)
}

fn handle_http_single_cases(kw: &str) -> Result<Option<types::TorrentBatch>, types::ApiError> {
    match kw {
        "http-fetch" => {
            let resp = http::fetch(&types::HttpRequest {
                method: String::from("GET"),
                target: types::HttpTarget::PublicUrl(String::from(
                    "https://tracker.invalid/search",
                )),
                headers: Vec::new(),
                body: None,
                max_bytes: 4096,
            })?;
            for h in &resp.headers {
                let lower = h.name.to_ascii_lowercase();
                if matches!(
                    lower.as_str(),
                    "set-cookie" | "cookie" | "authorization" | "proxy-authorization" | "x-api-key"
                ) {
                    return Err(err(
                        types::ErrorCode::Conflict,
                        "leaked sensitive response header",
                    ));
                }
            }
            let body = String::from_utf8_lossy(&resp.body);
            Ok(Some(single_batch(simple_torrent(
                format!("http:{body}"),
                "dl-1",
            ))))
        }
        "site-auth-http" => {
            let resp = http::fetch(&types::HttpRequest {
                method: String::from("POST"),
                target: types::HttpTarget::SiteRelative(types::SiteTarget {
                    path: String::from("/api/search"),
                    query: vec![types::Header {
                        name: String::from("q"),
                        value: String::from("matrix"),
                    }],
                    auth_slot: Some(String::from("site-cookie")),
                }),
                headers: Vec::new(),
                body: Some(b"hello".to_vec()),
                max_bytes: 4096,
            })?;
            let body = String::from_utf8_lossy(&resp.body);
            Ok(Some(single_batch(simple_torrent(
                format!("auth:{body}"),
                "dl-auth",
            ))))
        }
        "forbidden-header" => {
            let _ = http::fetch(&types::HttpRequest {
                method: String::from("GET"),
                target: types::HttpTarget::PublicUrl(String::from("https://tracker.invalid/")),
                headers: vec![types::Header {
                    name: String::from("Cookie"),
                    value: String::from("spoof=1"),
                }],
                body: None,
                max_bytes: 4096,
            })?;
            Err(err(
                types::ErrorCode::Conflict,
                "expected forbidden header error",
            ))
        }
        "forbidden-set-cookie-header" => {
            let _ = http::fetch(&types::HttpRequest {
                method: String::from("GET"),
                target: types::HttpTarget::PublicUrl(String::from("https://tracker.invalid/")),
                headers: vec![types::Header {
                    name: String::from("Set-Cookie"),
                    value: String::from("spoof=1"),
                }],
                body: None,
                max_bytes: 4096,
            })?;
            Err(err(
                types::ErrorCode::Conflict,
                "expected forbidden set-cookie header error",
            ))
        }
        "site-auth-http-other-upper"
        | "site-auth-http-other-mixed"
        | "site-auth-http-wrong-case-site"
        | "site-auth-http-same-upper" => {
            let slot = match kw {
                "site-auth-http-other-upper" => "SITE:other-site",
                "site-auth-http-other-mixed" => "Site:other-site",
                "site-auth-http-wrong-case-site" => "SITE:Site-a",
                _ => "SITE:site-a",
            };
            let resp = http::fetch(&types::HttpRequest {
                method: String::from("GET"),
                target: types::HttpTarget::SiteRelative(types::SiteTarget {
                    path: String::from("/api/search"),
                    query: Vec::new(),
                    auth_slot: Some(String::from(slot)),
                }),
                headers: Vec::new(),
                body: None,
                max_bytes: 4096,
            })?;
            let body = String::from_utf8_lossy(&resp.body);
            Ok(Some(single_batch(simple_torrent(
                format!("auth-slot:{body}"),
                "dl-auth-slot",
            ))))
        }
        _ => Ok(None),
    }
}

fn handle_http_batch_cases(kw: &str) -> Result<Option<types::TorrentBatch>, types::ApiError> {
    match kw {
        "fetch-many" => {
            let reqs: Vec<_> = ["1", "2", "3"]
                .into_iter()
                .map(|p| types::HttpRequest {
                    method: String::from("GET"),
                    target: types::HttpTarget::PublicUrl(format!("https://tracker.invalid/{p}")),
                    headers: Vec::new(),
                    body: None,
                    max_bytes: 1024,
                })
                .collect();
            let res = http::fetch_many(&reqs);
            let mut parts = Vec::new();
            for item in res {
                let r = item?;
                parts.push(String::from(String::from_utf8_lossy(&r.body).as_ref()));
            }
            Ok(Some(single_batch(simple_torrent(
                format!("many:{}", parts.join("|")),
                "dl-many",
            ))))
        }
        "fetch-many-empty" => {
            let res = http::fetch_many(&[]);
            if let Some(first) = res.into_iter().next() {
                first?;
            }
            Err(err(
                types::ErrorCode::Conflict,
                "expected empty fetch-many error",
            ))
        }
        "fetch-many-too-many" => {
            let reqs: Vec<_> = (0..9)
                .map(|i| types::HttpRequest {
                    method: String::from("GET"),
                    target: types::HttpTarget::PublicUrl(format!("https://tracker.invalid/{i}")),
                    headers: Vec::new(),
                    body: None,
                    max_bytes: 1024,
                })
                .collect();
            let res = http::fetch_many(&reqs);
            if let Some(first) = res.into_iter().next() {
                first?;
            }
            Err(err(
                types::ErrorCode::Conflict,
                "expected 9-item fetch-many error",
            ))
        }
        "http-quota" => {
            for i in 0..16 {
                http::fetch(&types::HttpRequest {
                    method: String::from("GET"),
                    target: types::HttpTarget::PublicUrl(format!("https://tracker.invalid/{i}")),
                    headers: Vec::new(),
                    body: None,
                    max_bytes: 1024,
                })?;
            }
            Ok(Some(single_batch(simple_torrent(
                String::from("no-quota-hit"),
                "dl-q",
            ))))
        }
        _ => Ok(None),
    }
}

fn handle_browser_cases(kw: &str) -> Result<Option<types::TorrentBatch>, types::ApiError> {
    match kw {
        "browser-search" => {
            let sess = browser::open(browser::Purpose::Search)?;
            let receipt = sess.navigate("/search", "load")?;
            let els = sess.query(".row", 10)?;
            let el = els
                .first()
                .ok_or_else(|| err(types::ErrorCode::NotFound, "missing element"))?;
            let text = sess.text(&el.handle, 1024)?;
            let href = sess.attribute(&el.handle, "href")?.unwrap_or_default();
            sess.fill(
                &el.handle,
                &browser::FillValue::Text(String::from("matrix")),
            )?;
            let _wait = sess.wait_for(Some(".done"), "visible", 500)?;
            let resp = sess.read_response(&receipt, 4096)?;
            for h in &resp.headers {
                if h.name.eq_ignore_ascii_case("set-cookie") {
                    return Err(err(
                        types::ErrorCode::Conflict,
                        "leaked set-cookie in browser response",
                    ));
                }
            }
            sess.close()?;
            Ok(Some(single_batch(simple_torrent(
                format!("browser:{text}:{href}"),
                "dl-b",
            ))))
        }
        "browser-stale-after-nav" => {
            let sess = browser::open(browser::Purpose::Search)?;
            let _ = sess.navigate("/a", "load")?;
            let els = sess.query(".row", 5)?;
            let handle = els.first().map(|e| e.handle.clone()).unwrap_or_default();
            let _ = sess.navigate("/b", "load")?;
            let _ = sess.text(&handle, 512)?;
            Err(err(types::ErrorCode::Conflict, "expected stale-handle"))
        }
        "browser-stale-after-click" => {
            let sess = browser::open(browser::Purpose::Search)?;
            let _ = sess.navigate("/a", "load")?;
            let els = sess.query(".row", 5)?;
            let handle = els.first().map(|e| e.handle.clone()).unwrap_or_default();
            let _ = sess.click(&handle)?;
            let _ = sess.attribute(&handle, "href")?;
            Err(err(types::ErrorCode::Conflict, "expected stale-handle"))
        }
        "browser-forged-receipt" => {
            let sess = browser::open(browser::Purpose::Search)?;
            let _ = sess.read_response("forged-receipt", 512)?;
            Err(err(
                types::ErrorCode::Conflict,
                "expected stale-handle on receipt",
            ))
        }
        "browser-capture-forbidden" => {
            let sess = browser::open(browser::Purpose::Search)?;
            let _ = sess.capture_session()?;
            Err(err(
                types::ErrorCode::Conflict,
                "expected wrong-context on capture",
            ))
        }
        "browser-double-close" => {
            let sess = browser::open(browser::Purpose::Search)?;
            sess.close()?;
            sess.close()?;
            Ok(Some(single_batch(simple_torrent(
                String::from("closed-twice"),
                "dl-c",
            ))))
        }
        "browser-use-after-close" => {
            let sess = browser::open(browser::Purpose::Search)?;
            sess.close()?;
            let _ = sess.dom(512)?;
            Err(err(
                types::ErrorCode::Conflict,
                "expected stale-handle after close",
            ))
        }
        "browser-trap" => {
            let _sess = browser::open(browser::Purpose::Search)?;
            core::arch::wasm32::unreachable();
        }
        _ => handle_browser_validation_cases(kw),
    }
}

fn handle_browser_validation_cases(
    kw: &str,
) -> Result<Option<types::TorrentBatch>, types::ApiError> {
    let target = match kw {
        "browser-nav-external" => Some("https://evil.com/phish"),
        "browser-nav-proto-rel" => Some("//evil.com/phish"),
        "browser-nav-js" => Some("javascript:alert(1)"),
        "browser-nav-space" => Some("/search?q=a b"),
        _ => None,
    };
    if let Some(url) = target {
        let sess = browser::open(browser::Purpose::Search)?;
        let _ = sess.navigate(url, "load")?;
        return Err(err(types::ErrorCode::Conflict, "expected nav error"));
    }
    let attr_name = match kw {
        "browser-attr-on" => Some("onclick"),
        "browser-attr-empty" => Some("   "),
        "browser-attr-data" => Some("data-id"),
        _ => None,
    };
    if let Some(name) = attr_name {
        let sess = browser::open(browser::Purpose::Search)?;
        let _ = sess.navigate("/search", "load")?;
        let els = sess.query(".row", 5)?;
        let handle = els.first().map(|e| e.handle.clone()).unwrap_or_default();
        let val = sess.attribute(&handle, name)?.unwrap_or_default();
        sess.close()?;
        return Ok(Some(single_batch(simple_torrent(
            format!("attr:{val}"),
            "dl-attr",
        ))));
    }
    Ok(None)
}

impl lifecycle::Guest for IndexerFixture {
    fn validate(config_schema: String, _state_schema: u32) -> Result<(), types::ApiError> {
        if config_schema.contains("reject") {
            return Err(err(types::ErrorCode::InvalidArgument, "rejected by guest"));
        }
        if config_schema.contains("wrong-context-kv-put") {
            return match kv::put("k", b"v", None) {
                Ok(_) => Err(err(types::ErrorCode::Conflict, "expected wrong-context")),
                Err(e) => Err(e),
            };
        }
        if config_schema.contains("wrong-context-http") {
            return match http::fetch(&types::HttpRequest {
                method: String::from("GET"),
                target: types::HttpTarget::PublicUrl(String::from("https://tracker.invalid/")),
                headers: Vec::new(),
                body: None,
                max_bytes: 512,
            }) {
                Ok(_) => Err(err(types::ErrorCode::Conflict, "expected wrong-context")),
                Err(e) => Err(e),
            };
        }
        if config_schema.contains("wrong-context-browser") {
            return match browser::open(browser::Purpose::Search) {
                Ok(_) => Err(err(types::ErrorCode::Conflict, "expected wrong-context")),
                Err(e) => Err(e),
            };
        }
        Ok(())
    }

    fn health() -> Result<lifecycle::HealthState, types::ApiError> {
        if let Ok(cfg) = config::get()
            && cfg.validated_json.contains("degrade")
        {
            return Ok(lifecycle::HealthState::Degraded(String::from(
                "degraded by config",
            )));
        }
        Ok(lifecycle::HealthState::Healthy)
    }
}

impl indexer::Guest for IndexerFixture {
    fn describe() -> Vec<types::Descriptor> {
        vec![types::Descriptor {
            id: String::from("fixture"),
            name: String::from("Fixture Indexer"),
            kind: String::from("pt"),
            capabilities: vec![
                String::from("search"),
                String::from("rss"),
                String::from("detail"),
                String::from("resolve-download"),
                String::from("user-stats"),
                String::from("categories"),
            ],
        }]
    }

    fn search(query: types::SearchQuery) -> Result<types::TorrentBatch, types::ApiError> {
        let kw = query.keyword.as_str();
        if kw == "loop" {
            loop {
                core::hint::spin_loop();
            }
        }
        if let Some(batch) = handle_http_cases(kw)? {
            return Ok(batch);
        }
        if let Some(batch) = handle_browser_cases(kw)? {
            return Ok(batch);
        }
        match kw {
            "site-views" => {
                let page = site::list(&types::PageRequest {
                    cursor: None,
                    limit: 10,
                })?;
                let first_id = page.items.first().map(|s| s.id.clone()).unwrap_or_default();
                let view = site::get(&first_id)?;
                let stats = site::stats(&first_id)?;
                Ok(single_batch(simple_torrent(
                    format!(
                        "site:{}:{}:{}",
                        view.name,
                        stats.uploaded.unwrap_or(0),
                        page.items.len()
                    ),
                    "dl-site",
                )))
            }
            "site-get-other" => {
                let _ = site::get("other-site")?;
                Err(err(
                    types::ErrorCode::Conflict,
                    "expected permission-denied",
                ))
            }
            "kv-write" => {
                kv::put("scoped-key", b"scoped-val", None)?;
                let got = kv::get("scoped-key")?.map(|e| e.value).unwrap_or_default();
                Ok(single_batch(simple_torrent(
                    format!("kv:{}", String::from_utf8_lossy(&got)),
                    "dl-kv",
                )))
            }
            "kv-read" => {
                let got = kv::get("scoped-key")?
                    .map(|e| String::from(String::from_utf8_lossy(&e.value).as_ref()))
                    .unwrap_or_else(|| String::from("none"));
                Ok(single_batch(simple_torrent(
                    format!("kv-read:{got}"),
                    "dl-kvr",
                )))
            }
            "blob-put" => {
                let _ = blob::put("b", b"val", None)?;
                Err(err(
                    types::ErrorCode::Conflict,
                    "expected wrong-context for blob.put",
                ))
            }
            _ => {
                let dl_ref = if kw.starts_with("dl-case:") {
                    kw
                } else {
                    "raw-dl-1"
                };
                Ok(single_batch(simple_torrent(format!("hit:{kw}"), dl_ref)))
            }
        }
    }

    fn rss(cursor: Option<String>) -> Result<types::TorrentBatch, types::ApiError> {
        let label = cursor.unwrap_or_else(|| String::from("head"));
        Ok(single_batch(simple_torrent(
            format!("rss:{label}"),
            "raw-rss-1",
        )))
    }

    fn detail(torrent_ref: String) -> Result<types::TorrentRaw, types::ApiError> {
        Ok(simple_torrent(
            format!("detail:{torrent_ref}"),
            &format!("raw-detail-{torrent_ref}"),
        ))
    }

    fn resolve_download(download_ref: String) -> Result<types::HttpRequest, types::ApiError> {
        Ok(resolve_download_case(&download_ref))
    }

    fn user_stats() -> Result<types::StatsView, types::ApiError> {
        Ok(types::StatsView {
            uploaded: Some(1_000_000),
            downloaded: Some(250_000),
        })
    }

    fn categories() -> Result<Vec<types::Category>, types::ApiError> {
        Ok(vec![
            types::Category {
                id: String::from("401"),
                name: String::from("Movies"),
            },
            types::Category {
                id: String::from("402"),
                name: String::from("TV"),
            },
        ])
    }
}

fn site_dl(
    path: &str,
    query: Vec<types::Header>,
    auth_slot: Option<&str>,
    headers: Vec<types::Header>,
) -> types::HttpRequest {
    types::HttpRequest {
        method: String::from("GET"),
        target: types::HttpTarget::SiteRelative(types::SiteTarget {
            path: String::from(path),
            query,
            auth_slot: auth_slot.map(String::from),
        }),
        headers,
        body: None,
        max_bytes: 10 * 1024 * 1024,
    }
}

fn hdr(name: &str, value: &str) -> types::Header {
    types::Header {
        name: String::from(name),
        value: String::from(value),
    }
}

fn resolve_download_case(download_ref: &str) -> types::HttpRequest {
    match download_ref {
        "dl-case:other-site-auth" => site_dl(
            "/download/1",
            Vec::new(),
            Some("site:other-site"),
            Vec::new(),
        ),
        "dl-case:other-site-auth-upper" => site_dl(
            "/download/1",
            Vec::new(),
            Some("SITE:other-site"),
            Vec::new(),
        ),
        "dl-case:other-site-auth-mixed" => site_dl(
            "/download/1",
            Vec::new(),
            Some("Site:other-site"),
            Vec::new(),
        ),
        "dl-case:empty-auth-slot" => site_dl("/download/1", Vec::new(), Some("  "), Vec::new()),
        "dl-case:query-escape" => site_dl(
            "/download.php",
            vec![hdr("id", "9"), hdr("token", "1&admin=1#frag\r\n ")],
            Some("site-cookie"),
            Vec::new(),
        ),
        "dl-case:forbidden-header" => site_dl(
            "/download/1",
            Vec::new(),
            None,
            vec![hdr("Cookie", "spoof=1")],
        ),
        "dl-case:set-cookie-header" => site_dl(
            "/download/1",
            Vec::new(),
            None,
            vec![hdr("Set-Cookie", "spoof=1")],
        ),
        "dl-case:crlf-header" => site_dl(
            "/download/1",
            Vec::new(),
            None,
            vec![hdr("X-Custom", "ok\r\nEvil: 1")],
        ),
        "dl-case:bad-path-scheme" => site_dl("https://evil.com/p", Vec::new(), None, Vec::new()),
        "dl-case:bad-path-slash" => site_dl("//evil.com/p", Vec::new(), None, Vec::new()),
        "dl-case:bad-path-backslash" => site_dl("/dl\\evil", Vec::new(), None, Vec::new()),
        "dl-case:bad-path-space" => site_dl("/dl evil", Vec::new(), None, Vec::new()),
        "dl-case:bad-path-query" => site_dl(
            "/download.php?passkey=stolen&admin=1",
            Vec::new(),
            Some("site-cookie"),
            Vec::new(),
        ),
        "dl-case:bad-path-js" => site_dl("javascript:alert(1)", Vec::new(), None, Vec::new()),
        "dl-case:bad-path-http-colon" => site_dl("http:evil", Vec::new(), None, Vec::new()),
        "dl-case:unauth-site-rel" => {
            site_dl("/public/1.torrent", vec![hdr("id", "9")], None, Vec::new())
        }
        "dl-case:public-https"
        | "dl-case:public-file"
        | "dl-case:public-whitespace"
        | "dl-case:public-empty-host-port"
        | "dl-case:public-backslash"
        | "dl-case:public-forbidden-header"
        | "dl-case:public-userinfo"
        | "dl-case:public-authority-confusion"
        | "dl-case:public-empty-userinfo"
        | "dl-case:public-invalid-port"
        | "dl-case:public-empty-ipv6"
        | "dl-case:public-ipv6-ok"
        | "dl-case:public-port-ok"
        | "dl-case:public-ipv6-port-ok" => {
            let url = match download_ref {
                "dl-case:public-file" => "file:///etc/passwd",
                "dl-case:public-whitespace" => "https://cdn.example.com/a b.torrent",
                "dl-case:public-empty-host-port" => "https://:8080/a.torrent",
                "dl-case:public-backslash" => "https://cdn.example\\a.torrent",
                "dl-case:public-userinfo" => "https://user:pass@cdn.example/a.torrent",
                "dl-case:public-authority-confusion" => "https://trusted.com@evil.com/a.torrent",
                "dl-case:public-empty-userinfo" => "https://@cdn.example/a.torrent",
                "dl-case:public-invalid-port" => "https://cdn.example:abc/a.torrent",
                "dl-case:public-empty-ipv6" => "https://[]/a.torrent",
                "dl-case:public-ipv6-ok" => "https://[::1]/a.torrent",
                "dl-case:public-port-ok" => "https://cdn.example:8443/a.torrent",
                "dl-case:public-ipv6-port-ok" => "https://[::1]:8443/a.torrent",
                _ => "https://cdn.example.com/a.torrent",
            };
            let headers = if download_ref == "dl-case:public-forbidden-header" {
                vec![hdr("Set-Cookie", "a=1")]
            } else {
                Vec::new()
            };
            types::HttpRequest {
                method: String::from("GET"),
                target: types::HttpTarget::PublicUrl(String::from(url)),
                headers,
                body: None,
                max_bytes: 10 * 1024 * 1024,
            }
        }
        _ => site_dl(
            &format!("/download/{download_ref}"),
            Vec::new(),
            Some("site-cookie"),
            Vec::new(),
        ),
    }
}

export!(IndexerFixture);
