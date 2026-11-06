use std::sync::Arc;

use media::{CatalogGet, TmdbError};

use crate::ApiState;
use crate::http_agent;

mod retry;
mod transport;

#[cfg(test)]
mod tests;

/// TMDB HTTP client whose API key is read live from settings KV
/// (`tmdb.api_key`), so configuring it in the UI takes effect without a
/// restart. main.rs seeds the KV from the `CRAWLER_MEDIA_TMDB_KEY` env var
/// at startup when the env is present.
pub struct TmdbHttp {
    store: Arc<crate::Store>,
    transport: Arc<dyn transport::Transport>,
    runtime: Arc<dyn retry::Runtime>,
}

impl TmdbHttp {
    pub fn new(state: &ApiState) -> Self {
        Self {
            store: state.store.clone(),
            transport: Arc::new(transport::UreqTransport),
            runtime: Arc::new(retry::SystemRuntime),
        }
    }

    fn api_key(&self) -> Option<String> {
        self.store
            .get_setting(crate::settings_keys::TMDB_API_KEY)
            .ok()
            .flatten()
            .filter(|key| !key.is_empty())
    }
}

impl CatalogGet for TmdbHttp {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
        let request =
            media::CatalogRequest::origin(self.runtime.now() + media::TMDB_REQUEST_TIMEOUT);
        request.scope(|| self.get_body(path))
    }
}

impl TmdbHttp {
    fn get_body(&self, path: &str) -> Result<String, TmdbError> {
        if let Some(request) = media::CatalogRequest::current() {
            request.check()?;
        }
        let Some(api_key) = self.api_key() else {
            return Err(TmdbError::Http(
                "TMDB API key 未配置，请到 设置 → 元数据源 填写".into(),
            ));
        };
        // media::Tmdb owns the language parameter (body_with_language) and the
        // cache key; here we only append the API key. Adding language here too
        // would double-apply it (and break /images which must stay unqualified).
        let join = if path.contains('?') { '&' } else { '?' };
        let url = format!("https://api.themoviedb.org/3{path}{join}api_key={api_key}");
        let proxy = http_agent::current_proxy();
        if let Some(proxy) = &proxy {
            tracing::info!(proxy = %http_agent::mask_proxy_url(proxy), "出站元数据请求已通过代理转发");
        }
        retry::get(self, path, &url, proxy.as_deref())
    }
}
