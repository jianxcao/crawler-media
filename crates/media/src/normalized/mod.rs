use serde_json::Value;
use std::collections::BTreeMap;

use domain::{EntityScope, MetadataFields, MetadataPreferences, NormalizedMetadata};

use crate::client::TmdbError;

pub mod generic;
pub mod selection;
pub mod tmdb;

pub fn normalize_metadata(
    source: &str,
    scope: EntityScope,
    body: &str,
    preferences: &MetadataPreferences,
) -> Result<NormalizedMetadata, TmdbError> {
    let json: Value = serde_json::from_str(body)?;
    let mut fields = MetadataFields::default();
    let mut origins = BTreeMap::new();

    if source == "tmdb" {
        tmdb::normalize_tmdb(&json, scope, preferences, &mut fields, &mut origins)?;
    } else {
        generic::normalize_generic(&json, &mut fields, &mut origins);
    }

    Ok(NormalizedMetadata {
        fields,
        field_origins: origins,
    })
}
