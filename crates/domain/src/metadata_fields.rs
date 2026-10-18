use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};

use crate::MediaKind;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum FieldValue<T> {
    Missing,
    Empty,
    Value(T),
}

impl<T> Default for FieldValue<T> {
    fn default() -> Self {
        Self::Missing
    }
}

impl<T> FieldValue<T> {
    pub fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    pub fn is_value(&self) -> bool {
        matches!(self, Self::Value(_))
    }

    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Value(v) => Some(v),
            _ => None,
        }
    }

    pub fn into_value(self) -> Option<T> {
        match self {
            Self::Value(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_ref(&self) -> FieldValue<&T> {
        match self {
            Self::Missing => FieldValue::Missing,
            Self::Empty => FieldValue::Empty,
            Self::Value(v) => FieldValue::Value(v),
        }
    }

    pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> FieldValue<U> {
        match self {
            Self::Missing => FieldValue::Missing,
            Self::Empty => FieldValue::Empty,
            Self::Value(v) => FieldValue::Value(f(v)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataGenre {
    pub source: String,
    pub source_id: Option<String>,
    pub name: String,
    pub filter_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataCast {
    pub name: String,
    pub role: Option<String>,
    pub tmdb_id: Option<String>,
    pub thumb: Option<String>,
    pub order: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MetadataFields {
    pub title: FieldValue<String>,
    pub original_title: FieldValue<String>,
    pub overview: FieldValue<String>,
    pub tagline: FieldValue<String>,
    pub release_date: FieldValue<String>,
    pub end_date: FieldValue<String>,
    pub content_rating: FieldValue<String>,
    pub language: FieldValue<String>,
    pub status: FieldValue<String>,
    pub air_date: FieldValue<String>,
    pub poster_url: FieldValue<String>,
    pub backdrop_url: FieldValue<String>,
    pub still_url: FieldValue<String>,
    pub year: FieldValue<u16>,
    pub rating: FieldValue<f64>,
    pub vote_count: FieldValue<u64>,
    pub runtime_minutes: FieldValue<u32>,
    pub number_of_seasons: FieldValue<u32>,
    pub number_of_episodes: FieldValue<u32>,
    pub countries: FieldValue<Vec<String>>,
    pub studios: FieldValue<Vec<String>>,
    pub directors: FieldValue<Vec<String>>,
    pub creators: FieldValue<Vec<String>>,
    pub genres: FieldValue<Vec<MetadataGenre>>,
    pub episode_run_time: FieldValue<Vec<u32>>,
    pub cast: FieldValue<Vec<MetadataCast>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataPreferences {
    pub language_priority: Vec<String>,
    pub cert_country_priority: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceSelection {
    Direct,
    Translation,
    Certification,
    Empty,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFieldOrigin {
    pub selection: SourceSelection,
    pub response_pointer: String,
    pub actual_locale: Option<String>,
    pub certification_country: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NormalizedMetadata {
    pub fields: MetadataFields,
    pub field_origins: BTreeMap<String, SourceFieldOrigin>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MediaAlias {
    pub source: String,
    pub kind: MediaKind,
    pub external_id: String,
}
