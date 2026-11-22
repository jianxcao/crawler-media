use domain::Site;

use crate::IndexerError;
use crate::profile::{ApiDialect, Framework, Profile};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchMethod {
    Get,
    PostJson,
    PostForm,
}
#[derive(Clone)]
pub struct FetchRequest {
    pub key: String,
    pub url: String,
    pub method: FetchMethod,
    pub body: Option<String>,
    pub cookie: Option<String>,
    pub api_key: Option<String>,
    pub proxy: Option<String>,
    pub cdp_url: Option<String>,
}

pub trait Fetcher: Send + Sync {
    fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError>;
}

pub fn effective_base_url(site: &Site, profile: &Profile) -> String {
    profile.effective_site_base_url(&site.url)
}

pub(crate) fn uses_html_cookie_fallback(site: &Site, profile: &Profile) -> bool {
    profile.framework == Framework::Api
        && site.api_key.as_deref().is_none_or(|k| k.trim().is_empty())
        && site.cookie.as_deref().is_some_and(|c| !c.trim().is_empty())
        && !profile.list.item.is_empty()
}

pub fn search_request_page_categories(
    site: &Site,
    profile: &Profile,
    keyword: &str,
    page: u32,
    categories: &[String],
) -> FetchRequest {
    let base = effective_base_url(site, profile);
    let category_ids = profile
        .categories
        .as_ref()
        .map(|mapping| mapping.ids(categories))
        .unwrap_or_default();
    let html_fallback = uses_html_cookie_fallback(site, profile);
    let raw_path = if html_fallback {
        "/torrents.php"
    } else {
        profile.search.path.as_str()
    };
    let path = if raw_path.starts_with('/') {
        raw_path.to_string()
    } else {
        format!("/{raw_path}")
    };
    let (method, url, body) = if profile.framework == Framework::Nexusphp || html_fallback {
        let url = nexusphp_search_url(
            &base,
            &path,
            profile,
            keyword,
            page,
            &category_ids,
            html_fallback,
        );
        (FetchMethod::Get, url, None)
    } else {
        match profile.api_dialect {
            ApiDialect::Mteam => (
                FetchMethod::PostJson,
                format!("{base}{path}"),
                Some(mteam_search_body(profile, keyword, page, category_ids)),
            ),
            ApiDialect::HdareaV1 => (
                FetchMethod::Get,
                hdarea_v1_search_url(&base, &path, profile, keyword, page, &category_ids),
                None,
            ),
        }
    };
    FetchRequest {
        key: format!("search:{}", site.id),
        url,
        method,
        body,
        cookie: site.cookie.clone(),
        api_key: site.api_key.clone(),
        proxy: site.proxy.clone(),
        cdp_url: site.cdp_url.clone(),
    }
}

fn nexusphp_search_url(
    base: &str,
    path: &str,
    profile: &Profile,
    keyword: &str,
    page: u32,
    category_ids: &[serde_json::Value],
    html_fallback: bool,
) -> String {
    let query_param = if html_fallback {
        "search"
    } else {
        profile.search.query_param.as_str()
    };
    let mut url = format!("{base}{path}?{}={}", query_param, form_encode(keyword));
    if page > 1 {
        let param = if html_fallback {
            "page"
        } else {
            profile.search.page_param.as_deref().unwrap_or("page")
        };
        let page_start = if html_fallback {
            0
        } else {
            profile.search.page_start.unwrap_or(0)
        };
        let page_number = page_start.saturating_add(page - 1);
        url.push_str(&format!("&{}={page_number}", form_encode(param)));
    }
    let param = if html_fallback {
        "cat"
    } else {
        profile.search.category_param.as_deref().unwrap_or("cat")
    };
    for id in category_ids {
        let value = id
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| id.to_string());
        url.push_str(&format!("&{}{}=1", form_encode(param), form_encode(&value)));
    }
    url
}

fn mteam_search_body(
    profile: &Profile,
    keyword: &str,
    page: u32,
    category_ids: Vec<serde_json::Value>,
) -> String {
    let mut search_json = serde_json::json!({
        "mode": "normal",
        "keyword": keyword,
        "pageSize": profile.search.page_size.unwrap_or(100),
    });
    let page_param = profile.search.page_param.as_deref().unwrap_or("pageNumber");
    let page_start = profile.search.page_start.unwrap_or(1);
    search_json[page_param] = serde_json::json!(page_start.saturating_add(page - 1));
    if !category_ids.is_empty() {
        let param = profile
            .search
            .category_param
            .as_deref()
            .unwrap_or("categories");
        search_json[param] = serde_json::Value::Array(category_ids);
    }
    search_json.to_string()
}

fn hdarea_v1_search_url(
    base: &str,
    path: &str,
    profile: &Profile,
    keyword: &str,
    page: u32,
    category_ids: &[serde_json::Value],
) -> String {
    let page_size = profile.search.page_size.unwrap_or(100);
    let page_param = profile.search.page_param.as_deref().unwrap_or("page");
    let page_start = profile.search.page_start.unwrap_or(0);
    let page_number = page_start.saturating_add(page.max(1) - 1);
    let mut url = format!(
        "{base}{path}?incldead=1&per_page={page_size}&{}={page_number}",
        url_encode(page_param),
    );
    let trimmed_kw = keyword.trim();
    if !trimmed_kw.is_empty() {
        url.push_str(&format!(
            "&{}={}",
            url_encode(&profile.search.query_param),
            url_encode(trimmed_kw),
        ));
    }
    if !category_ids.is_empty() {
        let cat_param = profile.search.category_param.as_deref().unwrap_or("cat");
        let joined = category_ids
            .iter()
            .map(|id| {
                let raw = id
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| id.to_string());
                url_encode(&raw)
            })
            .collect::<Vec<_>>()
            .join(",");
        url.push_str(&format!("&{}={joined}", url_encode(cat_param)));
    }
    url
}

pub fn rss_request(site: &Site, _profile: &Profile) -> FetchRequest {
    FetchRequest {
        key: format!("rss:{}", site.id),
        url: site.rss_url.clone().unwrap_or_else(|| site.url.clone()),
        method: FetchMethod::Get,
        body: None,
        cookie: site.cookie.clone(),
        api_key: site.api_key.clone(),
        proxy: site.proxy.clone(),
        cdp_url: site.cdp_url.clone(),
    }
}

pub fn download_token_request(site: &Site, profile: &Profile, id: &str) -> Option<FetchRequest> {
    let dl_cfg = profile.download.as_ref()?;
    let base = effective_base_url(site, profile);
    let path = if dl_cfg.path.starts_with('/') {
        dl_cfg.path.clone()
    } else {
        format!("/{}", dl_cfg.path)
    };
    let method_str = dl_cfg.method.as_deref().unwrap_or("post_form");
    let param = dl_cfg.id_param.as_deref().unwrap_or("id");

    let (method, body) = match method_str.to_lowercase().as_str() {
        "post_json" => (
            FetchMethod::PostJson,
            Some(serde_json::json!({ param: id }).to_string()),
        ),
        _ => (FetchMethod::PostForm, Some(format!("{param}={id}"))),
    };

    Some(FetchRequest {
        key: format!("download:{}:{id}", site.id),
        url: format!("{base}{path}"),
        method,
        body,
        cookie: site.cookie.clone(),
        api_key: site.api_key.clone(),
        proxy: site.proxy.clone(),
        cdp_url: None,
    })
}

pub fn mteam_download_request(site: &Site, id: &str) -> FetchRequest {
    FetchRequest {
        key: format!("download:{}:{id}", site.id),
        url: format!("{}/api/torrent/genDlToken", site.url.trim_end_matches('/')),
        method: FetchMethod::PostForm,
        body: Some(format!("id={id}")),
        cookie: None,
        api_key: site.api_key.clone(),
        proxy: site.proxy.clone(),
        cdp_url: None,
    }
}

fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            b' ' => encoded.push('+'),
            _ => {
                use std::fmt::Write;
                let _ = write!(encoded, "%{byte:02X}");
            }
        }
    }
    encoded
}

fn url_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            _ => {
                use std::fmt::Write;
                let _ = write!(encoded, "%{byte:02X}");
            }
        }
    }
    encoded
}
