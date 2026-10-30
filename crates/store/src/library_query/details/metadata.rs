use crate::metadata::EffectiveMetadata;
use crate::metadata::locale::{LocaleFallback, select_locale};
use crate::{Store, StoreError};
use domain::{FieldValue, LibraryId, MediaId, MetadataFields};
use rusqlite::OptionalExtension;
use std::collections::HashMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdoptedFilterFacts {
    pub genres: Vec<String>,
    pub studios: Vec<String>,
    pub countries: Vec<String>,
    pub overview: Option<String>,
    pub people: Vec<AdoptedPerson>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdoptedPerson {
    pub name: String,
    pub role: Option<String>,
    pub tmdb_id: Option<String>,
}

pub fn instance_default_locale(store: &Store) -> Result<Option<String>, StoreError> {
    store.with_read(crate::Db::Library, instance_default_locale_on)
}
pub(crate) fn instance_default_locale_on(
    conn: &rusqlite::Connection,
) -> Result<Option<String>, StoreError> {
    crate::metadata_language::default_locale(conn)
}
pub fn library_display_locale(
    store: &Store,
    library_id: LibraryId,
) -> Result<Option<String>, StoreError> {
    store.with_read(crate::Db::Library, |conn| {
        library_display_locale_on(conn, library_id)
    })
}
pub(crate) fn library_display_locale_on(
    conn: &rusqlite::Connection,
    library_id: LibraryId,
) -> Result<Option<String>, StoreError> {
    let display: Option<String> = conn
        .query_row(
            "SELECT display_locale FROM libraries WHERE library_id = ?1",
            [library_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    match display.filter(|locale| !locale.trim().is_empty()) {
        Some(locale) => Ok(Some(locale)),
        None => instance_default_locale_on(conn),
    }
}

impl Store {
    pub fn adopted_filter_facts_for_media(
        &self,
        media_ids: &[MediaId],
        preferred_locale: Option<&str>,
    ) -> Result<HashMap<MediaId, AdoptedFilterFacts>, StoreError> {
        self.with_library_read(|db| db.adopted_filter_facts_for_media(media_ids, preferred_locale))
    }

    pub fn adopted_filter_facts(
        &self,
        media_ids: &[MediaId],
        preferred_locale: &str,
    ) -> Result<HashMap<MediaId, AdoptedFilterFacts>, StoreError> {
        self.with_library_read(|db| db.adopted_filter_facts(media_ids, preferred_locale))
    }
}

fn pick_locale_row<'a>(
    rows: &'a [(String, String, String, String)],
    preferred_locale: Option<&str>,
) -> Option<&'a (String, String, String, String)> {
    if rows.is_empty() {
        return None;
    }
    if let Some(preferred) = preferred_locale.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(exact) = rows.iter().find(|(loc, ..)| loc == preferred) {
            return Some(exact);
        }
        let prefix = preferred.split(['-', '_']).next().unwrap_or(preferred);
        if let Some(prefix_match) = rows
            .iter()
            .find(|(loc, ..)| loc.split(['-', '_']).next().unwrap_or(loc) == prefix)
        {
            return Some(prefix_match);
        }
    }
    rows.first()
}

/// Merge the persisted composed effective row into the NFO/base input fields.
///
/// Every composed field (not a short whitelist) fills a missing input field; a field whose
/// effective origin is a manual override wins over the input as well, so details, cards and
/// protocol hydration show the same value the composer persisted.
pub(crate) fn fill_from_adopted(base: &mut MetadataFields, adopted: &EffectiveMetadata) {
    fill_from_adopted_parts(
        base,
        &adopted.title,
        &adopted.details_json,
        &adopted.field_origins_json,
    );
}

pub(crate) fn fill_from_adopted_parts(
    base: &mut MetadataFields,
    title: &str,
    details_json: &str,
    origins_json: &str,
) {
    fill(
        &mut base.title,
        Some(title.to_string()).filter(|t| !t.is_empty()),
    );
    let composed = serde_json::from_str::<serde_json::Value>(details_json)
        .ok()
        .and_then(|value| value.get("fields").cloned())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let manual: Vec<String> = serde_json::from_str::<serde_json::Value>(origins_json)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .map(|origins| {
            origins
                .into_iter()
                .filter(|(_, origin)| {
                    origin.get("source").and_then(|s| s.as_str()) == Some("manual")
                })
                .map(|(field, _)| field)
                .collect()
        })
        .unwrap_or_default();
    let Ok(serde_json::Value::Object(mut merged)) = serde_json::to_value(&*base) else {
        return;
    };
    for (field, value) in composed {
        let Some(current) = merged.get(&field) else {
            continue; // display-only keys (e.g. studio_logo) are not MetadataFields
        };
        let missing = current.get("state").and_then(|s| s.as_str()) == Some("missing");
        let is_missing_value = value.get("state").and_then(|s| s.as_str()) == Some("missing");
        if is_missing_value {
            continue;
        }
        if missing || manual.contains(&field) {
            merged.insert(field, value);
        }
    }
    if let Ok(fields) = serde_json::from_value(serde_json::Value::Object(merged)) {
        *base = fields;
    }
}

fn fill<T>(field: &mut FieldValue<T>, value: Option<T>) {
    if field.is_missing()
        && let Some(value) = value
    {
        *field = FieldValue::Value(value);
    }
}

fn adopted_facts_from_json(
    genres_json: &str,
    countries_json: &str,
    details_json: &str,
    media_id: MediaId,
) -> Option<AdoptedFilterFacts> {
    let genres = parse_adopted_json(genres_json, media_id, "genres_json")?;
    let countries = parse_adopted_json(countries_json, media_id, "countries_json")?;
    let details = parse_adopted_json(details_json, media_id, "details_json")?;
    let fields = details.get("fields");
    Some(AdoptedFilterFacts {
        genres: json_names(&genres),
        studios: fields
            .and_then(|fields| fields.get("studios"))
            .map(json_names)
            .unwrap_or_default(),
        countries: json_names(&countries),
        overview: fields
            .and_then(|fields| fields.get("overview"))
            .and_then(|overview| overview.get("value"))
            .and_then(|value| value.as_str())
            .map(str::to_string),
        people: fields
            .and_then(|fields| fields.get("cast"))
            .and_then(|cast| cast.get("value"))
            .and_then(|cast| cast.as_array())
            .map(|cast| {
                cast.iter()
                    .filter_map(|member| {
                        Some(AdoptedPerson {
                            name: member.get("name")?.as_str()?.to_string(),
                            role: member
                                .get("role")
                                .and_then(|role| role.as_str())
                                .map(str::to_string),
                            tmdb_id: member
                                .get("tmdb_id")
                                .and_then(|id| id.as_str())
                                .map(str::to_string),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn parse_adopted_json(raw: &str, media_id: MediaId, field: &str) -> Option<serde_json::Value> {
    match serde_json::from_str(raw) {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::warn!(
                %error,
                %media_id,
                field,
                "skipping malformed adopted metadata while loading filter facts"
            );
            None
        }
    }
}

fn json_names(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::String(text) => vec![text.clone()],
        serde_json::Value::Array(items) => items.iter().flat_map(json_names).collect(),
        serde_json::Value::Object(map) => ["name", "value"]
            .iter()
            .find_map(|key| map.get(*key))
            .map(json_names)
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

#[path = "metadata/connection.rs"]
mod connection;
