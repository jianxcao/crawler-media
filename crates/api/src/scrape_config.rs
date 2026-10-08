//! Scrape & organize configuration, aligned with the upstream
//! scrape-customization design (metadata.scrape namespace). Stored as JSON in
//! the settings KV; **empty fields mean "follow default"** — effective values
//! merge at read time, so the defaults always equal the pre-config behavior
//! (zero-migration promise).

use serde::{Deserialize, Serialize};

pub const KEY: &str = crate::settings_keys::METADATA_SCRAPE;

/// Stored user settings; every field defaults to "unset = follow default".
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScrapeConfigSetting {
    #[serde(default)]
    pub language_priority: Vec<String>,
    #[serde(default)]
    pub cert_country_priority: Vec<String>,
    /// "default" (TMDB default poster) | "language" (pick by language priority)
    #[serde(default)]
    pub poster_mode: String,
    #[serde(default)]
    pub poster_language_priority: Vec<String>,
    #[serde(default)]
    pub backdrop_language_priority: Vec<String>,
    #[serde(default)]
    pub poster_min_width: Option<u32>,
    #[serde(default)]
    pub backdrop_min_width: Option<u32>,
    #[serde(default)]
    pub poster_size: String,
    #[serde(default)]
    pub backdrop_size: String,
    #[serde(default)]
    pub still_size: String,
    #[serde(default)]
    pub naming_entry_dir: String,
    #[serde(default)]
    pub naming_movie_file: String,
    #[serde(default)]
    pub naming_season_dir: String,
    #[serde(default)]
    pub naming_episode_file: String,
    #[serde(default)]
    pub mirror_images: Option<bool>,
    #[serde(default)]
    pub mirror_nfo: Option<bool>,
    #[serde(default)]
    pub mirror_episode_thumbs: Option<bool>,
    #[serde(default)]
    pub theintrodb_enabled: Option<bool>,
    #[serde(default)]
    pub theintrodb_api_key: Option<String>,
    /// 声纹识别单集采样时长（秒）：从每集开头取多长音频做 Chromaprint 指纹。
    /// 空 = 跟随默认 180（3 分钟）。STRM 挂载越小越省流量。
    #[serde(default)]
    pub fingerprint_duration_secs: Option<u32>,
    /// 声纹采样模式："full_window" | "adaptive"。空 = 跟随默认 "full_window"。
    #[serde(default)]
    pub fingerprint_sampling_mode: Option<String>,
}

/// Effective configuration after merging defaults (what consumers read).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveScrapeConfig {
    /// Any naming template field was explicitly set — only then do the
    /// segment-composition rules apply; otherwise built-in defaults win.
    pub naming_customized: bool,
    /// Per-segment "explicitly set" flags (the effective values are defaulted,
    /// so composition must know which came from the user).
    pub naming_entry_set: bool,
    pub naming_season_set: bool,
    pub language_priority: Vec<String>,
    pub cert_country_priority: Vec<String>,
    pub poster_mode: String,
    pub poster_language_priority: Vec<String>,
    pub backdrop_language_priority: Vec<String>,
    pub poster_min_width: u32,
    pub backdrop_min_width: u32,
    pub poster_size: String,
    pub backdrop_size: String,
    pub still_size: String,
    pub naming_entry_dir: String,
    pub naming_movie_file: String,
    pub naming_season_dir: String,
    pub naming_episode_file: String,
    pub mirror_images: bool,
    pub mirror_nfo: bool,
    pub mirror_episode_thumbs: bool,
    pub theintrodb_enabled: bool,
    pub theintrodb_api_key: Option<String>,
    pub fingerprint_duration_secs: u32,
    pub fingerprint_sampling_mode: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScrapeConfig {
    pub setting: ScrapeConfigSetting,
    pub effective: EffectiveScrapeConfig,
}

// Defaults = current hardcoded behavior (默认值 = 现状).
pub fn default_language_priority() -> Vec<String> {
    vec!["zh-CN".into(), "en-US".into()]
}
pub fn default_cert_country_priority() -> Vec<String> {
    vec!["CN".into(), "US".into()]
}
pub fn default_poster_language_priority() -> Vec<String> {
    vec!["meta".into(), "en".into(), String::new()]
}
pub fn default_backdrop_language_priority() -> Vec<String> {
    vec![String::new(), "meta".into(), "en".into()]
}
pub fn default_naming_entry_dir() -> &'static str {
    "{title} ({year})"
}
pub fn default_naming_movie_file() -> &'static str {
    "{title} ({year}){part} - {resolution}{ext}"
}
pub fn default_naming_season_dir() -> &'static str {
    "Season {season}"
}
pub fn default_naming_episode_file() -> &'static str {
    "{title} - {season_episode}{part}{ext}"
}

impl EffectiveScrapeConfig {
    /// Primary metadata language (first priority; en-US fallback).
    pub fn primary_language(&self) -> &str {
        self.language_priority
            .first()
            .map(String::as_str)
            .unwrap_or("zh-CN")
    }

    /// Whether artwork assets are written beside media files at all.
    pub fn write_media_assets(&self) -> bool {
        self.mirror_images
    }

    /// Compose the full movie pattern. With no customized naming, the built-in
    /// default applies byte-for-byte; once any field is set, empty entry dir
    /// means the file pattern is the whole path (legacy single-segment
    /// templates round-trip exactly).
    pub fn compose_movie_pattern(&self) -> String {
        if !self.naming_customized {
            return format!(
                "{}/{}",
                default_naming_entry_dir(),
                default_naming_movie_file(),
            );
        }
        let file = non_empty(&self.naming_movie_file, default_naming_movie_file());
        if !self.naming_entry_set {
            file.to_string()
        } else {
            format!("{}/{}", self.naming_entry_dir.trim(), file)
        }
    }

    /// Compose the full TV pattern with the same rules.
    pub fn compose_tv_pattern(&self) -> String {
        if !self.naming_customized {
            return format!(
                "{}/{}/{}",
                default_naming_entry_dir(),
                default_naming_season_dir(),
                default_naming_episode_file(),
            );
        }
        let season = non_empty(&self.naming_season_dir, default_naming_season_dir());
        let file = non_empty(&self.naming_episode_file, default_naming_episode_file());
        if !self.naming_entry_set {
            if !self.naming_season_set {
                file.to_string()
            } else {
                format!("{season}/{file}")
            }
        } else {
            format!("{}/{season}/{file}", self.naming_entry_dir.trim())
        }
    }
}

pub fn non_empty<'a>(value: &'a str, fallback: &'static str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
}

/// Merge stored settings with defaults (empty = default).
pub fn effective_of(setting: &ScrapeConfigSetting) -> EffectiveScrapeConfig {
    let language_priority = if setting.language_priority.is_empty() {
        default_language_priority()
    } else {
        setting.language_priority.clone()
    };
    let cert_country_priority = if setting.cert_country_priority.is_empty() {
        default_cert_country_priority()
    } else {
        setting.cert_country_priority.clone()
    };
    let poster_mode = if setting.poster_mode.is_empty() {
        "default".to_string()
    } else {
        setting.poster_mode.clone()
    };
    EffectiveScrapeConfig {
        naming_customized: !setting.naming_entry_dir.is_empty()
            || !setting.naming_movie_file.is_empty()
            || !setting.naming_season_dir.is_empty()
            || !setting.naming_episode_file.is_empty(),
        naming_entry_set: !setting.naming_entry_dir.is_empty(),
        naming_season_set: !setting.naming_season_dir.is_empty(),
        language_priority,
        cert_country_priority,
        poster_mode,
        poster_language_priority: if setting.poster_language_priority.is_empty() {
            default_poster_language_priority()
        } else {
            setting.poster_language_priority.clone()
        },
        backdrop_language_priority: if setting.backdrop_language_priority.is_empty() {
            default_backdrop_language_priority()
        } else {
            setting.backdrop_language_priority.clone()
        },
        poster_min_width: setting.poster_min_width.unwrap_or(500),
        backdrop_min_width: setting.backdrop_min_width.unwrap_or(1920),
        poster_size: non_empty(&setting.poster_size, "w780").to_string(),
        backdrop_size: non_empty(&setting.backdrop_size, "original").to_string(),
        still_size: non_empty(&setting.still_size, "w300").to_string(),
        naming_entry_dir: non_empty(&setting.naming_entry_dir, default_naming_entry_dir())
            .to_string(),
        naming_movie_file: non_empty(&setting.naming_movie_file, default_naming_movie_file())
            .to_string(),
        naming_season_dir: non_empty(&setting.naming_season_dir, default_naming_season_dir())
            .to_string(),
        naming_episode_file: non_empty(&setting.naming_episode_file, default_naming_episode_file())
            .to_string(),
        mirror_images: setting.mirror_images.unwrap_or(true),
        mirror_nfo: setting.mirror_nfo.unwrap_or(true),
        mirror_episode_thumbs: setting.mirror_episode_thumbs.unwrap_or(true),
        theintrodb_enabled: setting.theintrodb_enabled.unwrap_or(true),
        theintrodb_api_key: setting.theintrodb_api_key.clone(),
        fingerprint_duration_secs: setting
            .fingerprint_duration_secs
            .filter(|v| *v > 0)
            .unwrap_or(180),
        fingerprint_sampling_mode: setting
            .fingerprint_sampling_mode
            .as_deref()
            .map(|s| s.trim())
            .filter(|s| *s == "adaptive" || *s == "full_window")
            .unwrap_or("full_window")
            .to_string(),
    }
}

pub fn compose_config(setting: ScrapeConfigSetting) -> ScrapeConfig {
    let effective = effective_of(&setting);
    ScrapeConfig { setting, effective }
}
