use media::{CatalogGet, TmdbError};

use crate::http_agent;

pub struct DoubanHttp;

impl CatalogGet for DoubanHttp {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
        const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
        let (url, referer) = if path.starts_with("/rexxar/") {
            (
                format!("https://m.douban.com{path}"),
                "https://m.douban.com/",
            )
        } else {
            (
                format!("https://movie.douban.com{path}"),
                "https://movie.douban.com/",
            )
        };
        let response = http_agent::call_douban(|agent| {
            agent
                .get(&url)
                .header("User-Agent", UA)
                .header("Referer", referer)
                .call()
        })
        .map_err(|err| TmdbError::Http(err.to_string()))?;
        response
            .into_body()
            .read_to_string()
            .map_err(|err| TmdbError::Http(err.to_string()))
    }
}
