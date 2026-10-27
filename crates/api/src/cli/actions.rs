use serde_json::json;

use super::Transport;

pub async fn create_subscribe(
    transport: &impl Transport,
    title: &str,
    kind: &str,
) -> Result<String, String> {
    if title.trim().is_empty() {
        return Err("title is required".into());
    }
    if kind != "movie" && kind != "tv" {
        return Err("kind must be movie or tv".into());
    }
    let coverage = if kind == "tv" {
        json!({ "kind": "tv", "season": 1, "episode_from": 1 })
    } else {
        json!({ "kind": "movie" })
    };
    let body = json!({
        "media": { "kind": kind, "title": title },
        "coverage": coverage,
        "fetch_mode": "search",
    });
    let (status, body) = transport
        .send("POST", "/api/v1/subscriptions", Some(body))
        .await?;
    if !(200..300).contains(&status) {
        return Err(format!("subscribe failed: {status} {body}"));
    }
    let id = body["id"].as_str().ok_or("Subscribe id missing")?;
    Ok(format!("id={id}\n"))
}

pub async fn create_site(
    transport: &impl Transport,
    name: &str,
    url: &str,
    profile_id: &str,
    cookie: Option<&str>,
    api_key: Option<&str>,
) -> Result<String, String> {
    if name.trim().is_empty() || url.trim().is_empty() || profile_id.trim().is_empty() {
        return Err("name, url, and profile are required".into());
    }
    let mut body = json!({
        "name": name,
        "url": url,
        "profile_id": profile_id,
        "enabled": true,
    });
    if let Some(cookie) = cookie.filter(|value| !value.is_empty()) {
        body["cookie"] = json!(cookie);
    }
    if let Some(api_key) = api_key.filter(|value| !value.is_empty()) {
        body["api_key"] = json!(api_key);
    }
    let (status, body) = transport.send("POST", "/api/v1/sites", Some(body)).await?;
    if !(200..300).contains(&status) {
        return Err(format!("sites add failed: {status} {body}"));
    }
    let name = body["name"].as_str().unwrap_or(name);
    let url = body["url"].as_str().unwrap_or(url);
    let profile = body["profile_id"].as_str().unwrap_or(profile_id);
    Ok(format!("{name}\t{url}\t{profile}\n"))
}

pub async fn disable_site(transport: &impl Transport, id: &str) -> Result<String, String> {
    set_site_enabled(transport, id, false).await
}

pub async fn enable_site(transport: &impl Transport, id: &str) -> Result<String, String> {
    set_site_enabled(transport, id, true).await
}

async fn set_site_enabled(
    transport: &impl Transport,
    id: &str,
    enabled: bool,
) -> Result<String, String> {
    if id.trim().is_empty() {
        return Err("id is required".into());
    }
    let body = json!({ "enabled": enabled });
    let (status, body) = transport
        .send("PATCH", &format!("/api/v1/sites/{id}"), Some(body))
        .await?;
    if status != 200 {
        return Err(format!("sites enabled={enabled} failed: {status} {body}"));
    }
    Ok(format!("enabled={enabled}\n"))
}

pub async fn tick_jobs(transport: &impl Transport, now: Option<i64>) -> Result<String, String> {
    let path = match now {
        Some(now) => format!("/api/v1/jobs/tick?now={now}"),
        None => "/api/v1/jobs/tick".into(),
    };
    let (status, body) = transport.send("POST", &path, None).await?;
    if status != 200 {
        return Err(format!("jobs tick failed: {status} {body}"));
    }
    let ran = body["ran"].as_u64().unwrap_or(0);
    let kinds = body["kinds"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.as_str())
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    Ok(format!("ran={ran} kinds={kinds}\n"))
}

pub async fn search_torrents(transport: &impl Transport, query: &str) -> Result<String, String> {
    let encoded = encode_query(query);
    let (status, body) = transport
        .send(
            "GET",
            &format!("/api/v1/search/torrents?keyword={encoded}"),
            None,
        )
        .await?;
    if status != 200 {
        return Err(format!("search failed: {status} {body}"));
    }
    let rows = body["items"]
        .as_array()
        .ok_or("search response is missing items")?;
    let mut out = String::new();
    for row in rows {
        let title = row["title"].as_str().unwrap_or("-");
        let seeders = row["seeders"]
            .as_u64()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".into());
        let resolution = row["release"]["resolution"].as_str().unwrap_or("-");
        out.push_str(&format!("{title}\tseeders={seeders}\t{resolution}\n"));
    }
    let empty = Vec::new();
    for failure in body["sites"].as_array().unwrap_or(&empty) {
        let site = failure["site_id"].as_str().unwrap_or("-");
        if let Some(error) = failure["error"].as_str() {
            out.push_str(&format!("site {site} failed: {error}\n"));
        }
    }
    if rows.is_empty() && out.is_empty() {
        return Ok("No Torrents\n".into());
    }
    Ok(out)
}

pub async fn admit_torrent(
    transport: &impl Transport,
    subscribe_id: &str,
    enclosure: &str,
) -> Result<String, String> {
    let body = json!({
        "subscribe_id": subscribe_id,
        "enclosure": enclosure,
    });
    let (status, body) = transport
        .send("POST", "/api/v1/search/admit", Some(body))
        .await?;
    if status != 200 {
        return Err(format!("admit failed: {status} {body}"));
    }
    let title = body["title"].as_str().unwrap_or("-");
    Ok(format!("admitted={title}\n"))
}

pub async fn claim_unidentified(
    transport: &impl Transport,
    path: &str,
    title: &str,
    kind: &str,
    year: Option<u16>,
) -> Result<String, String> {
    let mut action = json!({ "type": "identify_and_transfer", "title": title, "kind": kind });
    if let Some(year) = year {
        action["year"] = json!(year);
    }
    let body = json!({ "path": path, "action": action });
    let (status, body) = transport
        .send("POST", "/api/v1/unidentified/claim", Some(body))
        .await?;
    if status != 200 {
        return Err(format!("claim failed: {status} {body}"));
    }
    let title = body["title"].as_str().unwrap_or("-");
    Ok(format!("claimed={title}\n"))
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
