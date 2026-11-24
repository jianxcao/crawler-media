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
        && profile.effective_auth_types().contains(&"cookie")
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
    let auth_types = profile.effective_auth_types();
    let supports_cookie = auth_types.contains(&"cookie");
    let cookie = if !supports_cookie || (profile.framework == Framework::Api && !html_fallback) {
        None
    } else {
        site.cookie.clone()
    };
    let supports_api_key =
        profile.framework != Framework::Nexusphp && auth_types.contains(&"apikey");
    let api_key = if html_fallback || !supports_api_key {
        None
    } else {
        site.api_key.clone()
    };
    FetchRequest {
        key: format!("search:{}", site.id),
        url,
        method,
        body,
        cookie,
        api_key,
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

pub fn rss_request(site: &Site, profile: &Profile) -> FetchRequest {
    let auth_types = profile.effective_auth_types();
    let supports_cookie = auth_types.contains(&"cookie");
    let supports_api_key =
        profile.framework != Framework::Nexusphp && auth_types.contains(&"apikey");
    FetchRequest {
        key: format!("rss:{}", site.id),
        url: site.rss_url.clone().unwrap_or_else(|| site.url.clone()),
        method: FetchMethod::Get,
        body: None,
        cookie: if supports_cookie {
            site.cookie.clone()
        } else {
            None
        },
        api_key: if supports_api_key {
            site.api_key.clone()
        } else {
            None
        },
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

    let html_fallback = uses_html_cookie_fallback(site, profile);
    let cookie = if profile.framework == Framework::Api && !html_fallback {
        None
    } else {
        site.cookie.clone()
    };
    let (method, body) = match method_str.to_lowercase().as_str() {
        "post_json" => (
            FetchMethod::PostJson,
            Some(serde_json::json!({ param: id }).to_string()),
        ),
        _ => (
            FetchMethod::PostForm,
            Some(format!("{}={}", url_encode(param), url_encode(id))),
        ),
    };

    Some(FetchRequest {
        key: format!("download:{}:{id}", site.id),
        url: format!("{base}{path}"),
        method,
        body,
        cookie,
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
        body: Some(format!("id={}", url_encode(id))),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::ProfileSet;
    use domain::SiteId;

    #[test]
    fn post_form_download_token_request_url_encodes_param_and_id() {
        let profiles = ProfileSet::load(None).unwrap();
        let mut profile = profiles.get("hdarea").unwrap().clone();
        if let Some(dl) = profile.download.as_mut() {
            dl.method = Some("post_form".into());
            dl.id_param = Some("torrent id".into());
        }
        let site = Site {
            id: SiteId::new(),
            name: "HDArea".into(),
            url: "https://hdarea.club".into(),
            profile_id: "hdarea".into(),
            cookie: Some("stale=1".into()),
            api_key: Some("valid".into()),
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: None,
            cdp_url: None,
            downloader_id: None,
            enabled: true,
            maintenance_via: domain::MaintenanceVia::Http,
        };
        let req = download_token_request(&site, &profile, "469752&inject=1").unwrap();
        assert_eq!(req.method, FetchMethod::PostForm);
        assert_eq!(req.cookie, None);
        assert_eq!(
            req.body.as_deref(),
            Some("torrent%20id=469752%26inject%3D1")
        );
    }

    #[test]
    fn apikey_only_profile_disables_html_cookie_fallback_and_rss_cookie() {
        let profiles = ProfileSet::load(None).unwrap();
        let mut apikey_only = profiles.get("hdarea").unwrap().clone();
        apikey_only.auth_types = vec!["apikey".into()];

        let site = Site {
            id: SiteId::new(),
            name: "ApiOnly".into(),
            url: "https://hdarea.club".into(),
            profile_id: "hdarea".into(),
            cookie: Some("c_secure_uid=stale".into()),
            api_key: None,
            rss_url: Some("https://hdarea.club/torrentrss.php".into()),
            proxy: None,
            rate_limit_per_minute: None,
            cdp_url: None,
            downloader_id: None,
            enabled: true,
            maintenance_via: domain::MaintenanceVia::Http,
        };

        assert!(!uses_html_cookie_fallback(&site, &apikey_only));
        let search_req = search_request_page_categories(&site, &apikey_only, "dune", 1, &[]);
        assert!(search_req.url.contains("/api/v1/torrents"));
        assert_eq!(search_req.cookie, None);

        let rss_req = rss_request(&site, &apikey_only);
        assert_eq!(rss_req.cookie, None);

        let hybrid = profiles.get("hdarea").unwrap();
        assert!(uses_html_cookie_fallback(&site, hybrid));
        assert_eq!(
            rss_request(&site, hybrid).cookie.as_deref(),
            Some("c_secure_uid=stale")
        );
    }
}
