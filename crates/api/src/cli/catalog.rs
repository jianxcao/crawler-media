use super::Transport;
use super::lists::format_rows;

pub async fn search_catalog(transport: &impl Transport, query: &str) -> Result<String, String> {
    let encoded = encode_query(query);
    let (status, body) = transport
        .send(
            "GET",
            &format!("/api/v1/search/titles?keyword={encoded}"),
            None,
        )
        .await?;
    if status != 200 {
        return Err(format!("catalog failed: {status} {body}"));
    }
    let rows = body["titles"]
        .as_array()
        .ok_or("catalog response is missing titles")?;
    if rows.is_empty() {
        return Ok("No Media\n".into());
    }
    let mut out = String::new();
    for row in rows {
        let title = row["title"].as_str().unwrap_or("-");
        let kind = row["kind"].as_str().unwrap_or("-");
        let year = row["year"]
            .as_u64()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".into());
        let tmdb = row["tmdb_id"]
            .as_str()
            .or_else(|| {
                (row["provider"].as_str() == Some("tmdb"))
                    .then(|| row["external_id"].as_str())
                    .flatten()
            })
            .unwrap_or("-");
        let douban = row["douban_id"]
            .as_str()
            .or_else(|| {
                (row["provider"].as_str() == Some("douban"))
                    .then(|| row["external_id"].as_str())
                    .flatten()
            })
            .unwrap_or("-");
        let tvdb = row["tvdb_id"]
            .as_str()
            .or_else(|| {
                (row["provider"].as_str() == Some("tvdb"))
                    .then(|| row["external_id"].as_str())
                    .flatten()
            })
            .unwrap_or("-");
        let bangumi = row["bangumi_id"]
            .as_str()
            .or_else(|| {
                (row["provider"].as_str() == Some("bangumi"))
                    .then(|| row["external_id"].as_str())
                    .flatten()
            })
            .unwrap_or("-");
        let anilist = row["anilist_id"]
            .as_str()
            .or_else(|| {
                (row["provider"].as_str() == Some("anilist"))
                    .then(|| row["external_id"].as_str())
                    .flatten()
            })
            .unwrap_or("-");
        out.push_str(&format!(
            "{title}\t{kind}\tyear={year}\ttmdb={tmdb}\tdouban={douban}\ttvdb={tvdb}\tbangumi={bangumi}\tanilist={anilist}\n"
        ));
    }
    Ok(out)
}

pub async fn list_catalog_cache(transport: &impl Transport) -> Result<String, String> {
    format_rows(
        transport,
        "/api/v1/catalog/cache",
        "catalog",
        "No cache\n",
        |row, out| {
            let source = row["source"].as_str().unwrap_or("-");
            let key = row["cache_key"].as_str().unwrap_or("-");
            let title = row["title"].as_str().unwrap_or("-");
            out.push_str(&format!("{source}\t{key}\t{title}\n"));
        },
    )
    .await
}

pub async fn delete_catalog_cache(
    transport: &impl Transport,
    source: &str,
    cache_key: &str,
) -> Result<String, String> {
    if source.trim().is_empty() || cache_key.trim().is_empty() {
        return Err("source and key are required".into());
    }
    let encoded_source = encode_query(source);
    let encoded_key = encode_query(cache_key);
    let (status, body) = transport
        .send(
            "DELETE",
            &format!("/api/v1/catalog/cache?source={encoded_source}&cache_key={encoded_key}"),
            None,
        )
        .await?;
    if status != 200 {
        return Err(format!("catalog delete failed: {status} {body}"));
    }
    Ok("deleted=true\n".into())
}

fn encode_query(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
