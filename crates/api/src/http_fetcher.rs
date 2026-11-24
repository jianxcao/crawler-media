use indexer::{FetchMethod, FetchRequest, Fetcher, IndexerError};
use std::time::Duration;

pub struct HttpFetcher;

impl Fetcher for HttpFetcher {
    fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
        let is_api_key = request
            .api_key
            .as_deref()
            .is_some_and(|key| !key.trim().is_empty());
        let mut config = ureq::Agent::config_builder()
            .http_status_as_error(!is_api_key)
            .timeout_resolve(Some(Duration::from_secs(5)))
            .timeout_connect(Some(Duration::from_secs(8)))
            .timeout_global(Some(Duration::from_secs(20)));
        if let Some(url) = &request.proxy {
            let proxy =
                ureq::Proxy::new(url).map_err(|error| IndexerError::Fetch(error.to_string()))?;
            config = config.proxy(Some(proxy));
        }
        let agent = config.build().new_agent();
        const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
        let content_type = match request.method {
            FetchMethod::Get => None,
            FetchMethod::PostJson => Some("application/json"),
            FetchMethod::PostForm => Some("application/x-www-form-urlencoded"),
        };
        let response = match content_type {
            None => {
                let mut builder = agent.get(&request.url).header("User-Agent", UA);
                if let Some(cookie) = &request.cookie {
                    builder = builder.header("Cookie", cookie);
                }
                if let Some(key) = &request.api_key {
                    builder = builder.header("x-api-key", key);
                }
                builder.call()
            }
            Some(content_type) => {
                let mut builder = agent.post(&request.url).header("User-Agent", UA);
                if let Some(cookie) = &request.cookie {
                    builder = builder.header("Cookie", cookie);
                }
                if let Some(key) = &request.api_key {
                    builder = builder.header("x-api-key", key);
                }
                builder
                    .header("Accept", "application/json")
                    .header("Content-Type", content_type)
                    .send(request.body.as_deref().unwrap_or(""))
            }
        };
        let response = response.map_err(|error| IndexerError::Fetch(error.to_string()))?;
        let status = response.status();
        let body = response
            .into_body()
            .read_to_string()
            .map_err(|error| IndexerError::Fetch(error.to_string()))?;
        if status.as_u16() >= 400 {
            let has_api_error_envelope =
                is_api_key && !request.key.starts_with("rss:") && has_dialect_error_envelope(&body);
            if !has_api_error_envelope {
                return Err(IndexerError::Fetch(format!(
                    "http status: {}",
                    status.as_u16()
                )));
            }
        }
        Ok(body)
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
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn spawn_one_shot_http(
        status_line: &'static str,
        content_type: &'static str,
        body: &'static str,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
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
        format!("http://{addr}/api/v1/torrents")
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
            let url = spawn_one_shot_http("401 Unauthorized", "application/json", json_body);
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
            assert_eq!(HttpFetcher.fetch(&req).unwrap(), json_body);
        }

        let rss_url = spawn_one_shot_http(
            "401 Unauthorized",
            "application/json",
            r#"{"success":false,"message":"Invalid API Key"}"#,
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
            HttpFetcher.fetch(&rss_req).unwrap_err().to_string(),
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
            let url = spawn_one_shot_http(status_line, content_type, body);
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
            let err = HttpFetcher.fetch(&req).unwrap_err();
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
            let handle = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 2048];
                let n = stream.read(&mut buf).unwrap();
                let raw = String::from_utf8_lossy(&buf[..n]).into_owned();
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                );
                raw
            });
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
            assert_eq!(HttpFetcher.fetch(&req).unwrap(), "{}");
            let raw = handle.join().unwrap().to_ascii_lowercase();
            assert!(
                raw.contains(&format!("content-type: {expected_ct}")),
                "raw={raw}"
            );
        }
    }
}
