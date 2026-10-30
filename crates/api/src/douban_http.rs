use media::{CatalogGet, TmdbError};

use crate::http_agent;

pub struct DoubanHttp;

impl CatalogGet for DoubanHttp {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
        self.get_with_origin(path, None)
    }
}

impl DoubanHttp {
    fn get_with_origin(&self, path: &str, origin: Option<&str>) -> Result<String, TmdbError> {
        const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
        const MOBILE_UA: &str =
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15";
        let (url, referer, user_agent) = if path.starts_with("/rexxar/") {
            (
                format!("https://m.douban.com{path}"),
                "https://m.douban.com/movie/",
                MOBILE_UA,
            )
        } else {
            (
                format!("https://movie.douban.com{path}"),
                "https://movie.douban.com/",
                UA,
            )
        };
        let url = origin
            .map(|origin| format!("{origin}{path}"))
            .unwrap_or(url);
        let response = http_agent::call_douban(|agent| {
            agent
                .get(&url)
                .header("User-Agent", user_agent)
                .header("Referer", referer)
                .call()
        })
        .map_err(|error| match error {
            ureq::Error::StatusCode(404) => TmdbError::NotFound(path.to_owned()),
            error => TmdbError::Http(error.to_string()),
        })?;
        response
            .into_body()
            .read_to_string()
            .map_err(|err| TmdbError::Http(err.to_string()))
    }
}

#[cfg(test)]
mod tests;
