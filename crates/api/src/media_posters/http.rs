use std::io::Read;
use std::time::Duration;
use super::PosterError;

pub trait ImageFetcher: Send + Sync {
    fn fetch(&self, url: &str) -> Result<(String, Vec<u8>), PosterError>;
}

pub struct RestricedImageFetcher;

impl ImageFetcher for RestricedImageFetcher {
    fn fetch(&self, url: &str) -> Result<(String, Vec<u8>), PosterError> {
        let parsed = url::Url::parse(url).map_err(|_| PosterError::InvalidImage)?;
        let host = parsed.host_str().ok_or(PosterError::InvalidImage)?;
        if host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1" {
            return Err(PosterError::InvalidImage);
        }

        let agent = ureq::Agent::config_builder()
            .max_redirects(2)
            .timeout_connect(Some(Duration::from_secs(3)))
            .timeout_global(Some(Duration::from_secs(4)))
            .build()
            .new_agent();

        let response = agent.get(url).call().map_err(|e| PosterError::Upstream(e.to_string()))?;
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("image/jpeg")
            .to_string();

        let mut bytes = Vec::new();
        response
            .into_body()
            .as_reader()
            .take(super::cache::MAX_IMAGE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| PosterError::Io(e.to_string()))?;

        if bytes.len() > super::cache::MAX_IMAGE_BYTES || bytes.is_empty() {
            return Err(PosterError::InvalidImage);
        }

        Ok((content_type, bytes))
    }
}
