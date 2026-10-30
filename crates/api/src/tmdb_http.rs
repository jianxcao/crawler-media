use std::sync::Arc;

use media::{CatalogGet, TmdbError};

use crate::ApiState;
use crate::http_agent;

/// TMDB HTTP client whose API key is read live from settings KV
/// (`tmdb.api_key`), so configuring it in the UI takes effect without a
/// restart. main.rs seeds the KV from the `CRAWLER_MEDIA_TMDB_KEY` env var
/// at startup when the env is present.
pub struct TmdbHttp {
    readers: Arc<crate::store_readers::StoreReaders>,
}

impl TmdbHttp {
    pub fn new(state: &ApiState) -> Self {
        Self {
            readers: state.store_readers.clone(),
        }
    }

    fn api_key(&self) -> Option<String> {
        let store = self
            .readers
            .checkout(crate::http::store_wait::STORE_READ_WAIT)?;
        store
            .get_setting(crate::settings_keys::TMDB_API_KEY)
            .ok()
            .flatten()
            .filter(|key| !key.is_empty())
    }
}

impl CatalogGet for TmdbHttp {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
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
        let response = http_agent::call(|agent| agent.get(&url).call())
            .map_err(|err| TmdbError::Http(err.to_string()))?;
        response
            .into_body()
            .read_to_string()
            .map_err(|err| TmdbError::Http(err.to_string()))
    }
}
