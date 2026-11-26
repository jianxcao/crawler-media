use domain::{Site, Torrent};
use serde::Deserialize;

use crate::IndexerError;
use crate::profile::Profile;

use super::{join_url, parse_size};

pub(crate) fn parse_mteam_items(body: &str) -> Result<Vec<MTeamItem>, IndexerError> {
    let parsed: MTeamResponse =
        serde_json::from_str(body).map_err(|err| IndexerError::Parse(err.to_string()))?;
    // M-Team signals rate limits and bad keys in the envelope rather than (or
    // alongside) the HTTP status; surface its own message instead of an empty list.
    check_mteam_envelope(
        parsed.success,
        parsed.code.as_deref(),
        parsed.message.as_deref(),
    )?;
    match parsed.data {
        None | Some(serde_json::Value::Null) => Ok(Vec::new()),
        Some(payload) => serde_json::from_value::<MTeamData>(payload)
            .map(MTeamData::into_items)
            .map_err(|err| IndexerError::Parse(err.to_string())),
    }
}

pub(crate) fn parse_mteam_download(body: &str) -> Result<String, IndexerError> {
    let parsed: MTeamDownloadResponse =
        serde_json::from_str(body).map_err(|err| IndexerError::Parse(err.to_string()))?;
    check_mteam_envelope(
        parsed.success,
        parsed.code.as_deref(),
        parsed.message.as_deref(),
    )?;
    match parsed.data.as_deref().map(str::trim) {
        Some(url) if !url.is_empty() => Ok(url.to_string()),
        _ => {
            let detail = format!(
                "M-Team 未返回下载链接（{}）",
                parsed.message.unwrap_or_else(|| "无描述".into())
            );
            if is_explicit_success_envelope(parsed.success, parsed.code.as_deref()) {
                Err(IndexerError::SourceNotFound(detail))
            } else {
                Err(IndexerError::Parse(detail))
            }
        }
    }
}

fn is_explicit_success_envelope(success: Option<bool>, code: Option<&str>) -> bool {
    let code = code.map(str::trim).filter(|c| !c.is_empty());
    code == Some("0") || success == Some(true)
}

fn is_not_found_envelope(code: Option<&str>, http_code: Option<u16>) -> bool {
    matches!(http_code, Some(404 | 410)) || matches!(code, Some("404" | "410"))
}

fn is_transient_or_auth_envelope(code: Option<&str>, http_code: Option<u16>) -> bool {
    http_code.is_some_and(|c| matches!(c, 401 | 403 | 429) || (500..600).contains(&c))
        || code.is_some_and(|c| {
            matches!(c, "401" | "403" | "429")
                || c.parse::<u16>().is_ok_and(|n| (500..600).contains(&n))
        })
}

fn check_mteam_envelope(
    success: Option<bool>,
    code: Option<&str>,
    message: Option<&str>,
) -> Result<(), IndexerError> {
    let code = code.map(str::trim).filter(|c| !c.is_empty());
    let code_failed = code.is_some_and(|c| c != "0");
    if !code_failed && success != Some(false) {
        return Ok(());
    }
    let msg = message.unwrap_or("无描述");
    let label = code.filter(|c| *c != "0").unwrap_or("error");
    let detail = format!("M-Team 返回错误 {label}：{msg}");
    if is_not_found_envelope(code, None) {
        return Err(IndexerError::SourceNotFound(detail));
    }
    if is_transient_or_auth_envelope(code, None) {
        return Err(IndexerError::Fetch(detail));
    }
    Err(IndexerError::Parse(detail))
}

impl MTeamItem {
    pub(crate) fn into_torrent(self, site_id: domain::SiteId, enclosure: String) -> Torrent {
        let status = self.status.unwrap_or_default();
        let free = status
            .discount
            .as_deref()
            .is_some_and(|d| d.eq_ignore_ascii_case("free"));
        let id = self.id.clone();
        let detail_url = id
            .as_deref()
            .map(|raw| format!("https://www.m-team.cc/torrent/{}", raw));
        Torrent {
            site_id,
            title: combine_title_and_subtitle(self.name, self.small_descr),
            enclosure,
            size_bytes: self.size.as_deref().and_then(parse_size),
            seeders: status.seeders.as_deref().and_then(|s| s.parse().ok()),
            free,
            hr: status.hr,
            imdb_id: None,
            id,
            leechers: status.leechers.as_deref().and_then(|s| s.parse().ok()),
            snatched: status.snatched.as_deref().and_then(|s| s.parse().ok()),
            upload_time: self.upload_time,
            detail_url,
            category: None,
            poster_url: self.image_list.into_iter().next(),
        }
    }
}

pub(crate) fn parse_hdarea_torrents(
    site: &Site,
    profile: &Profile,
    body: &str,
) -> Result<Vec<Torrent>, IndexerError> {
    let parsed: HdareaSearchResponse =
        serde_json::from_str(body).map_err(|err| IndexerError::Parse(err.to_string()))?;
    check_hdarea_envelope(
        parsed.success,
        parsed.code.as_deref(),
        parsed.http_code,
        parsed.message.as_deref(),
    )?;
    let base = profile.effective_site_base_url(&site.url);
    let items = match parsed.data {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(val) => serde_json::from_value::<HdareaData>(val)
            .map(HdareaData::into_items)
            .map_err(|err| IndexerError::Parse(err.to_string()))?,
    };
    Ok(items
        .into_iter()
        .map(|item| item.into_torrent(site.id, &base))
        .collect())
}

pub(crate) fn parse_hdarea_download(base: &str, body: &str) -> Result<String, IndexerError> {
    let parsed: HdareaDownloadResponse =
        serde_json::from_str(body).map_err(|err| IndexerError::Parse(err.to_string()))?;
    check_hdarea_envelope(
        parsed.success,
        parsed.code.as_deref(),
        parsed.http_code,
        parsed.message.as_deref(),
    )?;
    let extracted = match parsed.data {
        Some(serde_json::Value::String(url)) if !url.trim().is_empty() => {
            Some(join_url(base, url.trim()))
        }
        Some(serde_json::Value::Object(map)) => ["download_url", "url", "dl_url", "link"]
            .into_iter()
            .find_map(|key| map.get(key).and_then(serde_json::Value::as_str))
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(|url| join_url(base, url)),
        _ => None,
    };
    extracted.ok_or_else(|| {
        let detail = format!(
            "HDArea 未返回下载链接（{}）",
            parsed.message.unwrap_or_else(|| "无描述".into())
        );
        if is_explicit_success_envelope(parsed.success, parsed.code.as_deref()) {
            IndexerError::SourceNotFound(detail)
        } else {
            IndexerError::Parse(detail)
        }
    })
}

fn check_hdarea_envelope(
    success: Option<bool>,
    code: Option<&str>,
    http_code: Option<u16>,
    message: Option<&str>,
) -> Result<(), IndexerError> {
    let code = code.map(str::trim).filter(|c| !c.is_empty());
    let code_failed = code.is_some_and(|c| c != "0");
    let http_failed = http_code.is_some_and(|c| c >= 400);
    if !code_failed && !http_failed && success != Some(false) {
        return Ok(());
    }
    let msg = message.unwrap_or("无描述");
    if matches!(http_code, Some(401 | 403)) || matches!(code, Some("401" | "403")) {
        let status = http_code
            .filter(|c| matches!(c, 401 | 403))
            .or_else(|| code.and_then(|c| c.parse().ok()))
            .unwrap_or(401);
        return Err(IndexerError::Fetch(format!(
            "HDArea API 认证失败（HTTP {status}）：{msg}"
        )));
    }
    let label = code
        .filter(|c| *c != "0")
        .map(str::to_string)
        .or_else(|| http_code.filter(|c| *c >= 400).map(|c| c.to_string()))
        .unwrap_or_else(|| "error".into());
    let detail = format!("HDArea API 返回错误 {label}：{msg}");
    if is_not_found_envelope(code, http_code) {
        return Err(IndexerError::SourceNotFound(detail));
    }
    if is_transient_or_auth_envelope(code, http_code) {
        return Err(IndexerError::Fetch(detail));
    }
    Err(IndexerError::Parse(detail))
}

fn combine_title_and_subtitle(name: Option<String>, small_descr: Option<String>) -> String {
    let main = name.unwrap_or_default();
    let main = main.trim();
    let sub = small_descr.unwrap_or_default();
    let sub = sub.trim();
    match (main.is_empty(), sub.is_empty()) {
        (false, false) => format!("{main} [{sub}]"),
        (false, true) => main.to_string(),
        (true, false) => sub.to_string(),
        (true, true) => String::new(),
    }
}

/// M-Team scalars are strings in the documented contract, but real envelopes
/// flip fields to numbers or null (pagination on error, optional metadata).
/// Accept all three shapes so one odd field cannot sink a whole search.
fn de_opt_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(serde_json::Value::String(text)) => Some(text),
        Some(serde_json::Value::Number(number)) => Some(number.to_string()),
        Some(serde_json::Value::Bool(flag)) => Some(flag.to_string()),
        _ => None,
    })
}

fn de_vec_or_null<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Option::<Vec<T>>::deserialize(deserializer).map(Option::unwrap_or_default)
}

fn de_image_list<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(de_vec_or_null::<D, Option<String>>(deserializer)?
        .into_iter()
        .flatten()
        .filter(|s| !s.trim().is_empty())
        .collect())
}

#[derive(Deserialize)]
struct MTeamResponse {
    #[serde(default, deserialize_with = "de_opt_bool")]
    success: Option<bool>,
    #[serde(default, deserialize_with = "de_opt_string")]
    code: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    message: Option<String>,
    #[serde(default)]
    data: Option<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum MTeamData {
    Envelope {
        #[serde(default, deserialize_with = "de_vec_or_null")]
        data: Vec<MTeamItem>,
    },
    Items(Vec<MTeamItem>),
}

impl MTeamData {
    fn into_items(self) -> Vec<MTeamItem> {
        match self {
            Self::Envelope { data } | Self::Items(data) => data,
        }
    }
}

#[derive(Clone, Deserialize)]
pub(crate) struct MTeamItem {
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(crate) id: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    name: Option<String>,
    #[serde(
        default,
        rename = "smallDescr",
        alias = "small_descr",
        deserialize_with = "de_opt_string"
    )]
    small_descr: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    size: Option<String>,
    #[serde(default, alias = "createdDate", deserialize_with = "de_opt_string")]
    upload_time: Option<String>,
    #[serde(default, rename = "imageList", deserialize_with = "de_image_list")]
    image_list: Vec<String>,
    #[serde(default)]
    status: Option<MTeamStatus>,
}

#[derive(Clone, Default, Deserialize)]
struct MTeamStatus {
    #[serde(default, deserialize_with = "de_opt_string")]
    seeders: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    leechers: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    snatched: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    discount: Option<String>,
    #[serde(default)]
    hr: Option<bool>,
}

#[derive(Deserialize)]
struct MTeamDownloadResponse {
    #[serde(default, deserialize_with = "de_opt_bool")]
    success: Option<bool>,
    #[serde(default, deserialize_with = "de_opt_string")]
    code: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    message: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    data: Option<String>,
}

fn de_opt_u16<'de, D>(deserializer: D) -> Result<Option<u16>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(de_opt_string(deserializer)?.and_then(|s| s.trim().parse().ok()))
}

fn de_opt_u32<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(de_opt_string(deserializer)?.and_then(|s| s.trim().parse().ok()))
}

fn de_opt_f64<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(de_opt_string(deserializer)?.and_then(|s| s.trim().parse().ok()))
}

fn de_opt_bool<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(serde_json::Value::Bool(flag)) => Some(flag),
        Some(serde_json::Value::Number(num)) => num.as_i64().map(|n| n != 0),
        Some(serde_json::Value::String(text)) => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Some(true),
            "false" | "0" | "no" => Some(false),
            _ => None,
        },
        _ => None,
    })
}

#[derive(Deserialize)]
struct HdareaSearchResponse {
    #[serde(default, deserialize_with = "de_opt_bool")]
    success: Option<bool>,
    #[serde(default, deserialize_with = "de_opt_string")]
    code: Option<String>,
    #[serde(default, deserialize_with = "de_opt_u16")]
    http_code: Option<u16>,
    #[serde(default, deserialize_with = "de_opt_string")]
    message: Option<String>,
    #[serde(default)]
    data: Option<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum HdareaData {
    Envelope {
        #[serde(default, deserialize_with = "de_vec_or_null")]
        items: Vec<HdareaItem>,
    },
    Items(Vec<HdareaItem>),
}

impl HdareaData {
    fn into_items(self) -> Vec<HdareaItem> {
        match self {
            Self::Envelope { items } | Self::Items(items) => items,
        }
    }
}

#[derive(Deserialize)]
struct HdareaItem {
    #[serde(default, deserialize_with = "de_opt_string")]
    id: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    name: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    small_descr: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    category_id: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    category_name: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    size: Option<String>,
    #[serde(default, deserialize_with = "de_opt_u32")]
    seeders: Option<u32>,
    #[serde(default, deserialize_with = "de_opt_u32")]
    leechers: Option<u32>,
    #[serde(default, alias = "snatched", deserialize_with = "de_opt_u32")]
    times_completed: Option<u32>,
    #[serde(default, alias = "upload_time", deserialize_with = "de_opt_string")]
    added: Option<String>,
    #[serde(default, deserialize_with = "de_opt_u32")]
    sp_state: Option<u32>,
    #[serde(default, deserialize_with = "de_opt_f64")]
    download_volume_factor: Option<f64>,
    #[serde(default, deserialize_with = "de_opt_bool")]
    is_free: Option<bool>,
    #[serde(default, alias = "hit_and_run", deserialize_with = "de_opt_bool")]
    hr: Option<bool>,
    #[serde(default, deserialize_with = "de_opt_string")]
    imdb_id: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    poster_url: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    detail_url: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    download_url: Option<String>,
}

impl HdareaItem {
    fn into_torrent(self, site_id: domain::SiteId, base: &str) -> Torrent {
        let free = self.is_free == Some(true)
            || self.download_volume_factor.is_some_and(|v| v == 0.0)
            || matches!(self.sp_state, Some(2 | 4));
        let id = self.id.filter(|s| !s.trim().is_empty());
        let enclosure = self
            .download_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(|u| join_url(base, u))
            .unwrap_or_else(|| {
                let id_str = id.as_deref().unwrap_or_default();
                format!("{base}/download.php?id={id_str}")
            });
        let detail_url = self
            .detail_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(|u| join_url(base, u))
            .or_else(|| {
                id.as_deref()
                    .map(|raw| format!("{base}/details.php?id={raw}"))
            });
        Torrent {
            site_id,
            title: combine_title_and_subtitle(self.name, self.small_descr),
            enclosure,
            size_bytes: self.size.as_deref().and_then(parse_size),
            seeders: self.seeders,
            free,
            hr: self.hr,
            imdb_id: self.imdb_id.filter(|s| !s.trim().is_empty()),
            id,
            leechers: self.leechers,
            snatched: self.times_completed,
            upload_time: self.added.filter(|s| !s.trim().is_empty()),
            detail_url,
            category: self
                .category_name
                .filter(|s| !s.trim().is_empty())
                .or_else(|| self.category_id.filter(|s| !s.trim().is_empty())),
            poster_url: self.poster_url.filter(|s| !s.trim().is_empty()),
        }
    }
}

#[derive(Deserialize)]
struct HdareaDownloadResponse {
    #[serde(default, deserialize_with = "de_opt_bool")]
    success: Option<bool>,
    #[serde(default, deserialize_with = "de_opt_string")]
    code: Option<String>,
    #[serde(default, deserialize_with = "de_opt_u16")]
    http_code: Option<u16>,
    #[serde(default, deserialize_with = "de_opt_string")]
    message: Option<String>,
    #[serde(default)]
    data: Option<serde_json::Value>,
}
