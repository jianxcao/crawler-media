use media::{CatalogGet, TmdbError};

use crate::http_agent;

pub struct BangumiHttp;

impl CatalogGet for BangumiHttp {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
        let url = format!("https://api.bgm.tv{path}");
        let response = http_agent::call(|agent| agent.get(&url).call())
            .map_err(|err| TmdbError::Http(err.to_string()))?;
        response
            .into_body()
            .read_to_string()
            .map_err(|err| TmdbError::Http(err.to_string()))
    }
}
