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
        let response = match request.method {
            FetchMethod::Get => {
                let mut builder = agent.get(&request.url).header("User-Agent", UA);
                if let Some(cookie) = &request.cookie {
                    builder = builder.header("Cookie", cookie);
                }
                if let Some(key) = &request.api_key {
                    builder = builder.header("x-api-key", key);
                }
                builder.call()
            }
            FetchMethod::PostJson | FetchMethod::PostForm => {
                let mut builder = agent.post(&request.url).header("User-Agent", UA);
                if let Some(cookie) = &request.cookie {
                    builder = builder.header("Cookie", cookie);
                }
                if let Some(key) = &request.api_key {
                    builder = builder.header("x-api-key", key);
                }
                let content_type = match request.method {
                    FetchMethod::PostJson => "application/json",
                    FetchMethod::PostForm => "application/x-www-form-urlencoded",
                    FetchMethod::Get => unreachable!(),
                };
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
            let has_json_envelope = serde_json::from_str::<serde_json::Value>(&body)
                .is_ok_and(|value| value.is_object());
            if !is_api_key || !has_json_envelope {
                return Err(IndexerError::Fetch(format!(
                    "http status: {}",
                    status.as_u16()
                )));
            }
        }
        Ok(body)
    }
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
    fn api_key_request_preserves_401_json_error_envelope_and_rejects_html_errors() {
        let json_body = r#"{"success":false,"code":401,"message":"Invalid API Key"}"#;
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
        let out = HttpFetcher.fetch(&req).unwrap();
        assert_eq!(out, json_body);

        let html_url = spawn_one_shot_http("502 Bad Gateway", "text/html", "<html>502</html>");
        let html_req = FetchRequest {
            url: html_url,
            ..req
        };
        let err = HttpFetcher.fetch(&html_req).unwrap_err();
        assert_eq!(err.to_string(), "http status: 502");
    }
}
