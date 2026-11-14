//! Current Library PATCH contract, validated before any settings are written.
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub(crate) struct PatchLibrary {
    name: Option<String>,
    root_paths: Option<Vec<String>>,
    display_locale: Option<String>,
    detect_intros: Option<bool>,
    enable_fingerprint: Option<bool>,
    access_mode: Option<String>,
    admin_visible: Option<bool>,
    member_ids: Option<Vec<domain::UserId>>,
    match_rules: Option<Vec<Value>>,
    default_filter_id: Option<String>,
    realtime_watch: Option<bool>,
    generate_thumbnails: Option<bool>,
    extract_chapter_images: Option<bool>,
    exclude_from_home: Option<bool>,
    auto_series_collections: Option<bool>,
}
