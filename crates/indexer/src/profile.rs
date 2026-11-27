use std::path::Path;

use serde::Deserialize;

use crate::RegistryError;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Framework {
    Nexusphp,
    Api,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApiDialect {
    #[default]
    Mteam,
    HdareaV1,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SearchConfig {
    pub path: String,
    pub query_param: String,
    #[serde(default)]
    pub page_size: Option<u32>,
    /// Pagination parameter for NexusPHP query strings or API request bodies.
    #[serde(default)]
    pub page_param: Option<String>,
    /// Page number used for the first page (NexusPHP commonly uses zero).
    #[serde(default)]
    pub page_start: Option<u32>,
    #[serde(default)]
    pub category_param: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct DownloadConfig {
    pub path: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub id_param: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct CategoryMapping {
    #[serde(default)]
    pub movie: Vec<serde_json::Value>,
    #[serde(default)]
    pub tv: Vec<serde_json::Value>,
    #[serde(default)]
    pub anime: Vec<serde_json::Value>,
    #[serde(default)]
    pub documentary: Vec<serde_json::Value>,
    #[serde(default)]
    pub music: Vec<serde_json::Value>,
    #[serde(default)]
    pub game: Vec<serde_json::Value>,
    #[serde(default)]
    pub av: Vec<serde_json::Value>,
    #[serde(default)]
    pub other: Vec<serde_json::Value>,
}

impl CategoryMapping {
    pub(crate) fn ids(&self, requested: &[String]) -> Vec<serde_json::Value> {
        let mut ids = Vec::new();
        for name in requested {
            let values = match name.trim().to_ascii_lowercase().as_str() {
                "movie" => &self.movie,
                "tv" => &self.tv,
                "anime" => &self.anime,
                "documentary" => &self.documentary,
                "music" => &self.music,
                "game" => &self.game,
                "av" => &self.av,
                "other" => &self.other,
                _ => continue,
            };
            for id in values {
                if !ids.contains(id) {
                    ids.push(id.clone());
                }
            }
        }
        ids
    }

    pub fn canonical_for_value(&self, raw: &str) -> Option<&'static str> {
        let cat_id = extract_category_id(raw)?;
        let lists: [(&'static str, &[serde_json::Value]); 8] = [
            ("movie", &self.movie),
            ("tv", &self.tv),
            ("documentary", &self.documentary),
            ("anime", &self.anime),
            ("music", &self.music),
            ("game", &self.game),
            ("av", &self.av),
            ("other", &self.other),
        ];
        for (canonical, values) in lists {
            if values.iter().any(|v| match v {
                serde_json::Value::String(s) => s.trim() == cat_id,
                serde_json::Value::Number(n) => n.to_string() == cat_id,
                _ => false,
            }) {
                return Some(canonical);
            }
        }
        None
    }
}

fn extract_category_id(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return Some(trimmed);
    }
    if let Some((_, tail)) = trimmed.split_once("cat=") {
        let id = tail.split(['&', '#', '=']).next()?.trim();
        if !id.is_empty() {
            return Some(id);
        }
    }
    if let Some(tail) = trimmed.strip_prefix("cat")
        && !tail.is_empty()
        && tail.bytes().all(|b| b.is_ascii_digit())
    {
        return Some(tail);
    }
    None
}

/// Normalize a site-specific category value (`?cat=402`, `"402"`, `"剧集"`, `"TV Shows"`) into
/// the canonical category vocabulary when recognized, otherwise preserve the trimmed string.
pub fn normalize_category(raw: &str, mapping: Option<&CategoryMapping>) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(m) = mapping
        && let Some(canonical) = m.canonical_for_value(trimmed)
    {
        return Some(canonical.to_string());
    }
    let lower = trimmed.to_ascii_lowercase();
    let canonical = match lower.as_str() {
        "movie" | "movies" | "film" | "films" | "电影" | "影片" => Some("movie"),
        "tv" | "tv series" | "tv show" | "tv shows" | "series" | "剧集" | "电视剧" | "连续剧"
        | "美剧" | "日剧" | "韩剧" | "剧" | "综艺" => Some("tv"),
        "documentary" | "documentaries" | "doc" | "纪录" | "纪录片" => Some("documentary"),
        "anime" | "animation" | "cartoon" | "动漫" | "动画" => Some("anime"),
        "music" | "audio" | "lossless" | "音乐" | "无损音乐" | "演唱会" => Some("music"),
        "game" | "games" | "游戏" => Some("game"),
        "av" | "adult" | "xxx" | "成人" => Some("av"),
        "other" | "misc" | "其他" | "其它" => Some("other"),
        _ => None,
    };
    Some(canonical.unwrap_or(trimmed).to_string())
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct ListConfig {
    #[serde(default)]
    pub item: String,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct Field {
    pub selector: String,
    pub attr: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct Fields {
    pub title: Option<Field>,
    pub enclosure: Option<Field>,
    pub size: Option<Field>,
    pub seeders: Option<Field>,
    pub free: Option<Field>,
    pub hr: Option<Field>,
    pub leechers: Option<Field>,
    pub snatched: Option<Field>,
    pub upload_time: Option<Field>,
    /// 详情链接（通常复用 title 选择器的 href）。
    pub detail: Option<Field>,
    pub category: Option<Field>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RssConfig {
    pub item: String,
    pub title: Field,
    pub enclosure: Field,
    pub size: Option<Field>,
    pub seeders: Option<Field>,
    pub free: Option<Field>,
    pub hr: Option<Field>,
    pub id: Option<Field>,
    pub leechers: Option<Field>,
    pub snatched: Option<Field>,
    pub upload_time: Option<Field>,
    pub category: Option<Field>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Profile {
    pub id: String,
    pub framework: Framework,
    #[serde(default)]
    pub api_dialect: ApiDialect,
    #[serde(default)]
    pub auth_types: Vec<String>,
    #[serde(default)]
    pub api_check_in_path: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub web_base_url: Option<String>,
    #[serde(default)]
    pub render: bool,
    pub search: SearchConfig,
    #[serde(default)]
    pub download: Option<DownloadConfig>,
    #[serde(default)]
    pub categories: Option<CategoryMapping>,
    #[serde(default)]
    pub login_success_css: Option<String>,
    #[serde(default)]
    pub list: ListConfig,
    #[serde(default)]
    pub fields: Fields,
    pub rss: Option<RssConfig>,
}

impl Profile {
    pub fn effective_auth_types(&self) -> Vec<&str> {
        if !self.auth_types.is_empty() {
            self.auth_types.iter().map(String::as_str).collect()
        } else if self.framework == Framework::Api {
            vec!["apikey"]
        } else {
            vec!["cookie"]
        }
    }

    pub fn supports_check_in(&self) -> bool {
        self.framework == Framework::Nexusphp || self.api_check_in_path.is_some()
    }

    /// Resolve the base URL for HTTP requests against a configured `Site`.
    pub fn effective_site_base_url(&self, site_url: &str) -> String {
        let trimmed = site_url.trim().trim_end_matches('/');
        if is_loopback_url(trimmed) {
            return trimmed.to_string();
        }
        if let Some(base) = &self.base_url {
            let base_trimmed = base.trim().trim_end_matches('/');
            let web_trimmed = self
                .web_base_url
                .as_deref()
                .map(|u| u.trim().trim_end_matches('/'));
            if trimmed.is_empty() || web_trimmed != Some(base_trimmed) {
                return base_trimmed.to_string();
            }
        }
        trimmed.to_string()
    }
}

fn is_loopback_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    for prefix in [
        "http://127.0.0.1",
        "https://127.0.0.1",
        "http://localhost",
        "https://localhost",
        "http://[::1]",
        "https://[::1]",
    ] {
        if let Some(rest) = lower.strip_prefix(prefix)
            && (rest.is_empty() || rest.starts_with(':') || rest.starts_with('/'))
        {
            return true;
        }
    }
    false
}
const BUILTIN: &[(&str, &str)] = &[
    ("demo", include_str!("profiles/demo.yaml")),
    ("mteam", include_str!("profiles/mteam.yaml")),
    ("pterclub", include_str!("profiles/pterclub.yaml")),
    // 移植自上游项目 sites/configs（U7）：
    ("chdbits", include_str!("profiles/chdbits.yaml")),
    ("hdsky", include_str!("profiles/hdsky.yaml")),
    ("hddolby", include_str!("profiles/hddolby.yaml")),
    ("hdfans", include_str!("profiles/hdfans.yaml")),
    ("ourbits", include_str!("profiles/ourbits.yaml")),
    ("keepfrds", include_str!("profiles/keepfrds.yaml")),
    ("agsvpt", include_str!("profiles/agsvpt.yaml")),
    ("audiences", include_str!("profiles/audiences.yaml")),
    ("hdarea", include_str!("profiles/hdarea.yaml")),
    ("hdhome", include_str!("profiles/hdhome.yaml")),
    ("hdtime", include_str!("profiles/hdtime.yaml")),
    ("hdvideo", include_str!("profiles/hdvideo.yaml")),
    ("hhanclub", include_str!("profiles/hhanclub.yaml")),
    ("nicept", include_str!("profiles/nicept.yaml")),
    ("piggo", include_str!("profiles/piggo.yaml")),
    ("pthome", include_str!("profiles/pthome.yaml")),
    ("pttime", include_str!("profiles/pttime.yaml")),
    ("soulvoice", include_str!("profiles/soulvoice.yaml")),
    ("ssd", include_str!("profiles/ssd.yaml")),
    ("tjupt", include_str!("profiles/tjupt.yaml")),
    ("ttg", include_str!("profiles/ttg.yaml")),
];

pub(crate) fn parse_yaml_profile(yaml: &str) -> Result<Profile, RegistryError> {
    let profile: Profile = serde_yaml::from_str(yaml)?;
    if profile.render {
        tracing::warn!(
            profile = %profile.id,
            "render is ignored: search never uses the Browser"
        );
    }
    Ok(profile)
}

pub(crate) fn load_builtin_profiles() -> Result<Vec<Profile>, RegistryError> {
    let mut profiles = Vec::with_capacity(BUILTIN.len());
    for (_, yaml) in BUILTIN {
        profiles.push(parse_yaml_profile(yaml)?);
    }
    Ok(profiles)
}

pub(crate) fn load_overlay_profiles(dir: &Path) -> Result<Vec<(String, Profile)>, RegistryError> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let is_yaml = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext == "yaml" || ext == "yml");
        if is_yaml {
            paths.push(path);
        }
    }
    paths.sort();
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let yaml = std::fs::read_to_string(&path)?;
        let profile = parse_yaml_profile(&yaml)?;
        out.push((path.display().to_string(), profile));
    }
    Ok(out)
}
