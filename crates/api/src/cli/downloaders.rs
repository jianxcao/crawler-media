use serde_json::json;

use super::Transport;
use super::parse::Command;

pub fn parse_downloaders(args: &[&str]) -> Result<Command, String> {
    if args.is_empty() {
        return Ok(Command::Downloaders);
    }
    let mut add = false;
    let mut name = None;
    let mut kind = None;
    let mut url = None;
    let mut username = None;
    let mut password = None;
    let mut is_default = false;
    let mut id = None;
    let mut rest = args;
    while let Some((flag, tail)) = rest.split_first() {
        match *flag {
            "--add" => {
                add = true;
                rest = tail;
            }
            "--name" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--name needs a value".to_string())?;
                name = Some((*value).to_string());
                rest = next;
            }
            "--kind" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--kind needs a value".to_string())?;
                kind = Some((*value).to_string());
                rest = next;
            }
            "--url" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--url needs a value".to_string())?;
                url = Some((*value).to_string());
                rest = next;
            }
            "--username" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--username needs a value".to_string())?;
                username = Some((*value).to_string());
                rest = next;
            }
            "--password" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--password needs a value".to_string())?;
                password = Some((*value).to_string());
                rest = next;
            }
            "--default" => {
                is_default = true;
                rest = tail;
            }
            "--id" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--id needs a value".to_string())?;
                id = Some((*value).to_string());
                rest = next;
            }
            other => return Err(format!("unknown downloaders flag {other}")),
        }
    }
    if add && id.is_some() {
        return Err("use --add or --default --id, not both".into());
    }
    if is_default && !add {
        return Ok(Command::DownloadersDefault {
            id: id.ok_or_else(|| "default needs --id".to_string())?,
        });
    }
    if !add {
        return Err(
            "usage: downloaders | downloaders --add --name --kind qbittorrent|transmission --url [--username] [--password] [--default] | downloaders --default --id <id>"
                .into(),
        );
    }
    let kind = kind.ok_or_else(|| "add needs --kind".to_string())?;
    if kind != "qbittorrent" && kind != "transmission" {
        return Err("kind must be qbittorrent or transmission".into());
    }
    Ok(Command::DownloadersAdd {
        name: name.ok_or_else(|| "add needs --name".to_string())?,
        kind,
        url: url.ok_or_else(|| "add needs --url".to_string())?,
        username,
        password,
        is_default,
    })
}

pub async fn create_downloader(
    transport: &impl Transport,
    name: &str,
    kind: &str,
    url: &str,
    username: Option<&str>,
    password: Option<&str>,
    is_default: bool,
) -> Result<String, String> {
    if name.trim().is_empty() || url.trim().is_empty() {
        return Err("name and url are required".into());
    }
    let mut body = json!({
        "name": name,
        "kind": kind,
        "url": url,
        "is_default": is_default,
    });
    if let Some(username) = username.filter(|value| !value.is_empty()) {
        body["username"] = json!(username);
    }
    if let Some(password) = password.filter(|value| !value.is_empty()) {
        body["password"] = json!(password);
    }
    let (status, body) = transport
        .send("POST", "/api/v1/downloaders", Some(body))
        .await?;
    if !(200..300).contains(&status) {
        return Err(format!("downloaders add failed: {status} {body}"));
    }
    let name = body["name"].as_str().unwrap_or(name);
    let kind = body["kind"].as_str().unwrap_or(kind);
    let url = body["url"].as_str().unwrap_or(url);
    let marker = if body["is_default"].as_bool().unwrap_or(is_default) {
        "\tdefault"
    } else {
        ""
    };
    Ok(format!("{name}\t{kind}\t{url}{marker}\n"))
}

pub async fn set_default_downloader(
    transport: &impl Transport,
    id: &str,
) -> Result<String, String> {
    if id.trim().is_empty() {
        return Err("id is required".into());
    }
    let body = json!({ "is_default": true });
    let (status, body) = transport
        .send("PATCH", &format!("/api/v1/downloaders/{id}"), Some(body))
        .await?;
    if status != 200 {
        return Err(format!("downloaders default failed: {status} {body}"));
    }
    Ok("default=true\n".into())
}
