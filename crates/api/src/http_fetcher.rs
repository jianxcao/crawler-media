use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;

use indexer::{FetchMethod, FetchRequest, Fetcher, IndexerError};
use net_guard::{CredentialSlot, GuardedMethod, GuardedRequest, NetGuardClient, TargetPolicy};

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

#[derive(Clone, Default)]
pub struct HttpFetcher {
    client: NetGuardClient,
    policy: TargetPolicy,
    store: Option<Arc<store::Store>>,
}

impl fmt::Debug for HttpFetcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpFetcher")
            .field("client", &self.client)
            .field("policy", &self.policy)
            .field("has_store", &self.store.is_some())
            .finish()
    }
}

impl HttpFetcher {
    pub fn new() -> Self {
        Self {
            client: NetGuardClient::new(),
            policy: TargetPolicy::production(),
            store: None,
        }
    }

    pub fn with_self_listen(self_addr: SocketAddr) -> Self {
        Self {
            client: NetGuardClient::new(),
            policy: TargetPolicy::production().with_blocked_self(self_addr),
            store: None,
        }
    }

    pub fn with_policy(policy: TargetPolicy) -> Self {
        Self {
            client: NetGuardClient::new(),
            policy,
            store: None,
        }
    }

    pub fn with_store(store: Arc<store::Store>) -> Self {
        Self {
            client: NetGuardClient::new(),
            policy: TargetPolicy::production(),
            store: Some(store),
        }
    }

    pub fn with_store_and_self_listen(store: Arc<store::Store>, self_addr: SocketAddr) -> Self {
        Self {
            client: NetGuardClient::new(),
            policy: TargetPolicy::production().with_blocked_self(self_addr),
            store: Some(store),
        }
    }

    fn policy_for_request(&self, url: &str) -> TargetPolicy {
        let mut policy = self.policy.clone();
        let Some(store) = &self.store else {
            return policy;
        };
        let Ok(target_norm) = net_guard::normalize_url(url, true) else {
            return policy;
        };
        let sites = match store.list_enabled_sites() {
            Ok(sites) => sites,
            Err(err) => {
                tracing::warn!(%err, "failed to load enabled sites for HttpFetcher policy");
                return policy;
            }
        };
        for site in &sites {
            let (next_policy, matched) =
                attach_site_private_exception_for_origin(policy, &site.url, &target_norm.origin);
            policy = next_policy;
            if matched {
                break;
            }
        }
        policy
    }
}

/// Build a [`net_guard::PrivateEndpointException`] for a non-global IP origin
/// when it is safe to exempt (neither cloud metadata, unspecified, nor self-blocked).
pub(crate) fn private_endpoint_exception_for_norm(
    norm: &net_guard::NormalizedTarget,
    policy: &TargetPolicy,
    path_prefixes: Vec<String>,
) -> Option<net_guard::PrivateEndpointException> {
    let ip = norm.host_is_ip?;
    if net_guard::is_global_unicast(ip)
        || net_guard::is_cloud_metadata_ip(ip)
        || net_guard::canonicalize_ip(ip).is_unspecified()
    {
        return None;
    }
    let sock = SocketAddr::new(ip, norm.port);
    if policy.is_self_blocked(sock) {
        return None;
    }
    Some(net_guard::PrivateEndpointException {
        host: norm.host_ascii.clone(),
        port: norm.port,
        allowed_ips: vec![ip],
        path_prefixes,
    })
}

/// Attach a [`net_guard::PrivateEndpointException`] for `site_url` to `policy`
/// when its normalized origin matches `target_origin` and passes safety checks.
pub(crate) fn attach_site_private_exception_for_origin(
    mut policy: TargetPolicy,
    site_url: &str,
    target_origin: &net_guard::Origin,
) -> (TargetPolicy, bool) {
    let Ok(site_norm) = net_guard::normalize_url(site_url, true) else {
        return (policy, false);
    };
    if &site_norm.origin != target_origin {
        return (policy, false);
    }
    if let Some(ex) = private_endpoint_exception_for_norm(&site_norm, &policy, vec!["/".into()]) {
        policy = policy.with_private_exception(ex);
    }
    (policy, true)
}

impl Fetcher for HttpFetcher {
    fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
        let is_api_key = request
            .api_key
            .as_deref()
            .is_some_and(|key| !key.trim().is_empty());
        let policy = self.policy_for_request(&request.url);
        let mut headers = vec![("User-Agent".to_string(), USER_AGENT.to_string())];
        let (method, body) = match request.method {
            FetchMethod::Get => (GuardedMethod::Get, None),
            FetchMethod::PostJson => {
                headers.push(("Accept".to_string(), "application/json".to_string()));
                headers.push(("Content-Type".to_string(), "application/json".to_string()));
                (
                    GuardedMethod::Post,
                    Some(request.body.as_deref().unwrap_or("").as_bytes().to_vec()),
                )
            }
            FetchMethod::PostForm => {
                headers.push(("Accept".to_string(), "application/json".to_string()));
                headers.push((
                    "Content-Type".to_string(),
                    "application/x-www-form-urlencoded".to_string(),
                ));
                (
                    GuardedMethod::Post,
                    Some(request.body.as_deref().unwrap_or("").as_bytes().to_vec()),
                )
            }
        };

        let mut auth_headers = Vec::new();
        if let Some(cookie) = &request.cookie {
            auth_headers.push(("Cookie".to_string(), cookie.clone()));
        }
        if let Some(key) = &request.api_key {
            auth_headers.push(("x-api-key".to_string(), key.clone()));
        }
        let creds = if auth_headers.is_empty() {
            None
        } else {
            let target = policy
                .normalize_url(&request.url)
                .map_err(|e| IndexerError::Fetch(e.to_string()))?;
            Some(CredentialSlot::new(
                request.key.clone(),
                target.origin,
                "site-fetch",
                auth_headers,
            ))
        };

        let guarded = GuardedRequest {
            method,
            url: request.url.clone(),
            headers,
            body,
            proxy: request.proxy.clone(),
            max_response_bytes: None,
        };
        let response = self
            .client
            .execute(guarded, &policy, creds.as_ref())
            .map_err(|e| {
                if e.is_post_send_unknown() {
                    IndexerError::UnknownOutcome(e.to_string())
                } else {
                    IndexerError::Fetch(e.to_string())
                }
            })?;
        let body_str = String::from_utf8_lossy(&response.body).into_owned();
        if response.status >= 400 {
            let has_api_error_envelope = is_api_key
                && !request.key.starts_with("rss:")
                && has_dialect_error_envelope(&body_str);
            if !has_api_error_envelope {
                return Err(IndexerError::Fetch(format!(
                    "http status: {}",
                    response.status
                )));
            }
        } else if !response.is_success() {
            return Err(IndexerError::Fetch(format!(
                "http status: {}",
                response.status
            )));
        }
        Ok(body_str)
    }
}

fn has_dialect_error_envelope(body: &str) -> bool {
    let Ok(serde_json::Value::Object(obj)) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    let success_failed = match obj.get("success") {
        Some(serde_json::Value::Bool(false)) => true,
        Some(serde_json::Value::Number(n)) => n.as_i64() == Some(0),
        Some(serde_json::Value::String(s)) => {
            matches!(s.trim().to_ascii_lowercase().as_str(), "false" | "0" | "no")
        }
        _ => false,
    };
    let code_failed = match obj.get("code") {
        Some(serde_json::Value::String(s)) => {
            let t = s.trim();
            !t.is_empty() && t != "0"
        }
        Some(serde_json::Value::Number(n)) => n.to_string() != "0",
        _ => false,
    };
    success_failed || code_failed
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;

    fn spawn_one_shot_http(
        status_line: &'static str,
        content_type: &'static str,
        body: &'static str,
    ) -> (SocketAddr, String) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        (addr, format!("http://{addr}/api/v1/torrents"))
    }

    struct TestHttpServer {
        addr: SocketAddr,
        stop: Arc<AtomicBool>,
        requests: Arc<Mutex<Vec<String>>>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl TestHttpServer {
        fn spawn(responses: Vec<Vec<u8>>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let addr = listener.local_addr().unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let stop_flag = Arc::clone(&stop);
            let requests = Arc::new(Mutex::new(Vec::new()));
            let reqs_clone = Arc::clone(&requests);
            let handle = thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(5);
                for resp in responses {
                    loop {
                        if stop_flag.load(Ordering::SeqCst) || Instant::now() > deadline {
                            return;
                        }
                        match listener.accept() {
                            Ok((mut stream, _)) => {
                                let _ = stream.set_nonblocking(false);
                                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                                let mut buf = [0u8; 4096];
                                let n = stream.read(&mut buf).unwrap_or(0);
                                reqs_clone
                                    .lock()
                                    .unwrap()
                                    .push(String::from_utf8_lossy(&buf[..n]).into_owned());
                                let _ = stream.write_all(&resp);
                                let _ = stream.flush();
                                break;
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                thread::sleep(Duration::from_millis(5));
                            }
                            Err(_) => return,
                        }
                    }
                }
            });
            Self {
                addr,
                stop,
                requests,
                handle: Some(handle),
            }
        }

        fn take_requests(&mut self) -> Vec<String> {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
            self.requests.lock().unwrap().clone()
        }
    }

    impl Drop for TestHttpServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }

    fn base_req(url: String) -> FetchRequest {
        FetchRequest {
            key: "k".into(),
            url,
            method: FetchMethod::Get,
            body: None,
            cookie: None,
            api_key: None,
            proxy: None,
            cdp_url: None,
        }
    }

    #[test]
    fn api_key_request_preserves_scalar_dialect_error_envelopes() {
        for json_body in [
            r#"{"success":false,"message":"Invalid API Key"}"#,
            r#"{"success":0,"message":"Invalid API Key"}"#,
            r#"{"success":"false","message":"Invalid API Key"}"#,
            r#"{"success":"no","message":"Invalid API Key"}"#,
            r#"{"code":"401","message":"bad key"}"#,
            r#"{"code":401,"message":"bad key"}"#,
        ] {
            let (addr, url) =
                spawn_one_shot_http("401 Unauthorized", "application/json", json_body);
            let fetcher = HttpFetcher::with_policy(
                TargetPolicy::production().with_test_loopback_socket(addr),
            );
            let req = FetchRequest {
                key: "search:test".into(),
                url,
                method: FetchMethod::Get,
                body: None,
                cookie: None,
                api_key: Some("bad-key".into()),
                proxy: None,
                cdp_url: None,
            };
            assert_eq!(fetcher.fetch(&req).unwrap(), json_body);
        }

        let (rss_addr, rss_url) = spawn_one_shot_http(
            "401 Unauthorized",
            "application/json",
            r#"{"success":false,"message":"Invalid API Key"}"#,
        );
        let rss_fetcher = HttpFetcher::with_policy(
            TargetPolicy::production().with_test_loopback_socket(rss_addr),
        );
        let rss_req = FetchRequest {
            key: "rss:https://example.test/rss".into(),
            url: rss_url,
            method: FetchMethod::Get,
            body: None,
            cookie: None,
            api_key: Some("bad-key".into()),
            proxy: None,
            cdp_url: None,
        };
        assert_eq!(
            rss_fetcher.fetch(&rss_req).unwrap_err().to_string(),
            "http status: 401"
        );
    }

    #[test]
    fn api_key_request_rejects_non_error_or_non_scalar_http_errors() {
        for (status_line, content_type, body, expected_err) in [
            (
                "401 Unauthorized",
                "application/json",
                r#"{"status":401,"error":"Unauthorized"}"#,
                "http status: 401",
            ),
            (
                "502 Bad Gateway",
                "application/json",
                r#"{"error":"Bad Gateway"}"#,
                "http status: 502",
            ),
            (
                "502 Bad Gateway",
                "application/json",
                r#"{"code":[]}"#,
                "http status: 502",
            ),
            (
                "502 Bad Gateway",
                "application/json",
                r#"{"code":{}}"#,
                "http status: 502",
            ),
            (
                "500 Internal Server Error",
                "application/json",
                r#"{"code":false}"#,
                "http status: 500",
            ),
            (
                "500 Internal Server Error",
                "application/json",
                r#"{"code":""}"#,
                "http status: 500",
            ),
            (
                "500 Internal Server Error",
                "application/json",
                r#"{"code":"   "}"#,
                "http status: 500",
            ),
            (
                "500 Internal Server Error",
                "application/json",
                r#"{"code":" 0 "}"#,
                "http status: 500",
            ),
            (
                "502 Bad Gateway",
                "application/json",
                "{}",
                "http status: 502",
            ),
            (
                "502 Bad Gateway",
                "text/html",
                "<html>502</html>",
                "http status: 502",
            ),
        ] {
            let (addr, url) = spawn_one_shot_http(status_line, content_type, body);
            let fetcher = HttpFetcher::with_policy(
                TargetPolicy::production().with_test_loopback_socket(addr),
            );
            let req = FetchRequest {
                key: "search:test".into(),
                url,
                method: FetchMethod::Get,
                body: None,
                cookie: None,
                api_key: Some("bad-key".into()),
                proxy: None,
                cdp_url: None,
            };
            let err = fetcher.fetch(&req).unwrap_err();
            assert_eq!(err.to_string(), expected_err, "body={body}");
        }
    }

    #[test]
    fn post_json_and_post_form_bind_matching_content_type_headers() {
        for (method, expected_ct, payload) in [
            (FetchMethod::PostJson, "application/json", r#"{"id":"99"}"#),
            (
                FetchMethod::PostForm,
                "application/x-www-form-urlencoded",
                "id=99",
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let handle = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 2048];
                let n = stream.read(&mut buf).unwrap();
                let raw = String::from_utf8_lossy(&buf[..n]).into_owned();
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                );
                raw
            });
            let fetcher = HttpFetcher::with_policy(
                TargetPolicy::production().with_test_loopback_socket(addr),
            );
            let req = FetchRequest {
                key: "download:test:99".into(),
                url: format!("http://{addr}/dl"),
                method,
                body: Some(payload.into()),
                cookie: None,
                api_key: Some("k".into()),
                proxy: None,
                cdp_url: None,
            };
            assert_eq!(fetcher.fetch(&req).unwrap(), "{}");
            let raw = handle.join().unwrap().to_ascii_lowercase();
            assert!(
                raw.contains(&format!("content-type: {expected_ct}")),
                "raw={raw}"
            );
        }
    }

    #[test]
    fn production_fetcher_blocks_loopback_metadata_and_external_proxy() {
        let server = TestHttpServer::spawn(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
        ]);
        let fetcher = HttpFetcher::new();

        let err_loopback = fetcher
            .fetch(&base_req(format!("http://{}/", server.addr)))
            .unwrap_err();
        assert!(
            err_loopback.to_string().contains("blocked"),
            "got {err_loopback}"
        );

        let err_meta = fetcher
            .fetch(&base_req("http://169.254.169.254/latest".into()))
            .unwrap_err();
        assert!(err_meta.to_string().contains("blocked"), "got {err_meta}");

        let mut proxied = base_req("https://example.com/".into());
        proxied.proxy = Some("http://127.0.0.1:8888".into());
        let err_proxy = fetcher.fetch(&proxied).unwrap_err();
        assert!(
            err_proxy.to_string().contains("unsupported"),
            "got {err_proxy}"
        );
    }

    #[test]
    fn fetcher_binds_credentials_to_initial_origin_and_blocks_self_and_non_2xx() {
        let mut target_b = TestHttpServer::spawn(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ndone".to_vec(),
        ]);
        let redirect_resp = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://{}/b\r\nContent-Length: 0\r\n\r\n",
            target_b.addr
        );
        let mut target_a = TestHttpServer::spawn(vec![
            redirect_resp.into_bytes(),
            b"HTTP/1.1 403 Forbidden\r\nContent-Length: 3\r\n\r\nbad".to_vec(),
        ]);
        let self_addr: SocketAddr = "127.0.0.1:18765".parse().unwrap();

        let policy = TargetPolicy::production()
            .with_blocked_self(self_addr)
            .with_test_loopback_socket(target_a.addr)
            .with_test_loopback_socket(target_b.addr);
        let fetcher = HttpFetcher::with_policy(policy);

        let mut req = base_req(format!("http://{}/a", target_a.addr));
        req.method = FetchMethod::PostJson;
        req.body = Some("{\"q\":1}".into());
        req.cookie = Some("uid=secret-cookie".into());
        req.api_key = Some("secret-key".into());

        let body = fetcher.fetch(&req).unwrap();
        assert_eq!(body, "done");

        let err_self = fetcher
            .fetch(&base_req(format!("http://{self_addr}/api/v1/sites")))
            .unwrap_err();
        assert!(err_self.to_string().contains("blocked"), "got {err_self}");

        let err_403 = fetcher
            .fetch(&base_req(format!("http://{}/fail", target_a.addr)))
            .unwrap_err();
        assert!(err_403.to_string().contains("403"), "got {err_403}");

        let reqs_a = target_a.take_requests();
        assert!(reqs_a[0].contains("uid=secret-cookie"), "{}", reqs_a[0]);
        assert!(reqs_a[0].contains("secret-key"), "{}", reqs_a[0]);
        let reqs_b = target_b.take_requests();
        assert!(!reqs_b[0].contains("secret-cookie"), "{}", reqs_b[0]);
        assert!(!reqs_b[0].contains("secret-key"), "{}", reqs_b[0]);
    }

    #[test]
    fn fetcher_post_302_to_blocked_self_returns_unknown_outcome_while_get_returns_fetch() {
        let self_addr: SocketAddr = "127.0.0.1:18765".parse().unwrap();
        let redirect_resp = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://{self_addr}/admin\r\nContent-Length: 0\r\n\r\n"
        );
        let server = TestHttpServer::spawn(vec![
            redirect_resp.as_bytes().to_vec(),
            redirect_resp.into_bytes(),
        ]);
        let policy = TargetPolicy::production()
            .with_blocked_self(self_addr)
            .with_test_loopback_socket(server.addr);
        let fetcher = HttpFetcher::with_policy(policy);

        let mut post_req = base_req(format!("http://{}/submit", server.addr));
        post_req.method = FetchMethod::PostForm;
        post_req.body = Some("ok=1".into());
        let post_err = fetcher.fetch(&post_req).unwrap_err();
        assert!(
            matches!(post_err, IndexerError::UnknownOutcome(_)),
            "POST -> 302 -> blocked_self must return UnknownOutcome, got {post_err:?}"
        );

        let get_req = base_req(format!("http://{}/check", server.addr));
        let get_err = fetcher.fetch(&get_req).unwrap_err();
        assert!(
            matches!(get_err, IndexerError::Fetch(_)),
            "GET -> 302 -> blocked_self must return Fetch, got {get_err:?}"
        );
    }

    fn insert_test_site(store: &store::Store, url: &str, enabled: bool) {
        let site = domain::Site {
            id: domain::SiteId::new(),
            name: "LocalPT".into(),
            url: url.into(),
            profile_id: "pterclub".into(),
            cookie: Some("uid=42".into()),
            api_key: None,
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: None,
            cdp_url: None,
            downloader_id: None,
            enabled,
            maintenance_via: domain::MaintenanceVia::Http,
            browser_search: false,
            timezone: "UTC".into(),
        };
        store.insert_site(&site).unwrap();
    }

    #[test]
    fn store_fetcher_allows_registered_enabled_site_and_blocks_ssrf_and_redirects() {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let store = Arc::new(store::Store::open(dir.path().join("data")).unwrap());

        let mut unregistered = TestHttpServer::spawn(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nleak".to_vec(),
        ]);
        let mut registered = TestHttpServer::spawn(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\nallowed".to_vec(),
            format!(
                "HTTP/1.1 302 Found\r\nLocation: http://{}/unregistered\r\nContent-Length: 0\r\n\r\n",
                unregistered.addr
            )
            .into_bytes(),
        ]);
        let mut disabled_srv = TestHttpServer::spawn(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nleak".to_vec(),
        ]);
        let self_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let self_addr = self_listener.local_addr().unwrap();

        insert_test_site(&store, &format!("http://{}/", registered.addr), true);
        insert_test_site(&store, &format!("http://{}/", disabled_srv.addr), false);
        insert_test_site(&store, "http://169.254.169.254/", true);
        insert_test_site(&store, "http://0.0.0.0:18080/", true);
        insert_test_site(&store, &format!("http://{self_addr}/"), true);

        let fetcher = HttpFetcher::with_store_and_self_listen(store, self_addr);

        // 1. Registered enabled Site on 127.0.0.1 works with credentials.
        let mut ok_req = base_req(format!(
            "http://{}/torrents.php?search=demo",
            registered.addr
        ));
        ok_req.cookie = Some("uid=42; pass=valid".into());
        assert_eq!(fetcher.fetch(&ok_req).unwrap(), "allowed");

        // 2. Redirect from registered loopback site to unregistered loopback port is blocked.
        let err_redir = fetcher
            .fetch(&base_req(format!("http://{}/redir", registered.addr)))
            .unwrap_err();
        assert!(
            err_redir.to_string().contains("blocked"),
            "redirect to unregistered loopback must be blocked, got {err_redir}"
        );

        // 3. Unregistered 127.0.0.1, disabled Site, 169.254.169.254, 0.0.0.0, and self_listen are ALL blocked.
        for bad_url in [
            format!("http://{}/spy", unregistered.addr),
            format!("http://{}/torrents.php", disabled_srv.addr),
            "http://169.254.169.254/latest/meta-data".to_string(),
            "http://0.0.0.0:18080/secret".to_string(),
            format!("http://{self_addr}/api/v1/sites"),
        ] {
            let err = fetcher.fetch(&base_req(bad_url.clone())).unwrap_err();
            assert!(
                err.to_string().contains("blocked"),
                "expected blocked error for {bad_url}, got {err}"
            );
        }

        assert_eq!(registered.take_requests().len(), 2);
        assert!(unregistered.take_requests().is_empty());
        assert!(disabled_srv.take_requests().is_empty());
    }
}
