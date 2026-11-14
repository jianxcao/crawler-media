//! Current Library creation contract; retired fields are rejected before any write.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateLibrary {
    pub name: String,
    pub kind: String,
    pub root_paths: Vec<String>,
    #[serde(default)]
    pub detect_intros: bool,
    #[serde(default)]
    pub enable_fingerprint: bool,
    pub access_mode: Option<String>,
    pub admin_visible: Option<bool>,
    pub member_ids: Option<Vec<domain::UserId>>,
    pub match_rules: Option<Vec<Value>>,
    pub default_filter_id: Option<String>,
    pub realtime_watch: Option<bool>,
    pub generate_thumbnails: Option<bool>,
    pub extract_chapter_images: Option<bool>,
    pub exclude_from_home: Option<bool>,
    pub auto_series_collections: Option<bool>,
}
