use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::IndexerError;

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
            let values = match name.as_str() {
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

#[derive(Clone, Default)]
pub struct ProfileSet {
    profiles: HashMap<String, Profile>,
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

impl ProfileSet {
    /// 全部 profile id（含 overlay，排序稳定）。
    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.profiles.keys().cloned().collect();
        ids.sort();
        ids
    }

    pub fn load(overlay: Option<&Path>) -> Result<Self, IndexerError> {
        let mut profiles = HashMap::new();
        for (_, yaml) in BUILTIN {
            let profile: Profile = serde_yaml::from_str(yaml)?;
            if profile.render {
                tracing::warn!(profile = %profile.id, "render is ignored: search never uses the Browser");
            }
            profiles.insert(profile.id.clone(), profile);
        }
        if let Some(dir) = overlay {
            for entry in std::fs::read_dir(dir)? {
                let path = entry?.path();
                let is_yaml = path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext == "yaml" || ext == "yml");
                if !is_yaml {
                    continue;
                }
                let profile: Profile = serde_yaml::from_str(&std::fs::read_to_string(path)?)?;
                if profile.render {
                    tracing::warn!(profile = %profile.id, "render is ignored: search never uses the Browser");
                }
                profiles.insert(profile.id.clone(), profile);
            }
        }
        Ok(Self { profiles })
    }

    pub fn get(&self, id: &str) -> Option<&Profile> {
        self.profiles.get(id)
    }
}
