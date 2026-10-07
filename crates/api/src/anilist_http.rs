use media::{CatalogGet, TmdbError};

use crate::http_agent;

pub struct AnilistHttp;

impl CatalogGet for AnilistHttp {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
        let (raw_query, format) = parse_path(path)?;
        let query = url_decode(raw_query);
        let body = serde_json::json!({
            "query": "query ($search: String, $format: MediaFormat) { Page(perPage: 20) { media(search: $search, type: ANIME, format: $format) { id title { romaji english native } format seasonYear coverImage { large } } } }",
            "variables": { "search": query, "format": format },
        });
        let response = http_agent::call(|agent| {
            agent
                .post("https://graphql.anilist.co")
                .header("Content-Type", "application/json")
                .header("Accept", "application/json")
                .send(body.to_string())
        })
        .map_err(|err| TmdbError::Http(err.to_string()))?;
        response
            .into_body()
            .read_to_string()
            .map_err(|err| TmdbError::Http(err.to_string()))
    }
}

fn parse_path(path: &str) -> Result<(&str, &str), TmdbError> {
    let query = path
        .split("query=")
        .nth(1)
        .and_then(|rest| rest.split('&').next())
        .ok_or_else(|| TmdbError::Parse("missing query".into()))?;
    let format = if path.contains("format=MOVIE") {
        "MOVIE"
    } else {
        "TV"
    };
    Ok((query, format))
}

fn url_decode(s: &str) -> String {
    let mut bytes = Vec::new();
    let mut chars = s.as_bytes().iter();
    while let Some(&b) = chars.next() {
        if b == b'+' {
            bytes.push(b' ');
        } else if b == b'%' {
            let h1 = chars.next().copied();
            let h2 = chars.next().copied();
            if let (Some(h1), Some(h2)) = (h1, h2) {
                let hex_str = [h1, h2];
                if let Ok(s) = std::str::from_utf8(&hex_str) {
                    if let Ok(val) = u8::from_str_radix(s, 16) {
                        bytes.push(val);
                    }
                }
            }
        } else {
            bytes.push(b);
        }
    }
    String::from_utf8(bytes).unwrap_or_else(|_| s.to_string())
}
