use domain::{Site, Torrent};
use scraper::{Html, Selector};
use serde::Deserialize;

use crate::IndexerError;
use crate::profile::{Field, Framework, Profile, RssConfig};

pub(crate) fn search(
    site: &Site,
    profile: &Profile,
    body: &str,
) -> Result<Vec<Torrent>, IndexerError> {
    match profile.framework {
        Framework::Nexusphp => parse_html(site, profile, body),
        Framework::Api => Err(IndexerError::Parse(
            "API search requires download-token resolution".into(),
        )),
    }
}

pub(crate) fn rss(
    site: &Site,
    profile: &Profile,
    body: &str,
) -> Result<Vec<Torrent>, IndexerError> {
    let rss = profile
        .rss
        .as_ref()
        .ok_or_else(|| IndexerError::Parse(format!("{} has no rss mapping", profile.id)))?;
    parse_rss(site, rss, body)
}

pub(crate) fn parse_html(
    site: &Site,
    profile: &Profile,
    body: &str,
) -> Result<Vec<Torrent>, IndexerError> {
    let base = profile.effective_site_base_url(&site.url);
    let join_base = if base.is_empty() {
        site.url.as_str()
    } else {
        base.as_str()
    };
    let document = Html::parse_document(body);

    // 登录态探测：如果配置了 login_success_css（例如 a[href*='logout.php']），页面缺失该元素时
    // 说明 Cookie 失效并被重定向到了登录页，明确报出认证失效，而不是静默返回 0 条。
    if let Some(login_css) = &profile.login_success_css {
        if let Ok(login_sel) = Selector::parse(login_css)
            && document.select(&login_sel).next().is_none()
        {
            // 进一步检查页面是否包含登录特征
            let is_login_page = body.contains("login.php")
                || body.contains("登录")
                || body.contains("登入")
                || body.contains("password")
                || body.contains("two_step_code");
            if is_login_page {
                return Err(IndexerError::Parse(
                    "站点 Cookie 已过期或失效，请重新登录并更新 Cookie".into(),
                ));
            }
        }
    } else {
        // 通用兜底探测：如果标题包含登录且完全找不到退出链接
        if (body.contains("login.php") || body.contains("登录") || body.contains("登入"))
            && !body.contains("logout.php")
            && let Ok(login_form_sel) =
                Selector::parse("form[action*='login'], form[action*='takelogin']")
            && document.select(&login_form_sel).next().is_some()
        {
            return Err(IndexerError::Parse(
                "站点 Cookie 已过期或失效，请重新登录并更新 Cookie".into(),
            ));
        }
    }

    let item_sel = selector(&profile.list.item)?;
    let mut torrents = Vec::new();
    for item in document.select(&item_sel) {
        let Some(title) =
            optional_text(item, profile.fields.title.as_ref()).filter(|s| !s.is_empty())
        else {
            continue;
        };
        let Some(enclosure) =
            optional_text(item, profile.fields.enclosure.as_ref()).filter(|s| !s.is_empty())
        else {
            continue;
        };
        let detail_url = profile
            .fields
            .detail
            .as_ref()
            .and_then(|f| optional_text(item, Some(f)))
            .map(|d| join_url(join_base, &d));
        let enclosure_url = join_url(join_base, &enclosure);
        torrents.push(Torrent {
            site_id: site.id,
            title,
            enclosure: enclosure_url.clone(),
            size_bytes: optional_text(item, profile.fields.size.as_ref())
                .and_then(|s| parse_size(&s)),
            seeders: optional_text(item, profile.fields.seeders.as_ref())
                .and_then(|s| s.parse().ok()),
            free: present(item, profile.fields.free.as_ref()),
            hr: hr_present(item, profile.fields.hr.as_ref()),
            imdb_id: None,
            id: extract_id(&enclosure_url).or_else(|| detail_url.as_deref().and_then(extract_id)),
            leechers: optional_text(item, profile.fields.leechers.as_ref())
                .and_then(|s| s.parse().ok()),
            snatched: optional_text(item, profile.fields.snatched.as_ref())
                .and_then(|s| s.parse().ok()),
            upload_time: optional_text(item, profile.fields.upload_time.as_ref()),
            detail_url,
            category: optional_text(item, profile.fields.category.as_ref()),
            poster_url: None,
        });
    }
    Ok(torrents)
}

pub(crate) fn parse_mteam_items(body: &str) -> Result<Vec<MTeamItem>, IndexerError> {
    let parsed: MTeamResponse =
        serde_json::from_str(body).map_err(|err| IndexerError::Parse(err.to_string()))?;
    // M-Team signals rate limits and bad keys in the envelope rather than the
    // HTTP status; surface its own message instead of an empty result list.
    if let Some(code) = parsed.code.as_deref()
        && code != "0"
    {
        return Err(IndexerError::Parse(format!(
            "M-Team 返回错误 {code}：{}",
            parsed.message.unwrap_or_else(|| "无描述".into())
        )));
    }
    Ok(parsed.data.map(|payload| payload.data).unwrap_or_default())
}

pub(crate) fn parse_mteam_download(body: &str) -> Result<String, IndexerError> {
    let parsed: MTeamDownloadResponse =
        serde_json::from_str(body).map_err(|err| IndexerError::Parse(err.to_string()))?;
    match parsed.data {
        Some(url) if !url.is_empty() => Ok(url),
        _ => Err(IndexerError::Parse(format!(
            "M-Team 未返回下载链接（{}）",
            parsed.message.unwrap_or_else(|| "无描述".into())
        ))),
    }
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
    let items = parsed.data.map(HdareaData::into_items).unwrap_or_default();
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
        IndexerError::Parse(format!(
            "HDArea 未返回下载链接（{}）",
            parsed.message.unwrap_or_else(|| "无描述".into())
        ))
    })
}

fn check_hdarea_envelope(
    success: Option<bool>,
    code: Option<&str>,
    http_code: Option<u16>,
    message: Option<&str>,
) -> Result<(), IndexerError> {
    let code_failed = code.is_some_and(|c| c != "0");
    if !code_failed && success != Some(false) {
        return Ok(());
    }
    let msg = message.unwrap_or("无描述");
    if matches!(http_code, Some(401 | 403)) || matches!(code, Some("401" | "403")) {
        let status = http_code.unwrap_or(401);
        return Err(IndexerError::Fetch(format!(
            "HDArea API 认证失败（HTTP {status}）：{msg}"
        )));
    }
    let label = code.unwrap_or("error");
    Err(IndexerError::Parse(format!(
        "HDArea API 返回错误 {label}：{msg}"
    )))
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

fn parse_rss(site: &Site, rss: &RssConfig, body: &str) -> Result<Vec<Torrent>, IndexerError> {
    let document =
        roxmltree::Document::parse(body).map_err(|err| IndexerError::Parse(err.to_string()))?;
    let mut torrents = Vec::new();
    for item in document
        .descendants()
        .filter(|node| node.has_tag_name(rss.item.as_str()))
    {
        let title = xml_field(item, &rss.title)
            .ok_or_else(|| IndexerError::Parse("rss item missing title".into()))?;
        let enclosure = xml_field(item, &rss.enclosure)
            .ok_or_else(|| IndexerError::Parse("rss item missing enclosure".into()))?;
        let enclosure_url = join_url(&site.url, &enclosure);
        let id = rss
            .id
            .as_ref()
            .and_then(|f| xml_field(item, f))
            .and_then(|raw| extract_id(&raw).or(Some(raw)))
            .or_else(|| extract_id(&enclosure_url));
        let detail_url = id.as_deref().map(|raw| join_url(&site.url, raw));
        torrents.push(Torrent {
            site_id: site.id,
            title,
            enclosure: enclosure_url,
            size_bytes: rss
                .size
                .as_ref()
                .and_then(|f| xml_field(item, f))
                .and_then(|s| parse_size(&s)),
            seeders: rss
                .seeders
                .as_ref()
                .and_then(|f| xml_field(item, f))
                .and_then(|s| s.parse().ok()),
            free: rss
                .free
                .as_ref()
                .and_then(|f| xml_field(item, f))
                .is_some_and(|s| truthy(&s)),
            hr: rss
                .hr
                .as_ref()
                .and_then(|f| xml_field(item, f))
                .and_then(|s| hr_flag(&s)),
            imdb_id: None,
            id,
            leechers: rss
                .leechers
                .as_ref()
                .and_then(|f| xml_field(item, f))
                .and_then(|s| s.parse().ok()),
            snatched: rss
                .snatched
                .as_ref()
                .and_then(|f| xml_field(item, f))
                .and_then(|s| s.parse().ok()),
            upload_time: rss.upload_time.as_ref().and_then(|f| xml_field(item, f)),
            detail_url,
            category: rss.category.as_ref().and_then(|f| xml_field(item, f)),
            poster_url: None,
        });
    }
    Ok(torrents)
}

/// 从下载/详情链接提取站点内种子 id（`download.php?id=123` / `torrent/123`）。
pub(crate) fn extract_id(url: &str) -> Option<String> {
    let query = url.split_once('?').map(|(_, q)| q).unwrap_or(url);
    query
        .split('&')
        .find_map(|part| part.strip_prefix("id="))
        .map(str::to_string)
        .or_else(|| {
            url.rsplit_once("torrent/")
                .map(|(_, raw)| raw.split(['?', '/', '&']).next().unwrap_or("").to_string())
                .filter(|raw| !raw.is_empty())
        })
}

fn selector(raw: &str) -> Result<Selector, IndexerError> {
    Selector::parse(raw).map_err(|err| IndexerError::Parse(err.to_string()))
}

fn optional_text(root: scraper::ElementRef<'_>, field: Option<&Field>) -> Option<String> {
    let field = field?;
    let sel = Selector::parse(&field.selector).ok()?;
    let el = root.select(&sel).next()?;
    if let Some(attr) = &field.attr {
        el.value().attr(attr).map(|s| s.trim().to_string())
    } else {
        let text = el.text().collect::<String>();
        Some(text.trim().to_string())
    }
}

fn present(root: scraper::ElementRef<'_>, field: Option<&Field>) -> bool {
    let Some(field) = field else {
        return false;
    };
    let Ok(sel) = Selector::parse(&field.selector) else {
        return false;
    };
    root.select(&sel).next().is_some()
}

fn hr_present(root: scraper::ElementRef<'_>, field: Option<&Field>) -> Option<bool> {
    let field = field?;
    let selector = match Selector::parse(&field.selector) {
        Ok(selector) => selector,
        Err(error) => {
            tracing::warn!(selector = %field.selector, ?error, "H&R selector 无效，状态未知");
            return None;
        }
    };
    Some(root.select(&selector).next().is_some())
}

fn hr_flag(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "y" => Some(true),
        "0" | "false" | "no" | "n" => Some(false),
        _ => None,
    }
}

fn xml_field(item: roxmltree::Node<'_, '_>, field: &Field) -> Option<String> {
    let node = item
        .descendants()
        .find(|node| node.has_tag_name(field.selector.as_str()))?;
    if let Some(attr) = &field.attr {
        node.attribute(attr.as_str()).map(|s| s.trim().to_string())
    } else {
        node.text().map(|s| s.trim().to_string())
    }
}

fn join_url(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    let base = base.trim_end_matches('/');
    let href = href.trim_start_matches('/');
    format!("{base}/{href}")
}

fn parse_size(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if let Ok(n) = raw.parse::<u64>() {
        return Some(n);
    }
    let compact = raw.split_whitespace().collect::<String>();
    let split = compact
        .char_indices()
        .find(|(_, ch)| ch.is_ascii_alphabetic())
        .map(|(i, _)| i)?;
    let n: f64 = compact[..split].parse().ok()?;
    let unit = compact[split..].to_ascii_uppercase();
    let mul = match unit.as_str() {
        "B" => 1.0,
        "KB" | "KIB" => 1024.0,
        "MB" | "MIB" => 1024.0 * 1024.0,
        "GB" | "GIB" => 1024.0 * 1024.0 * 1024.0,
        "TB" | "TIB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((n * mul).round() as u64)
}

fn truthy(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "y" | "free"
    )
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

#[derive(Deserialize)]
struct MTeamResponse {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    data: Option<MTeamData>,
}

#[derive(Deserialize)]
struct MTeamData {
    #[serde(default)]
    data: Vec<MTeamItem>,
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
    #[serde(default, rename = "imageList")]
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
    #[serde(default)]
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
    data: Option<HdareaData>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum HdareaData {
    Envelope {
        #[serde(default)]
        items: Vec<HdareaItem>,
    },
    Items(Vec<HdareaItem>),
}

impl HdareaData {
    fn into_items(self) -> Vec<HdareaItem> {
        match self {
            Self::Envelope { items } => items,
            Self::Items(items) => items,
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
