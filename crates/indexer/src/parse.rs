mod api;

use domain::{Site, Torrent};
use scraper::{Html, Selector};

use crate::IndexerError;
use crate::profile::{Field, Framework, Profile, RssConfig};

pub(crate) use api::{
    parse_hdarea_download, parse_hdarea_torrents, parse_mteam_download, parse_mteam_items,
};

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
    if field.selector.trim().is_empty() {
        return None;
    }
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

pub(super) fn join_url(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    let base = base.trim_end_matches('/');
    let href = href.trim_start_matches('/');
    format!("{base}/{href}")
}

pub(super) fn parse_size(raw: &str) -> Option<u64> {
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
