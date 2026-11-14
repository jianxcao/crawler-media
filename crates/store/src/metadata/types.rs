use domain::MediaId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectiveMetadata {
    pub media_id: MediaId,
    pub locale: String,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<u16>,
    pub overview: Option<String>,
    pub release_date: Option<String>,
    pub rating: Option<f64>,
    pub runtime_minutes: Option<u32>,
    pub language: Option<String>,
    pub genres_json: String,
    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,
    pub details_json: String,
    pub field_origins_json: String,
    pub revision: i64,
}
