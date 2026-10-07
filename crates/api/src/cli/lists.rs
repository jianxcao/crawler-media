use serde_json::Value;

use super::Transport;

pub async fn list_sites(transport: &impl Transport) -> Result<String, String> {
    let (status, body) = transport.send("GET", "/api/v1/sites", None).await?;
    if status != 200 {
        return Err(format!("sites failed: {status} {body}"));
    }
    let rows = body.as_array().ok_or("sites response is not a list")?;
    if rows.is_empty() {
        return Ok("No Sites\n".into());
    }
    let mut out = String::new();
    for row in rows {
        let id = row["id"].as_str().unwrap_or("-");
        let name = row["name"].as_str().unwrap_or("-");
        let url = row["url"].as_str().unwrap_or("-");
        let profile = row["profile_id"].as_str().unwrap_or("-");
        let enabled = row["enabled"].as_bool().unwrap_or(false);
        out.push_str(&format!(
            "{id}\t{name}\t{url}\t{profile}\tenabled={enabled}\n"
        ));
    }
    Ok(out)
}

pub async fn list_subscribes(transport: &impl Transport) -> Result<String, String> {
    format_rows(
        transport,
        "/api/v1/subscriptions",
        "subscribes",
        "No Subscribes\n",
        |row, out| {
            let id = row["id"].as_str().unwrap_or("-");
            let title = row["media"]["title"].as_str().unwrap_or("-");
            let kind = row["media"]["kind"].as_str().unwrap_or("-");
            let fetch = row["fetch_mode"].as_str().unwrap_or("-");
            out.push_str(&format!("{id}\t{title}\t{kind}\t{fetch}\n"));
        },
    )
    .await
}

pub async fn list_filters(transport: &impl Transport) -> Result<String, String> {
    let (status, body) = transport.send("GET", "/api/v1/rule-sets", None).await?;
    if status != 200 {
        return Err(format!("filters failed: {status} {body}"));
    }
    let rows = body.as_array().ok_or("filters response is not a list")?;
    if rows.is_empty() {
        return Ok("No Filters\n".into());
    }
    let default_id = rows.iter().find_map(|row| {
        row["is_default"]
            .as_bool()
            .unwrap_or(false)
            .then_some(row["id"].as_str())
            .flatten()
    });
    let mut out = String::new();
    for row in rows {
        let id = row["id"].as_str().unwrap_or("-");
        let name = row["name"].as_str().unwrap_or("-");
        let atoms = row["atoms"].as_array().map(|a| a.len()).unwrap_or(0);
        let marker = if default_id == Some(id) {
            "\tdefault"
        } else {
            ""
        };
        out.push_str(&format!("{id}\t{name}\tatoms={atoms}{marker}\n"));
    }
    Ok(out)
}

pub async fn list_downloaders(transport: &impl Transport) -> Result<String, String> {
    format_rows(
        transport,
        "/api/v1/downloaders",
        "downloaders",
        "No Downloaders\n",
        |row, out| {
            let id = row["id"].as_str().unwrap_or("-");
            let name = row["name"].as_str().unwrap_or("-");
            let kind = row["kind"].as_str().unwrap_or("-");
            let url = row["url"].as_str().unwrap_or("-");
            let marker = if row["is_default"].as_bool().unwrap_or(false) {
                "\tdefault"
            } else {
                ""
            };
            out.push_str(&format!("{id}\t{name}\t{kind}\t{url}{marker}\n"));
        },
    )
    .await
}

pub async fn list_users(transport: &impl Transport) -> Result<String, String> {
    format_rows(
        transport,
        "/api/v1/users",
        "users",
        "No Users\n",
        |row, out| {
            let id = row["id"].as_str().unwrap_or("-");
            let login = row["login"].as_str().unwrap_or("-");
            out.push_str(&format!("{id}\t{login}\n"));
        },
    )
    .await
}

pub async fn list_library(transport: &impl Transport) -> Result<String, String> {
    let (status, body) = transport.send("GET", "/api/v1/libraries", None).await?;
    if status != 200 {
        return Err(format!("library failed: {status} {body}"));
    }
    let libraries = body.as_array().ok_or("library response is not a list")?;
    let mut out = String::new();
    for library in libraries {
        let id = library["id"].as_str().unwrap_or_default();
        let kind = library["kind"].as_str().unwrap_or("-");
        if id.is_empty() {
            continue;
        }
        let (status, body) = transport
            .send("GET", &format!("/api/v1/libraries/{id}/items"), None)
            .await?;
        if status != 200 {
            continue;
        }
        for item in body.as_array().unwrap_or(&Vec::new()) {
            let title = item["title"].as_str().unwrap_or("-");
            let item_kind = item["kind"].as_str().unwrap_or(kind);
            out.push_str(&format!("{title}\t{item_kind}\n"));
        }
    }
    if out.is_empty() {
        return Ok("No Media\n".into());
    }
    Ok(out)
}

pub async fn list_downloads(transport: &impl Transport) -> Result<String, String> {
    let (status, body) = transport
        .send("GET", "/api/v1/downloaders/tasks", None)
        .await?;
    if status != 200 {
        return Err(format!("downloads failed: {status} {body}"));
    }
    let rows = body["items"]
        .as_array()
        .ok_or("downloads response is missing items")?;
    if rows.is_empty() {
        return Ok("No Downloads\n".into());
    }
    let mut out = String::new();
    for row in rows {
        let title = row["name"].as_str().unwrap_or("-");
        let media = row["media_title"].as_str().unwrap_or("-");
        out.push_str(&format!("{title}\t{media}\n"));
    }
    Ok(out)
}

pub async fn list_unidentified(transport: &impl Transport) -> Result<String, String> {
    format_rows(
        transport,
        "/api/v1/unidentified",
        "unidentified",
        "No Unidentified\n",
        |row, out| {
            let path = row["path"].as_str().unwrap_or("-");
            let confidence = row["confidence"].as_str().unwrap_or("-");
            out.push_str(&format!("{path}\t{confidence}\n"));
        },
    )
    .await
}

pub async fn list_jobs(transport: &impl Transport) -> Result<String, String> {
    let (status, body) = transport.send("GET", "/api/v1/jobs", None).await?;
    if status != 200 {
        return Err(format!("jobs failed: {status} {body}"));
    }
    let rows = body.as_array().ok_or("jobs response is not a list")?;
    if rows.is_empty() {
        return Ok("No Jobs\n".into());
    }
    let mut out = String::new();
    for row in rows {
        let kind = row["kind"].as_str().unwrap_or("-");
        let name = row["name"].as_str().unwrap_or("-");
        out.push_str(&format!("{kind}\t{name}\n"));
    }
    Ok(out)
}

pub async fn list_directory(transport: &impl Transport) -> Result<String, String> {
    let (status, body) = transport.send("GET", "/api/v1/directory", None).await?;
    if status != 200 {
        return Err(format!("directory failed: {status} {body}"));
    }
    let movie = body["movie_root"].as_str().unwrap_or("-");
    let tv = body["tv_root"].as_str().unwrap_or("-");
    let mode = body["transfer_mode"].as_str().unwrap_or("-");
    let movie_naming = body["movie_naming"].as_str().unwrap_or("-");
    let tv_naming = body["tv_naming"].as_str().unwrap_or("-");
    let intake = body["watch_intake"].as_str().unwrap_or("-");
    let inplace = body["watch_inplace"].as_str().unwrap_or("-");
    let scrape = body["scrape"].as_bool().unwrap_or(false);
    let mut out = format!(
        "movie_root={movie}\ntv_root={tv}\ntransfer_mode={mode}\nmovie_naming={movie_naming}\ntv_naming={tv_naming}\nscrape={scrape}\nwatch_intake={intake}\nwatch_inplace={inplace}\n"
    );
    if let Some(rows) = body["extra_roots"].as_array() {
        for row in rows {
            let kind = row["kind"].as_str().unwrap_or("-");
            let path = row["path"].as_str().unwrap_or("-");
            out.push_str(&format!("extra\t{kind}\t{path}\n"));
        }
    }
    Ok(out)
}

pub(super) async fn format_rows(
    transport: &impl Transport,
    path: &str,
    label: &str,
    empty: &str,
    write: impl Fn(&Value, &mut String),
) -> Result<String, String> {
    let (status, body) = transport.send("GET", path, None).await?;
    if status != 200 {
        return Err(format!("{label} failed: {status} {body}"));
    }
    let rows = body
        .as_array()
        .ok_or_else(|| format!("{label} response is not a list"))?;
    if rows.is_empty() {
        return Ok(empty.to_string());
    }
    let mut out = String::new();
    for row in rows {
        write(row, &mut out);
    }
    Ok(out)
}

pub async fn list_ledger(transport: &impl Transport) -> Result<String, String> {
    format_rows(
        transport,
        "/api/v1/ledger",
        "ledger",
        "No ledger\n",
        |row, out| {
            let title = row["media_title"].as_str().unwrap_or("-");
            let path = row["path"].as_str().unwrap_or("-");
            let resolution = row["resolution"].as_str().unwrap_or("-");
            out.push_str(&format!("{title}\t{path}\t{resolution}\n"));
        },
    )
    .await
}
