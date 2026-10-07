use crate::http_agent;
use media::{CatalogGet, TmdbError};

pub struct TvdbHttp {
    api_key: String,
}

impl TvdbHttp {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }
}

impl CatalogGet for TvdbHttp {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
        let url = format!("https://api4.thetvdb.com/v4{path}");
        http_agent::call(|agent| {
            agent
                .get(&url)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .call()
        })
        .map_err(|err| TmdbError::Http(err.to_string()))?
        .into_body()
        .read_to_string()
        .map_err(|err| TmdbError::Http(err.to_string()))
    }
}
