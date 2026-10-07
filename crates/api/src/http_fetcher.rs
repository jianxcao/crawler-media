use indexer::{FetchMethod, FetchRequest, Fetcher, IndexerError};
use std::time::Duration;

pub struct HttpFetcher;

impl Fetcher for HttpFetcher {
    fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
        let agent = match &request.proxy {
            Some(url) => {
                let proxy = ureq::Proxy::new(url)
                    .map_err(|error| IndexerError::Fetch(error.to_string()))?;
                ureq::Agent::config_builder()
                    .proxy(Some(proxy))
                    .timeout_resolve(Some(Duration::from_secs(5)))
                    .timeout_connect(Some(Duration::from_secs(8)))
                    .timeout_global(Some(Duration::from_secs(20)))
                    .build()
                    .new_agent()
            }
            None => ureq::Agent::config_builder()
                .timeout_resolve(Some(Duration::from_secs(5)))
                .timeout_connect(Some(Duration::from_secs(8)))
                .timeout_global(Some(Duration::from_secs(20)))
                .build()
                .new_agent(),
        };
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
        response
            .map_err(|error| IndexerError::Fetch(error.to_string()))?
            .into_body()
            .read_to_string()
            .map_err(|error| IndexerError::Fetch(error.to_string()))
    }
}
